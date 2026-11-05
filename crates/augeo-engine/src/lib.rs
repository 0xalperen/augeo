//! Plans, applies, verifies and reverts tweaks, and recovers from interruptions.
//!
//! The rules this crate exists to enforce:
//!
//! 1. Live measurement decides state. The journal only says what augeo expects.
//! 2. Nothing is written before its old value and the intent to write are durably on disk.
//! 3. The plan shown is the plan executed: the system is measured again and the hashes must match.
//! 4. A dry run is just a plan. Planning can't write, and writing needs a plan and a confirmation.
//! 5. An interrupted change is never rolled forward, because the tweak may have caused the crash.

#![forbid(unsafe_code)]

pub mod applicability;
pub mod disclaimer;
mod expand;

use std::collections::{BTreeMap, BTreeSet};

use applicability::Environment;
use augeo_catalog::{
    Catalog,
    schema::{Computed, Desired, Effect, LiveState, Resource, Status, Tweak},
};
use augeo_command::{Component, is_valid_exe};
use augeo_journal::{
    Body, Decision, Journal, JournalError, Ledger, Op, OpenReport, OpenTxn, Outcome, Params, Stamp,
};
use augeo_ports::{Address, Clock, GpuScheduling, PortError, Store, System, Value};
use expand::{Target, expand};
use sha2::{Digest, Sha256};

/// The word a level 3 tweak needs typed.
pub const TYPED_HIGH: &str = "apply";
/// The phrase a level 4 tweak needs typed.
pub const TYPED_EXPERT: &str = "I accept the risk";

/// Identifies the disclaimer text the user must accept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disclaimer {
    pub version: u32,
    pub text_sha256: String,
}

/// Facts about this machine right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    pub env: Environment,
    pub stamp: Stamp,
    /// The interactive user, so `user` scope reaches their hive even when elevated as someone else.
    pub user_sid: String,
    /// The active power plan at startup. Only a fallback, as the plan can change at any time, so
    /// per-plan tweaks read the live one.
    pub power_scheme: String,
    /// What Windows reports for this boot, if it answers. It only changes with a restart.
    pub gpu_scheduling: Option<GpuScheduling>,
    pub adapters: Vec<String>,
    pub display_device: Option<String>,
    /// The display's best mode, when it differs from the current one.
    pub max_display_mode: Option<String>,
    /// A frame cap worked out from the refresh rate.
    pub frame_cap: Option<u32>,
    pub disclaimer: Disclaimer,
}

/// A request to apply one tweak.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TweakRequest {
    pub id: String,
    pub option: Option<String>,
    pub exe: Option<String>,
}

impl TweakRequest {
    pub fn new(id: &str) -> Self {
        Self {
            id: id.to_string(),
            option: None,
            exe: None,
        }
    }
}

/// Whether a tweak changes values inside a power plan, which only matter while that plan is active.
fn sets_plan_values(resources: &[Resource]) -> bool {
    resources.iter().any(|r| {
        matches!(
            r,
            Resource::PowerIndexAc { .. } | Resource::PowerIndexDc { .. }
        )
    })
}

fn same_plan(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        _ => a == b,
    }
}

/// A power plan's name where its ID is one Windows always uses. Copies have IDs of their own.
fn plan_label(scheme: &str) -> String {
    match scheme.to_lowercase().as_str() {
        "381b4222-f694-41f0-9685-ff5bb260df2e" => "the Balanced plan".to_string(),
        "8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c" => "the High Performance plan".to_string(),
        "a1841308-3541-4fab-bc81-f71556f20b4a" => "the Power Saver plan".to_string(),
        "e9a42b02-d5df-448d-aa00-03f14749eb61" => "the Ultimate Performance plan".to_string(),
        other => format!("power plan {other}"),
    }
}

fn instance_key(id: &str, exe: Option<&str>) -> String {
    match exe {
        Some(exe) => format!("{id}@{}", exe.to_lowercase()),
        None => id.to_string(),
    }
}

/// What to do when a value was changed by something else after augeo applied a tweak.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DriftPolicy {
    /// Leave the newer value alone and stop tracking it. This is the default.
    #[default]
    Keep,
    /// Overwrite it with the value from before augeo's change.
    RestoreBaseline,
}

/// Which baseline to restore when a tweak was re-applied after an external change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EpochChoice {
    #[default]
    Newest,
    Oldest,
}

/// How an undo should behave.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UndoPolicy {
    pub drift: DriftPolicy,
    pub epoch: EpochChoice,
}

/// One resource in a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedStep {
    pub address: Address,
    pub current: Value,
    pub target: Value,
    pub write: bool,
    /// The value to record as the baseline. `None` for resources this transaction adds no epoch to.
    pub baseline: Option<Value>,
    /// Changed by something else since augeo applied the tweak.
    pub drift: bool,
    /// The old value is gone because the journal was damaged, so there is nothing to restore.
    pub baseline_lost: bool,
}

/// The plan for one tweak instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TweakPlan {
    pub instance: String,
    pub tweak_id: String,
    pub title: String,
    pub rev: u32,
    pub danger: u8,
    pub effect: Effect,
    pub security_lowering: bool,
    pub params: Params,
    pub steps: Vec<PlannedStep>,
}

