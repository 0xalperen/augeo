//! The catalog's data model. Unknown fields are errors, so a typo can't silently drop a safety field.
//!
//! A tweak declares resources and desired values and contains no revert script. Reverting means
//! restoring the snapshot taken before the write, so apply, verify and revert can't disagree.

use std::collections::BTreeMap;

use augeo_command::Component;
use serde::{Deserialize, Deserializer, Serialize};

fn component<'de, D: Deserializer<'de>>(d: D) -> Result<Component, D::Error> {
    let name = String::deserialize(d)?;
    Component::from_name(&name)
        .ok_or_else(|| serde::de::Error::custom(format!("unknown component '{name}'")))
}

/// One file under `catalog/`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogFile {
    #[serde(default)]
    pub tweak: Vec<Tweak>,
    #[serde(default)]
    pub myth: Vec<Myth>,
}

/// A popular tweak augeo deliberately doesn't ship, and why. Shown on the Myths screen.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Myth {
    pub name: String,
    pub reason: String,
    pub source: Option<String>,
}

/// A state Windows reports directly. It's a closed list, like resource kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveState {
    /// The registry value can be missing whichever way this is set. The tweak's options must be `on`
    /// and `off`.
    GpuScheduling,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tweak {
    /// A stable slug. It never changes once released.
    pub id: String,
    #[serde(deserialize_with = "component")]
    pub component: Component,
    /// The number used in `auge!cpu#1`. It never changes or gets reused.
    pub number: u32,
    pub title: String,
    pub summary: String,
    #[serde(default)]
    pub kind: TweakKind,
    pub danger: u8,
    pub evidence: Evidence,
    pub effect: Effect,
    #[serde(default)]
    pub status: Status,
    /// Why the tweak was retired. Required when `status` is `retired`.
    pub retired_reason: Option<String>,
    pub superseded_by: Option<String>,
    pub risks: Vec<String>,
    pub undo_note: String,
    #[serde(default)]
    pub conflicts: Vec<String>,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub security_lowering: bool,
    /// Where Windows reports a state that the tweak's own values can't fully show.
    #[serde(default)]
    pub live_state: Option<LiveState>,
    /// Whether the `recommended` preset includes this tweak.
    #[serde(default)]
    pub preset: bool,
    #[serde(default)]
    pub applicability: Applicability,
    #[serde(default)]
    pub option: Vec<TweakOption>,
    pub sources: Vec<Source>,
    #[serde(default)]
    pub anticheat: Vec<AntiCheatNote>,
    pub rev: Vec<Revision>,
}

