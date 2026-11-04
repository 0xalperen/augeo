//! Turns PresentMon captures into metrics, and compares runs without claiming more than they show.
//!
//! The rules behind this:
//! - The run is the statistical unit, because frames within a run are correlated.
//! - p99 frame time is the primary metric, chosen in advance. Others are corrected for multiple tests.
//! - A result inside the noise says "no detectable effect", and says how big an effect could have been
//!   detected.
//! - Runs captured under different conditions aren't compared at all.

#![forbid(unsafe_code)]

mod capture;
mod stats;

use serde::{Deserialize, Serialize};

pub use capture::{RunMetrics, metrics_from_csv};
pub use stats::{
    Comparison, Finding, Fingerprint, MetricKind, Verdict, compare, min_detectable_percent,
    t_two_sided_p,
};

/// One finished run, as kept in the history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedRun {
    /// Runs captured together share a series, which is what `auge!bench compare` takes.
    pub series: String,
    /// The run's position in its series.
    pub index: u32,
    /// What this run was for, such as "tweak off".
    pub label: String,
    pub time_ms: u64,
    pub fingerprint: Fingerprint,
    pub metrics: RunMetrics,
}