impl TweakPlan {
    /// Whether this tweak would change anything.
    pub fn writes(&self) -> bool {
        self.steps.iter().any(|s| s.write)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PlanSource {
    Apply(Vec<TweakRequest>),
    Undo(Vec<String>, UndoPolicy),
}

/// The exact changes that would be made. A dry run stops here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub op: Op,
    pub tweaks: Vec<TweakPlan>,
    pub hash: String,
    source: PlanSource,
}

/// Why a plan or execution was refused or failed.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineError {
    UnknownTweak(String),
    Retired {
        id: String,
        reason: String,
    },
    NotApplicable {
        id: String,
        reason: String,
    },
    NotAvailable {
        id: String,
        reason: String,
    },
    /// Another applied tweak owns this resource.
    ResourceOwned {
        id: String,
        owner: String,
        resource: String,
    },
    Conflict {
        id: String,
        other: String,
    },
    Requires {
        id: String,
        other: String,
    },
    /// Applied by an older definition: it has to be undone before re-applying.
    UndoFirst {
        id: String,
    },
    /// Applied to a power plan other than the one it would change now.
    AppliedToOtherPlan {
        id: String,
    },
    Invalid {
        id: String,
        reason: String,
    },
    /// A value couldn't be read, so its state is unknown and nothing should be planned from it.
    Unreadable {
        id: String,
        reason: String,
    },
    /// An interrupted change must be resolved before anything else.
    RecoveryRequired,
    ReadOnly,
    DisclaimerNotAccepted,
    ConfirmationMissing {
        id: String,
        needed: String,
    },
    /// A level 4 tweak must be applied on its own.
    MustApplyAlone {
        id: String,
    },
    /// The system changed since the plan was shown. The new plan must be shown and confirmed instead.
    PlanChanged(Box<Plan>),
    Journal(JournalError),
    UnknownTransaction(u64),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use EngineError::*;
        match self {
            UnknownTweak(id) => write!(f, "no tweak called {id}"),
            Retired { id, reason } => write!(f, "{id} was retired: {reason}"),
            NotApplicable { id, reason } => write!(f, "{id} doesn't apply here: {reason}"),
            NotAvailable { id, reason } => write!(f, "{id} isn't available here: {reason}"),
            ResourceOwned {
                id,
                owner,
                resource,
            } => {
                write!(
                    f,
                    "{id} needs {resource}, which {owner} already changed. Undo {owner} first"
                )
            }
            Conflict { id, other } => write!(f, "{id} conflicts with {other}. Undo {other} first"),
            Requires { id, other } => write!(f, "{id} needs {other} to be applied too"),
            AppliedToOtherPlan { id } => {
                write!(
                    f,
                    "{id} is applied to a different power plan. Undo it first, then apply it again"
                )
            }
            UndoFirst { id } => {
                write!(
                    f,
                    "{id} was applied by an older version of its definition. Undo it first"
                )
            }
            Invalid { id, reason } => write!(f, "{id}: {reason}"),
            Unreadable { id, reason } => write!(f, "couldn't read the state of {id}: {reason}"),
            RecoveryRequired => f.write_str("an interrupted change has to be resolved first"),
            ReadOnly => f.write_str("this instance is read-only"),
            DisclaimerNotAccepted => f.write_str("the disclaimer hasn't been accepted"),
            ConfirmationMissing { id, needed } => write!(f, "{id} needs confirmation: {needed}"),
            MustApplyAlone { id } => write!(f, "{id} must be applied on its own"),
            PlanChanged(_) => f.write_str("the system changed since this plan was made"),
            Journal(e) => write!(f, "{e}"),
            UnknownTransaction(txn) => write!(f, "no interrupted change numbered {txn}"),
        }
    }
}

impl std::error::Error for EngineError {}

impl From<JournalError> for EngineError {
    fn from(e: JournalError) -> Self {
        EngineError::Journal(e)
    }
}

/// What the user confirmed. The engine checks this itself, so a UI bug can't skip a confirmation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Confirmation {
    /// Ticked "I read the risks".
    pub risks_ticked: bool,
    /// What was typed, per tweak instance.
    pub typed: BTreeMap<String, String>,
}

/// How one tweak in an execution ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TweakResult {
    /// Applied or reverted, and verified by reading back.
    Done,
    /// Nothing needed changing.
    NoChange,
    /// Failed, and everything it changed was restored.
    RolledBack { error: String },
    /// Failed, and restoring also failed. Undo can retry because the baseline is kept.
    RollbackFailed { error: String },
    /// A revert stopped partway. It is safe to run again.
    RevertFailed { error: String },
}

/// The result of executing a plan.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub results: Vec<(String, TweakResult)>,
    /// Tweaks not attempted, because an earlier one failed.
    pub not_attempted: Vec<String>,
}

/// A tweak's state, decided by reading the system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TweakState {
    NotApplied,
    Applied,
    /// Already set, but not by augeo, so there is nothing for augeo to undo.
    AlreadySet,
    Partial,
    /// Something else changed it after augeo applied it.
    ChangedExternally,
    /// A value couldn't be read. This is never reported as "not applied".
    Unknown {
        reason: String,
    },
}

/// One resource's values, for the detail pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceView {
    pub address: Address,
    pub live: Option<Value>,
    pub desired: Value,
    pub baseline: Option<Value>,
}

/// A tweak's full status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TweakStatusView {
    pub state: TweakState,
    pub in_doubt: bool,
    pub restart_pending: bool,
    pub baseline_lost: bool,
    /// Applied by an older definition of this tweak.
    pub older_revision: bool,
    pub rollback_failed: bool,
    /// Why it may have changed, such as a Windows or driver update since it was applied.
    pub drift_note: Option<String>,
    /// Why a tweak's values are in place but doing nothing, such as being on an inactive power plan.
    pub not_in_effect: Option<String>,
    /// What Windows itself reports, where the tweak's own values can't fully show it.
    pub live_note: Option<String>,
    pub resources: Vec<ResourceView>,
}

/// One resource of an interrupted change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InDoubtResource {
    pub address: Address,
    pub baseline: Value,
    pub desired: Value,
    pub live: Option<Value>,
}

/// A change that was interrupted, for the recovery screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InDoubt {
    pub txn: u64,
    pub op: Op,
    pub instance: String,
    pub title: String,
    pub resources: Vec<InDoubtResource>,
}

impl InDoubt {
    /// What the recovery screen offers first.
    pub fn default_decision(&self) -> Decision {
        match self.op {
            Op::Apply => Decision::RollBack,
            Op::Revert => Decision::FinishRevert,
        }
    }
}

/// The result of resolving an interrupted change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Resources left alone because they hold a third value that neither side set.
    pub left_alone: Vec<Address>,
    pub errors: Vec<String>,
}

/// The engine, over any system and store.
#[derive(Debug)]
pub struct Engine<S, T, C> {
    system: S,
    journal: Journal<T>,
    clock: C,
    catalog: Catalog,
    ctx: Context,
}