impl Tweak {
    /// The revision new applies use.
    pub fn current_rev(&self) -> Option<&Revision> {
        self.rev.iter().max_by_key(|r| r.rev)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TweakKind {
    #[default]
    Tweak,
    /// Actionable only when a harmful state is detected.
    Repair,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
pub enum Evidence {
    Measured,
    Documented,
    Community,
    Experimental,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effect {
    Now,
    App,
    Adapter,
    Logoff,
    Restart,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Active,
    Retired,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Applicability {
    /// A build requirement such as `>=19041`.
    #[serde(default = "default_build")]
    pub build: String,
    #[serde(default)]
    pub gpu: GpuRule,
    #[serde(default)]
    pub chassis: ChassisRule,
    #[serde(default)]
    pub cpu_vendor: CpuVendorRule,
    #[serde(default)]
    pub cpu_layout: CpuLayoutRule,
    /// Only when G-SYNC or another VRR mode is active on the primary display.
    #[serde(default)]
    pub vrr: bool,
}

impl Default for Applicability {
    fn default() -> Self {
        Self {
            build: default_build(),
            gpu: GpuRule::default(),
            chassis: ChassisRule::default(),
            cpu_vendor: CpuVendorRule::default(),
            cpu_layout: CpuLayoutRule::default(),
            vrr: false,
        }
    }
}

fn default_build() -> String {
    ">=19041".to_string()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GpuRule {
    #[default]
    Any,
    Nvidia,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChassisRule {
    #[default]
    Any,
    Desktop,
    Laptop,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CpuVendorRule {
    #[default]
    Any,
    Amd,
    Intel,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CpuLayoutRule {
    #[default]
    Any,
    DualCcdX3d,
    NotDualCcdX3d,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TweakOption {
    pub key: String,
    pub name: String,
    #[serde(default)]
    pub recommended: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub url: String,
    pub retrieved: String,
    pub claim: String,
    pub measured: Option<Measured>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Measured {
    pub hw: String,
    pub n: u32,
    pub effect: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AntiCheatSystem {
    Vanguard,
    Faceit,
    Eac,
    Battleye,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AntiCheatStance {
    #[default]
    Untested,
    KnownIssue,
    RequiredByVendor,
}

/// A tweak with no note for a system reads as `untested`. Nothing ever reads as safe.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AntiCheatNote {
    pub system: AntiCheatSystem,
    #[serde(default)]
    pub stance: AntiCheatStance,
    pub source: String,
    pub note: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Revision {
    pub rev: u32,
    #[serde(default)]
    pub resource: Vec<Resource>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Machine,
    /// The interactive user's hive, resolved by SID even when elevated as another account.
    User,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegType {
    Dword,
    Qword,
    Sz,
    MultiSz,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IdSource {
    /// Listed in NVIDIA's public `NvApiDriverSettings.h`.
    Public,
    /// Known only from community files. May vanish in any driver.
    Community,
}

/// A value a resource should hold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum Desired {
    Int(i64),
    Text(String),
    List(Vec<String>),
    /// The value must not exist.
    Absent {
        absent: bool,
    },
    /// The value depends on the option the user picks.
    ByOption {
        by_option: BTreeMap<String, Desired>,
    },
    /// Worked out at plan time, such as a frame cap from the refresh rate, and shown before applying.
    Computed {
        computed: Computed,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Computed {
    FrameCapFromRefresh,
}

/// The closed list of things a tweak may change. There is no script kind, on purpose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Resource {
    RegValue {
        scope: Scope,
        path: String,
        name: String,
        #[serde(rename = "type")]
        value_type: RegType,
        desired: Desired,
        /// A missing value also counts as satisfied. For repairs where absent is the healthy default.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        accept_absent: bool,
    },
    /// One `key=value;` entry in a registry string shared with other keys.
    RegKvString {
        scope: Scope,
        path: String,
        name: String,
        key: String,
        desired: Desired,
    },
    PowerIndexAc {
        subgroup: String,
        setting: String,
        desired: Desired,
    },
    PowerIndexDc {
        subgroup: String,
        setting: String,
        desired: Desired,
    },
    PowerActiveScheme {
        scheme: String,
    },
    /// A boot setting. v0.1.0 only ever removes these, so there is no desired value.
    BcdElement {
        element: String,
    },
    DrsSetting {
        /// `base` for the global profile, or `{exe}` for the selected game.
        profile: String,
        setting: u32,
        id_source: IdSource,
        desired: Desired,
    },
    NicProperty {
        keyword: String,
        desired: Desired,
    },
    DisplayMode {
        mode: DisplayModeTarget,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayModeTarget {
    MaxRefresh,
}

impl Resource {
    /// Identifies what this resource touches. Two active tweaks can't share one without declaring it.
    pub fn key(&self) -> String {
        match self {
            Resource::RegValue {
                scope, path, name, ..
            } => format!("reg:{scope:?}:{path}:{name}").to_lowercase(),
            Resource::RegKvString {
                scope,
                path,
                name,
                key,
                ..
            } => format!("regkv:{scope:?}:{path}:{name}:{key}").to_lowercase(),
            Resource::PowerIndexAc {
                subgroup, setting, ..
            } => format!("powerac:{subgroup}:{setting}").to_lowercase(),
            Resource::PowerIndexDc {
                subgroup, setting, ..
            } => format!("powerdc:{subgroup}:{setting}").to_lowercase(),
            Resource::PowerActiveScheme { .. } => "power:active-scheme".to_string(),
            Resource::BcdElement { element } => format!("bcd:{element}").to_lowercase(),
            Resource::DrsSetting {
                profile, setting, ..
            } => format!("drs:{profile}:{setting:08x}").to_lowercase(),
            Resource::NicProperty { keyword, .. } => format!("nic:{keyword}").to_lowercase(),
            Resource::DisplayMode { .. } => "display:mode".to_string(),
        }
    }
}