impl<S: System, T: Store, C: Clock> Engine<S, T, C> {
    /// Opens the journal. As the writer, it also closes interrupted transactions that wrote nothing.
    pub fn open(
        system: S,
        store: T,
        clock: C,
        catalog: Catalog,
        ctx: Context,
        want_writer: bool,
    ) -> Result<Self, EngineError> {
        let journal = Journal::open(store, want_writer, ctx.stamp.clone())?;
        let mut engine = Self {
            system,
            journal,
            clock,
            catalog,
            ctx,
        };
        if engine.journal.is_writer() {
            for open in engine.journal.ledger().open {
                if open.steps.is_empty() {
                    engine.end(open.txn, Outcome::Aborted)?;
                }
            }
        }
        Ok(engine)
    }

    pub fn is_writer(&self) -> bool {
        self.journal.is_writer()
    }

    pub fn open_report(&self) -> &OpenReport {
        self.journal.report()
    }

    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    pub fn ledger(&self) -> Ledger {
        self.journal.ledger()
    }

    fn append(&mut self, body: Body) -> Result<(), JournalError> {
        self.journal.append(self.clock.now_ms(), body)
    }

    fn end(&mut self, txn: u64, outcome: Outcome) -> Result<(), JournalError> {
        self.append(Body::TxnEnd { txn, outcome })
    }

    // ---------------------------------------------------------------- disclaimer

    pub fn disclaimer_accepted(&self) -> bool {
        let wanted = (
            self.ctx.disclaimer.text_sha256.clone(),
            self.ctx.user_sid.clone(),
        );
        self.ledger().accepted.contains(&wanted)
    }

    /// Records that the interactive user accepted the current disclaimer text.
    pub fn accept_disclaimer(&mut self) -> Result<(), EngineError> {
        if self.disclaimer_accepted() {
            return Ok(());
        }
        let body = Body::Accept {
            text_sha256: self.ctx.disclaimer.text_sha256.clone(),
            version: self.ctx.disclaimer.version,
            sid: self.ctx.user_sid.clone(),
        };
        Ok(self.append(body)?)
    }

    // ---------------------------------------------------------------- lookups

    fn tweak(&self, id: &str) -> Result<Tweak, EngineError> {
        self.catalog
            .tweaks
            .iter()
            .find(|t| t.id == id)
            .cloned()
            .ok_or_else(|| EngineError::UnknownTweak(id.to_string()))
    }

    /// Whether a tweak applies to this machine, and if not, why.
    pub fn applicability(&self, tweak: &Tweak) -> Result<(), String> {
        applicability::check(&tweak.applicability, &self.ctx.env)
    }

    fn computed_value(&self, tweak: &Tweak) -> Result<Option<Value>, String> {
        let Some(rev) = tweak.current_rev() else {
            return Ok(None);
        };
        for resource in &rev.resource {
            let desired = match resource {
                Resource::DisplayMode { .. } => {
                    return self
                        .ctx
                        .max_display_mode
                        .clone()
                        .map(|m| Some(Value::Text(m)))
                        .ok_or_else(|| "the display's best mode is unknown".to_string());
                }
                Resource::RegValue { desired, .. }
                | Resource::RegKvString { desired, .. }
                | Resource::PowerIndexAc { desired, .. }
                | Resource::PowerIndexDc { desired, .. }
                | Resource::DrsSetting { desired, .. }
                | Resource::NicProperty { desired, .. } => desired,
                _ => continue,
            };
            if let Desired::Computed {
                computed: Computed::FrameCapFromRefresh,
            } = desired
            {
                return self
                    .ctx
                    .frame_cap
                    .map(|cap| Some(Value::Dword(cap)))
                    .ok_or_else(|| "the display's refresh rate is unknown".to_string());
            }
        }
        Ok(None)
    }

    /// What Windows reports for this tweak, if the tweak has such a state and Windows answers.
    fn reported_scheduling(&self, tweak: &Tweak) -> Option<GpuScheduling> {
        match tweak.live_state {
            Some(LiveState::GpuScheduling) => self.ctx.gpu_scheduling,
            None => None,
        }
    }

    /// Why this tweak can't do anything here, according to Windows.
    fn reported_unavailable(&self, tweak: &Tweak) -> Option<EngineError> {
        self.reported_scheduling(tweak)
            .filter(|s| !s.supported)
            .map(|_| EngineError::NotAvailable {
                id: tweak.id.clone(),
                reason: "Windows reports that this GPU and driver don't support it".to_string(),
            })
    }

    /// The registry value can be missing whichever way the setting is. So when what Windows reports
    /// already matches the chosen option, a missing value is fine, and there's nothing to write.
    fn accept_absent_if_reported(&self, tweak: &Tweak, params: &Params, targets: &mut [Target]) {
        let Some(scheduling) = self.reported_scheduling(tweak) else {
            return;
        };
        if (params.option.as_deref() == Some("on")) == scheduling.enabled {
            for target in targets {
                target.accept_absent = true;
            }
        }
    }

    /// The power plan active right now. If that can't be read, the one from startup is the best guess,
    /// and reading the tweak's own values then reports any real problem.
    fn live_power_scheme(&self) -> String {
        match self.system.read(&Address::PowerActiveScheme) {
            Ok(Value::Text(scheme)) => scheme,
            _ => self.ctx.power_scheme.clone(),
        }
    }

    /// The power plan a tweak switches to, if it does.
    fn plan_selected_by(&self, id: &str) -> Option<String> {
        let tweak = self.tweak(id).ok()?;
        tweak.current_rev()?.resource.iter().find_map(|r| match r {
            Resource::PowerActiveScheme { scheme } => Some(scheme.to_lowercase()),
            _ => None,
        })
    }

    fn build_params(&self, tweak: &Tweak, request: &TweakRequest) -> Result<Params, EngineError> {
        let invalid = |reason: String| EngineError::Invalid {
            id: tweak.id.clone(),
            reason,
        };
        let option = match &request.option {
            Some(key) if tweak.option.iter().any(|o| o.key == *key) => Some(key.clone()),
            Some(key) => return Err(invalid(format!("unknown option '{key}'"))),
            None => tweak
                .option
                .iter()
                .find(|o| o.recommended)
                .map(|o| o.key.clone()),
        };
        let exe = match (&request.exe, tweak.component) {
            (Some(exe), Component::Game) if is_valid_exe(exe) => Some(exe.clone()),
            (Some(exe), Component::Game) => return Err(invalid(format!("invalid game '{exe}'"))),
            (None, Component::Game) => return Err(invalid("this tweak needs a game".to_string())),
            (Some(_), _) => return Err(invalid("only game tweaks take a game".to_string())),
            (None, _) => None,
        };
        let computed = self.computed_value(tweak).map_err(invalid)?;
        let per_plan = tweak
            .current_rev()
            .is_some_and(|rev| sets_plan_values(&rev.resource));
        Ok(Params {
            option,
            exe,
            user_sid: self.ctx.user_sid.clone(),
            power_scheme: Some(if per_plan {
                self.live_power_scheme()
            } else {
                self.ctx.power_scheme.clone()
            }),
            adapters: self.ctx.adapters.clone(),
            display_device: self.ctx.display_device.clone(),
            computed,
        })
    }

    // ---------------------------------------------------------------- planning

    /// Builds the exact changes that applying would make. This never writes, so it is the dry run.
    pub fn plan_apply(&self, requests: &[TweakRequest]) -> Result<Plan, EngineError> {
        let ledger = self.ledger();
        if !ledger.open.is_empty() {
            return Err(EngineError::RecoveryRequired);
        }
        let in_plan: BTreeSet<&str> = requests.iter().map(|r| r.id.as_str()).collect();
        // A batch that switches power plans sets per-plan values on the plan it switches to, because
        // on the old one they'd do nothing.
        let switching_to = requests.iter().find_map(|r| self.plan_selected_by(&r.id));
        let mut tweaks = Vec::new();
        for request in requests {
            tweaks.push(self.plan_one_apply(
                request,
                &ledger,
                &in_plan,
                switching_to.as_deref(),
            )?);
        }
        Ok(Self::finish_plan(
            Op::Apply,
            tweaks,
            PlanSource::Apply(requests.to_vec()),
        ))
    }

    fn plan_one_apply(
        &self,
        request: &TweakRequest,
        ledger: &Ledger,
        in_plan: &BTreeSet<&str>,
        switching_to: Option<&str>,
    ) -> Result<TweakPlan, EngineError> {
        let tweak = self.tweak(&request.id)?;
        let id = tweak.id.clone();
        if tweak.status == Status::Retired {
            return Err(EngineError::Retired {
                id,
                reason: tweak.retired_reason.clone().unwrap_or_default(),
            });
        }
        self.applicability(&tweak)
            .map_err(|reason| EngineError::NotApplicable {
                id: id.clone(),
                reason,
            })?;
        for other in &tweak.requires {
            let applied = ledger.applied.values().any(|a| a.tweak_id == *other);
            if !applied && !in_plan.contains(other.as_str()) {
                return Err(EngineError::Requires {
                    id,
                    other: other.clone(),
                });
            }
        }
        for other in &tweak.conflicts {
            if ledger.applied.values().any(|a| a.tweak_id == *other) {
                return Err(EngineError::Conflict {
                    id,
                    other: other.clone(),
                });
            }
        }

        let mut params = self.build_params(&tweak, request)?;
        let instance = instance_key(&id, params.exe.as_deref());
        let rev = tweak
            .current_rev()
            .ok_or_else(|| EngineError::Invalid {
                id: id.clone(),
                reason: "has no revision".to_string(),
            })?
            .clone();
        if ledger
            .applied
            .get(&instance)
            .is_some_and(|a| a.rev != rev.rev)
        {
            return Err(EngineError::UndoFirst { id });
        }
        if sets_plan_values(&rev.resource) {
            if let Some(scheme) = switching_to {
                params.power_scheme = Some(scheme.to_string());
            }
            // Its old values are on another plan. Writing a second set would leave the first behind.
            let elsewhere = ledger.applied.get(&instance).is_some_and(|a| {
                !same_plan(
                    a.params.power_scheme.as_deref(),
                    params.power_scheme.as_deref(),
                )
            });
            if elsewhere {
                return Err(EngineError::AppliedToOtherPlan { id });
            }
        }
        if !ledger.applied.contains_key(&instance)
            && let Some(error) = self.reported_unavailable(&tweak)
        {
            return Err(error);
        }
        let mut targets = expand(&rev, &params).map_err(|reason| EngineError::Invalid {
            id: id.clone(),
            reason,
        })?;
        if !ledger.applied.contains_key(&instance) {
            self.accept_absent_if_reported(&tweak, &params, &mut targets);
        }
        if targets.is_empty() {
            return Err(EngineError::NotAvailable {
                id,
                reason:
                    "there's nothing on this PC for it to change, such as a wired network adapter"
                        .to_string(),
            });
        }

        let mut steps = Vec::new();
        for target in targets {
            let available =
                self.system
                    .is_available(&target.address)
                    .map_err(|e| EngineError::Unreadable {
                        id: id.clone(),
                        reason: e.to_string(),
                    })?;
            if !available {
                return Err(EngineError::NotAvailable {
                    id,
                    reason: format!("{} doesn't exist on this system", target.address),
                });
            }
            let live = self.read_for(&id, &target.address)?;
            let entry = ledger.resources.get(&target.address.key());
            if let Some(entry) = entry
                && entry.owner != instance
            {
                return Err(EngineError::ResourceOwned {
                    id,
                    owner: entry.owner.clone(),
                    resource: target.address.to_string(),
                });
            }
            let epoch = entry.and_then(|e| e.epochs.last());
            let satisfied = target.satisfied_by(&live);
            let baseline = match epoch {
                // Already applied by this instance. Keep the older baseline unless something else
                // has changed the value since, in which case the live value is what to return to.
                Some(_) if satisfied => None,
                Some(e) if live == e.baseline || live == e.desired => Some(e.baseline.clone()),
                _ => Some(live.clone()),
            };
            steps.push(PlannedStep {
                address: target.address,
                current: live,
                target: target.desired,
                write: !satisfied,
                baseline,
                drift: false,
                baseline_lost: false,
            });
        }
        Ok(TweakPlan {
            instance,
            tweak_id: id,
            title: tweak.title,
            rev: rev.rev,
            danger: tweak.danger,
            effect: tweak.effect,
            security_lowering: tweak.security_lowering,
            params,
            steps,
        })
    }

    fn read_for(&self, id: &str, address: &Address) -> Result<Value, EngineError> {
        self.system
            .read(address)
            .map_err(|e: PortError| EngineError::Unreadable {
                id: id.to_string(),
                reason: format!("{address}: {e}"),
            })
    }

    /// Plans undoing specific instances. An instance augeo hasn't applied is skipped, so undo is safe
    /// to repeat.
    pub fn plan_undo(&self, instances: &[String], policy: UndoPolicy) -> Result<Plan, EngineError> {
        let ledger = self.ledger();
        if !ledger.open.is_empty() {
            return Err(EngineError::RecoveryRequired);
        }
        let mut tweaks = Vec::new();
        for instance in instances {
            if ledger.applied.contains_key(instance) {
                tweaks.push(self.plan_one_undo(instance, policy, &ledger)?);
            }
        }
        Ok(Self::finish_plan(
            Op::Revert,
            tweaks,
            PlanSource::Undo(instances.to_vec(), policy),
        ))
    }

    /// Plans undoing everything, most recent first. Values changed by something else are kept unless
    /// the policy says otherwise.
    pub fn plan_undo_all(&self, policy: UndoPolicy) -> Result<Plan, EngineError> {
        let mut applied: Vec<_> = self.ledger().applied.into_values().collect();
        applied.sort_by_key(|a| std::cmp::Reverse(a.commit_seq));
        let instances: Vec<String> = applied.into_iter().map(|a| a.instance).collect();
        self.plan_undo(&instances, policy)
    }

    fn targets_for_applied(
        &self,
        tweak_id: &str,
        rev_number: u32,
        params: &Params,
    ) -> Result<(Tweak, Vec<Target>), EngineError> {
        let tweak = self.tweak(tweak_id)?;
        let rev = tweak
            .rev
            .iter()
            .find(|r| r.rev == rev_number)
            .ok_or_else(|| EngineError::Invalid {
                id: tweak_id.to_string(),
                reason: format!("this build has no revision {rev_number}"),
            })?;
        let targets = expand(rev, params).map_err(|reason| EngineError::Invalid {
            id: tweak_id.to_string(),
            reason,
        })?;
        Ok((tweak, targets))
    }

    fn plan_one_undo(
        &self,
        instance: &str,
        policy: UndoPolicy,
        ledger: &Ledger,
    ) -> Result<TweakPlan, EngineError> {
        let applied = ledger
            .applied
            .get(instance)
            .ok_or_else(|| EngineError::UnknownTweak(instance.to_string()))?;
        let id = applied.tweak_id.clone();
        let (tweak, targets) = self.targets_for_applied(&id, applied.rev, &applied.params)?;
        let mut steps = Vec::new();
        for target in targets {
            let live = self.read_for(&id, &target.address)?;
            let epoch =
                ledger
                    .resources
                    .get(&target.address.key())
                    .and_then(|e| match policy.epoch {
                        EpochChoice::Newest => e.epochs.last(),
                        EpochChoice::Oldest => e.epochs.first(),
                    });
            let Some(epoch) = epoch else {
                steps.push(PlannedStep {
                    address: target.address,
                    current: live.clone(),
                    target: live,
                    write: false,
                    baseline: None,
                    drift: false,
                    baseline_lost: true,
                });
                continue;
            };
            let at_baseline = live == epoch.baseline;
            let at_desired = live == epoch.desired || target.satisfied_by(&live);
            let drift = !at_baseline && !at_desired;
            let write = !at_baseline && (!drift || policy.drift == DriftPolicy::RestoreBaseline);
            steps.push(PlannedStep {
                address: target.address,
                current: live,
                target: epoch.baseline.clone(),
                write,
                baseline: None,
                drift,
                baseline_lost: false,
            });
        }
        Ok(TweakPlan {
            instance: instance.to_string(),
            tweak_id: id,
            title: tweak.title,
            rev: applied.rev,
            danger: tweak.danger,
            effect: tweak.effect,
            security_lowering: tweak.security_lowering,
            params: applied.params.clone(),
            steps,
        })
    }

    fn finish_plan(op: Op, tweaks: Vec<TweakPlan>, source: PlanSource) -> Plan {
        let canonical: Vec<serde_json::Value> = tweaks
            .iter()
            .map(|t| {
                let steps: Vec<serde_json::Value> = t
                    .steps
                    .iter()
                    .map(|s| {
                        serde_json::json!([s.address.key(), s.current, s.target, s.write, s.drift])
                    })
                    .collect();
                serde_json::json!([t.instance, t.rev, t.params, steps])
            })
            .collect();
        let text = serde_json::json!([format!("{op:?}"), canonical]).to_string();
        let digest = Sha256::digest(text.as_bytes());
        Plan {
            op,
            tweaks,
            hash: digest.iter().map(|b| format!("{b:02x}")).collect(),
            source,
        }
    }

    // ---------------------------------------------------------------- execution

    fn check_confirmation(plan: &Plan, confirmation: &Confirmation) -> Result<(), EngineError> {
        if plan.op != Op::Apply {
            return Ok(());
        }
        for tweak in plan.tweaks.iter().filter(|t| t.writes()) {
            let id = tweak.instance.clone();
            let typed = confirmation.typed.get(&tweak.instance).map(String::as_str);
            match tweak.danger {
                0 | 1 => {}
                2 => {
                    if !confirmation.risks_ticked {
                        return Err(EngineError::ConfirmationMissing {
                            id,
                            needed: "tick \"I read the risks\"".to_string(),
                        });
                    }
                }
                3 => {
                    if typed != Some(TYPED_HIGH) {
                        return Err(EngineError::ConfirmationMissing {
                            id,
                            needed: format!("type {TYPED_HIGH}"),
                        });
                    }
                }
                _ => {
                    if plan.tweaks.len() != 1 {
                        return Err(EngineError::MustApplyAlone { id });
                    }
                    if typed != Some(TYPED_EXPERT) {
                        return Err(EngineError::ConfirmationMissing {
                            id,
                            needed: format!("type {TYPED_EXPERT}"),
                        });
                    }
                }
            }
        }
        Ok(())
    }

    /// Executes a plan the user confirmed. If the system changed since it was made, nothing is written
    /// and the new plan comes back to be shown instead.
    pub fn execute(
        &mut self,
        plan: &Plan,
        confirmation: &Confirmation,
    ) -> Result<Report, EngineError> {
        if !self.is_writer() {
            return Err(EngineError::ReadOnly);
        }
        if !self.ledger().open.is_empty() {
            return Err(EngineError::RecoveryRequired);
        }
        if plan.op == Op::Apply && !self.disclaimer_accepted() {
            return Err(EngineError::DisclaimerNotAccepted);
        }
        Self::check_confirmation(plan, confirmation)?;

        let fresh = match &plan.source {
            PlanSource::Apply(requests) => self.plan_apply(requests)?,
            PlanSource::Undo(instances, policy) => self.plan_undo(instances, *policy)?,
        };
        if fresh.hash != plan.hash {
            return Err(EngineError::PlanChanged(Box::new(fresh)));
        }

        let mut report = Report::default();
        for (i, tweak) in fresh.tweaks.iter().enumerate() {
            let result = match fresh.op {
                Op::Apply => self.run_apply(tweak)?,
                Op::Revert => self.run_revert(tweak)?,
            };
            let failed = matches!(
                result,
                TweakResult::RolledBack { .. } | TweakResult::RollbackFailed { .. }
            );
            report.results.push((tweak.instance.clone(), result));
            // Applying stops at the first failure. Undo keeps going to restore as much as it can.
            if failed {
                report.not_attempted = fresh.tweaks[i + 1..]
                    .iter()
                    .map(|t| t.instance.clone())
                    .collect();
                break;
            }
        }
        Ok(report)
    }

    /// Writes a value and checks it took. Returns what went wrong, if anything.
    fn write_verified(&mut self, target: &Target) -> Option<String> {
        if let Err(e) = self.system.write(&target.address, &target.desired) {
            return Some(format!("{}: {e}", target.address));
        }
        match self.system.read(&target.address) {
            Ok(live) if target.satisfied_by(&live) => None,
            Ok(live) => Some(format!(
                "{}: wrote {} but read back {live}",
                target.address, target.desired
            )),
            Err(e) => Some(format!("{}: couldn't read it back: {e}", target.address)),
        }
    }

    /// One journaled write: intent on disk, write, read back, result on disk.
    fn journaled_write(
        &mut self,
        txn: u64,
        step: u32,
        target: &Target,
    ) -> Result<Option<String>, JournalError> {
        self.append(Body::Intent {
            txn,
            step,
            res_key: target.address.key(),
            value: target.desired.clone(),
        })?;
        let error = self.write_verified(target);
        let observed = self.system.read(&target.address).ok();
        self.append(Body::Done {
            txn,
            step,
            observed,
            error: error.clone(),
        })?;
        Ok(error)
    }

    fn run_apply(&mut self, tweak: &TweakPlan) -> Result<TweakResult, EngineError> {
        if !tweak.writes() {
            return Ok(TweakResult::NoChange);
        }
        let txn = self.journal.next_txn();
        self.append(Body::TxnBegin {
            txn,
            op: Op::Apply,
            instance: tweak.instance.clone(),
            tweak_id: tweak.tweak_id.clone(),
            rev: tweak.rev,
            params: tweak.params.clone(),
            plan_hash: String::new(),
        })?;
        for step in &tweak.steps {
            if let Some(baseline) = &step.baseline {
                self.append(Body::Baseline {
                    txn,
                    res_key: step.address.key(),
                    snapshot: baseline.clone(),
                    desired: step.target.clone(),
                })?;
            }
        }

        let mut next_step = 0;
        let mut attempted: Vec<&PlannedStep> = Vec::new();
        let mut failure = None;
        for step in tweak.steps.iter().filter(|s| s.write) {
            attempted.push(step);
            let target = Target {
                address: step.address.clone(),
                desired: step.target.clone(),
                accept_absent: false,
            };
            match self.journaled_write(txn, next_step, &target) {
                Ok(None) => next_step += 1,
                Ok(Some(error)) => {
                    next_step += 1;
                    failure = Some(error);
                    break;
                }
                Err(journal_error) => {
                    // The journal failed, so stop writing and put back what can be put back. If this
                    // process then dies, recovery sees the open transaction and measures everything.
                    self.restore_without_journal(&attempted);
                    return Err(journal_error.into());
                }
            }
        }

        let Some(error) = failure else {
            self.end(txn, Outcome::Committed)?;
            return Ok(TweakResult::Done);
        };

        let mut rollback_errors = Vec::new();
        for step in attempted.iter().rev() {
            let baseline = step
                .baseline
                .clone()
                .unwrap_or_else(|| step.current.clone());
            let live = self.system.read(&step.address).ok();
            if live.as_ref() == Some(&baseline) {
                continue;
            }
            let target = Target {
                address: step.address.clone(),
                desired: baseline,
                accept_absent: false,
            };
            match self.journaled_write(txn, next_step, &target) {
                Ok(None) => next_step += 1,
                Ok(Some(e)) => {
                    next_step += 1;
                    rollback_errors.push(e);
                }
                Err(journal_error) => {
                    self.restore_without_journal(&attempted);
                    return Err(journal_error.into());
                }
            }
        }
        if rollback_errors.is_empty() {
            self.end(txn, Outcome::RolledBack)?;
            Ok(TweakResult::RolledBack { error })
        } else {
            self.end(txn, Outcome::RollbackFailed)?;
            Ok(TweakResult::RollbackFailed {
                error: format!(
                    "{error}; restoring also failed: {}",
                    rollback_errors.join("; ")
                ),
            })
        }
    }

    fn restore_without_journal(&mut self, attempted: &[&PlannedStep]) {
        for step in attempted.iter().rev() {
            let baseline = step
                .baseline
                .clone()
                .unwrap_or_else(|| step.current.clone());
            let _ = self.system.write(&step.address, &baseline);
        }
    }

    fn run_revert(&mut self, tweak: &TweakPlan) -> Result<TweakResult, EngineError> {
        let txn = self.journal.next_txn();
        self.append(Body::TxnBegin {
            txn,
            op: Op::Revert,
            instance: tweak.instance.clone(),
            tweak_id: tweak.tweak_id.clone(),
            rev: tweak.rev,
            params: tweak.params.clone(),
            plan_hash: String::new(),
        })?;
        let wrote = tweak.writes();
        let errors = self.revert_steps(txn, 0, &tweak.steps)?;
        if errors.is_empty() {
            self.end(txn, Outcome::Committed)?;
            Ok(if wrote {
                TweakResult::Done
            } else {
                TweakResult::NoChange
            })
        } else {
            self.end(txn, Outcome::RevertFailed)?;
            Ok(TweakResult::RevertFailed {
                error: errors.join("; "),
            })
        }
    }

    fn revert_steps(
        &mut self,
        txn: u64,
        first_step: u32,
        steps: &[PlannedStep],
    ) -> Result<Vec<String>, EngineError> {
        let mut errors = Vec::new();
        for (next, step) in (first_step..).zip(steps.iter().filter(|s| s.write).rev()) {
            let target = Target {
                address: step.address.clone(),
                desired: step.target.clone(),
                accept_absent: false,
            };
            if let Some(e) = self.journaled_write(txn, next, &target)? {
                errors.push(e);
            }
        }
        Ok(errors)
    }

    // ---------------------------------------------------------------- recovery

    /// Changes that were interrupted and need a decision.
    pub fn in_doubt(&self) -> Result<Vec<InDoubt>, EngineError> {
        let mut list = Vec::new();
        for open in self.ledger().open {
            let (tweak, targets) =
                self.targets_for_applied(&open.tweak_id, open.rev, &open.params)?;
            let by_key: BTreeMap<String, Address> = targets
                .into_iter()
                .map(|t| (t.address.key(), t.address))
                .collect();
            let resources = open
                .baselines
                .iter()
                .filter_map(|(key, baseline, desired)| {
                    let address = by_key.get(key)?.clone();
                    Some(InDoubtResource {
                        live: self.system.read(&address).ok(),
                        address,
                        baseline: baseline.clone(),
                        desired: desired.clone(),
                    })
                })
                .collect();
            list.push(InDoubt {
                txn: open.txn,
                op: open.op,
                instance: open.instance,
                title: tweak.title,
                resources,
            });
        }
        Ok(list)
    }

    /// Resolves an interrupted change.
    pub fn resolve(&mut self, txn: u64, decision: Decision) -> Result<RecoveryReport, EngineError> {
        if !self.is_writer() {
            return Err(EngineError::ReadOnly);
        }
        let open = self
            .ledger()
            .open
            .into_iter()
            .find(|o| o.txn == txn)
            .ok_or(EngineError::UnknownTransaction(txn))?;
        self.append(Body::Recovery { txn, decision })?;
        match (open.op, decision) {
            (Op::Apply, Decision::Keep) => {
                self.end(txn, Outcome::Committed)?;
                Ok(RecoveryReport::default())
            }
            (Op::Apply, _) => self.roll_back_open(&open),
            (Op::Revert, Decision::FinishRevert | Decision::RollBack) => self.finish_revert(&open),
            (Op::Revert, Decision::Keep) => {
                self.end(txn, Outcome::RevertFailed)?;
                Ok(RecoveryReport::default())
            }
        }
    }

    fn roll_back_open(&mut self, open: &OpenTxn) -> Result<RecoveryReport, EngineError> {
        let (_, targets) = self.targets_for_applied(&open.tweak_id, open.rev, &open.params)?;
        let by_key: BTreeMap<String, Address> = targets
            .into_iter()
            .map(|t| (t.address.key(), t.address))
            .collect();
        let mut report = RecoveryReport::default();
        let mut next = open.steps.len() as u32;
        for (key, baseline, desired) in open.baselines.iter().rev() {
            let Some(address) = by_key.get(key) else {
                continue;
            };
            let live = match self.system.read(address) {
                Ok(live) => live,
                Err(e) => {
                    report.errors.push(format!("{address}: {e}"));
                    continue;
                }
            };
            if live == *baseline {
                continue;
            }
            if live != *desired {
                report.left_alone.push(address.clone());
                continue;
            }
            let target = Target {
                address: address.clone(),
                desired: baseline.clone(),
                accept_absent: false,
            };
            if let Some(e) = self.journaled_write(open.txn, next, &target)? {
                report.errors.push(e);
            }
            next += 1;
        }
        let outcome = if report.errors.is_empty() {
            Outcome::RolledBack
        } else {
            Outcome::RollbackFailed
        };
        self.end(open.txn, outcome)?;
        Ok(report)
    }

    fn finish_revert(&mut self, open: &OpenTxn) -> Result<RecoveryReport, EngineError> {
        let ledger = self.ledger();
        let plan = self.plan_one_undo(&open.instance, UndoPolicy::default(), &ledger)?;
        let errors = self.revert_steps(open.txn, open.steps.len() as u32, &plan.steps)?;
        let left_alone = plan
            .steps
            .iter()
            .filter(|s| s.drift)
            .map(|s| s.address.clone())
            .collect();
        let outcome = if errors.is_empty() {
            Outcome::Committed
        } else {
            Outcome::RevertFailed
        };
        self.end(open.txn, outcome)?;
        Ok(RecoveryReport { left_alone, errors })
    }

    // ---------------------------------------------------------------- state

    /// Reads a tweak's real state from the system.
    pub fn status(&self, id: &str, exe: Option<&str>) -> Result<TweakStatusView, EngineError> {
        let tweak = self.tweak(id)?;
        let ledger = self.ledger();
        let instance = instance_key(id, exe);
        let applied = ledger.applied.get(&instance);
        let in_doubt = ledger.open.iter().any(|o| o.instance == instance);

        let older_revision =
            applied.is_some_and(|a| tweak.current_rev().is_some_and(|r| r.rev != a.rev));
        let (targets, _params) = match applied {
            Some(a) => (
                self.targets_for_applied(id, a.rev, &a.params)?.1,
                a.params.clone(),
            ),
            None => {
                let request = TweakRequest {
                    id: id.to_string(),
                    option: None,
                    exe: exe.map(str::to_string),
                };
                let params = self.build_params(&tweak, &request)?;
                let rev = tweak.current_rev().ok_or_else(|| EngineError::Invalid {
                    id: id.to_string(),
                    reason: "has no revision".to_string(),
                })?;
                if let Some(error) = self.reported_unavailable(&tweak) {
                    return Err(error);
                }
                let mut targets = expand(rev, &params).map_err(|reason| EngineError::Invalid {
                    id: id.to_string(),
                    reason,
                })?;
                if targets.is_empty() {
                    return Err(EngineError::NotAvailable {
                        id: id.to_string(),
                        reason: "there's nothing on this PC for it to change".to_string(),
                    });
                }
                self.accept_absent_if_reported(&tweak, &params, &mut targets);
                // Planning would refuse this, so "not applied" would wrongly suggest it could be.
                // Only a clear no counts here. A failed check is left to the reads below to report.
                if let Some(missing) = targets
                    .iter()
                    .find(|t| matches!(self.system.is_available(&t.address), Ok(false)))
                {
                    return Err(EngineError::NotAvailable {
                        id: id.to_string(),
                        reason: format!("{} doesn't exist on this system", missing.address),
                    });
                }
                (targets, params)
            }
        };

        let not_in_effect = applied
            .filter(|_| {
                targets.iter().any(|t| {
                    matches!(t.address, Address::PowerAc { .. } | Address::PowerDc { .. })
                })
            })
            .and_then(|a| a.params.power_scheme.as_deref())
            .and_then(|applied_to| {
                let active = self.live_power_scheme();
                (!applied_to.eq_ignore_ascii_case(&active)).then(|| {
                    format!(
                        "It's set on {}, but {} is active now, so it isn't in effect. Undo it, then \
                         apply it again to use it on the current plan.",
                        plan_label(applied_to),
                        plan_label(&active)
                    )
                })
            });

        let mut resources = Vec::new();
        let mut read_error = None;
        let (mut at_desired, mut at_baseline, mut third) = (0, 0, 0);
        let mut baseline_lost = false;
        for target in targets {
            let live = match self.system.read(&target.address) {
                Ok(v) => Some(v),
                Err(e) => {
                    read_error = Some(format!("{}: {e}", target.address));
                    None
                }
            };
            let epoch = ledger
                .resources
                .get(&target.address.key())
                .filter(|e| e.owner == instance)
                .and_then(|e| e.epochs.last());
            if applied.is_some() && epoch.is_none() {
                baseline_lost = true;
            }
            if let Some(live) = &live {
                let desired = epoch.map_or(&target.desired, |e| &e.desired);
                if live == desired || target.satisfied_by(live) {
                    at_desired += 1;
                } else if applied.is_none() || epoch.is_some_and(|e| *live == e.baseline) {
                    at_baseline += 1;
                } else {
                    third += 1;
                }
            }
            resources.push(ResourceView {
                desired: target.desired.clone(),
                baseline: epoch.map(|e| e.baseline.clone()),
                address: target.address,
                live,
            });
        }

        let state = if let Some(reason) = read_error {
            TweakState::Unknown { reason }
        } else {
            match applied {
                None if at_baseline == 0 => TweakState::AlreadySet,
                None if at_desired == 0 => TweakState::NotApplied,
                None => TweakState::Partial,
                Some(_) if third > 0 => TweakState::ChangedExternally,
                // A revert that stopped partway can leave every value restored. That isn't drift.
                Some(a) if a.revert_failed && at_desired == 0 => TweakState::NotApplied,
                Some(_) if at_baseline == 0 => TweakState::Applied,
                Some(_) if at_desired == 0 => TweakState::ChangedExternally,
                Some(_) => TweakState::Partial,
            }
        };

        let drift_note = if state == TweakState::ChangedExternally {
            applied.and_then(|a| {
                if a.os_build != self.ctx.stamp.os_build {
                    Some(format!(
                        "Windows build changed from {} to {} since this was applied.",
                        a.os_build, self.ctx.stamp.os_build
                    ))
                } else if a.driver != self.ctx.stamp.driver {
                    Some(format!(
                        "The NVIDIA driver changed from {} to {} since this was applied.",
                        a.driver.as_deref().unwrap_or("none"),
                        self.ctx.stamp.driver.as_deref().unwrap_or("none")
                    ))
                } else {
                    None
                }
            })
        } else {
            None
        };

        let restart_pending = tweak.effect == Effect::Restart
            && applied.is_some_and(|a| a.boot_id == self.ctx.stamp.boot_id);

        Ok(TweakStatusView {
            state,
            in_doubt,
            restart_pending,
            baseline_lost,
            older_revision,
            rollback_failed: applied.is_some_and(|a| a.rollback_failed),
            drift_note,
            not_in_effect,
            live_note: self.reported_scheduling(&tweak).map(|s| {
                format!(
                    "Windows reports it's {} right now.",
                    if s.enabled { "on" } else { "off" }
                )
            }),
            resources,
        })
    }

    /// Applied tweaks still waiting on a restart. Benchmarks refuse to run while any are pending.
    pub fn pending_restarts(&self) -> Vec<String> {
        self.ledger()
            .applied
            .values()
            .filter(|a| {
                a.boot_id == self.ctx.stamp.boot_id
                    && self
                        .tweak(&a.tweak_id)
                        .is_ok_and(|t| t.effect == Effect::Restart)
            })
            .map(|a| a.instance.clone())
            .collect()
    }

    /// The `recommended` preset: active, applicable tweaks the catalog marks for it.
    pub fn preset_requests(&self) -> Vec<TweakRequest> {
        self.catalog
            .tweaks
            .iter()
            .filter(|t| t.preset && t.status == Status::Active && !t.security_lowering)
            .filter(|t| self.applicability(t).is_ok())
            .map(|t| TweakRequest::new(&t.id))
            .collect()
    }
}
