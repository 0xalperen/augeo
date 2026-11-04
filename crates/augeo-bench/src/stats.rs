//! Comparing two sets of runs. Each run is one sample, because frames within a run are correlated.

use serde::{Deserialize, Serialize};

use crate::capture::RunMetrics;

const ALPHA: f64 = 0.05;
const MIN_RUNS: usize = 3;
/// Below this frame-time spread relative to the mean, a run is pinned at a cap or the refresh rate.
const SATURATED_SD_RATIO: f64 = 0.02;

/// The conditions a run was captured under. Runs that differ here can't be compared fairly.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint {
    pub exe: String,
    pub windows_build: u32,
    pub driver: Option<String>,
    pub display_mode: Option<String>,
    pub power_scheme: Option<String>,
}

/// A metric, and which direction is better.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricKind {
    /// The primary metric, chosen in advance.
    P99FrameTime,
    AvgFps,
    Low1PercentileFps,
    Low1AverageFps,
    FrameTimeSd,
    PcLatency,
    ClickToPhoton,
    InputToPhoton,
    RenderPresent,
}

impl MetricKind {
    const ALL: [MetricKind; 9] = [
        MetricKind::P99FrameTime,
        MetricKind::AvgFps,
        MetricKind::Low1PercentileFps,
        MetricKind::Low1AverageFps,
        MetricKind::FrameTimeSd,
        MetricKind::PcLatency,
        MetricKind::ClickToPhoton,
        MetricKind::InputToPhoton,
        MetricKind::RenderPresent,
    ];

    pub fn label(self) -> &'static str {
        match self {
            MetricKind::P99FrameTime => "p99 frame time (primary)",
            MetricKind::AvgFps => "Average FPS",
            MetricKind::Low1PercentileFps => "1% low FPS (percentile)",
            MetricKind::Low1AverageFps => "1% low FPS (average of worst)",
            MetricKind::FrameTimeSd => "Frame time variation",
            MetricKind::PcLatency => "PC latency",
            MetricKind::ClickToPhoton => "Click to photon latency",
            MetricKind::InputToPhoton => "Input to photon latency",
            MetricKind::RenderPresent => "Render to present latency",
        }
    }

    fn higher_is_better(self) -> bool {
        matches!(
            self,
            MetricKind::AvgFps | MetricKind::Low1PercentileFps | MetricKind::Low1AverageFps
        )
    }

    fn value(self, run: &RunMetrics) -> Option<f64> {
        match self {
            MetricKind::P99FrameTime => Some(run.p99_ms),
            MetricKind::AvgFps => Some(run.avg_fps),
            MetricKind::Low1PercentileFps => Some(run.low_1_percentile_fps),
            MetricKind::Low1AverageFps => Some(run.low_1_average_fps),
            MetricKind::FrameTimeSd => Some(run.frame_time_sd_ms),
            MetricKind::PcLatency => run.pc_latency_ms,
            MetricKind::ClickToPhoton => run.click_to_photon_ms,
            MetricKind::InputToPhoton => run.input_to_photon_ms,
            MetricKind::RenderPresent => run.render_present_ms,
        }
    }
}

/// What a comparison concluded.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Improved,
    Worse,
    /// Within the noise. This doesn't mean no effect, only none this test could see.
    NoDetectableEffect {
        could_detect_percent: Option<f64>,
    },
    /// Not enough runs to say anything.
    NotEnoughRuns,
}

/// One metric's result.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub metric: MetricKind,
    pub before: f64,
    pub after: f64,
    pub change_percent: f64,
    pub p_value: Option<f64>,
    pub verdict: Verdict,
}

/// The result of comparing two sets of runs.
#[derive(Debug, Clone, PartialEq)]
pub enum Comparison {
    /// Conditions differed, so no conclusion is drawn.
    NotComparable { reasons: Vec<String> },
    Compared {
        findings: Vec<Finding>,
        /// FPS is pinned at a cap or the refresh rate, so frame rate can't show a gain.
        saturated: bool,
        warnings: Vec<String>,
    },
}

fn ln_gamma(x: f64) -> f64 {
    // Lanczos approximation.
    const G: f64 = 7.0;
    const C: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    if x < 0.5 {
        let pi = std::f64::consts::PI;
        return (pi / (pi * x).sin()).ln() - ln_gamma(1.0 - x);
    }
    let x = x - 1.0;
    let mut a = C[0];
    for (i, c) in C.iter().enumerate().skip(1) {
        a += c / (x + i as f64);
    }
    let t = x + G + 0.5;
    0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + a.ln()
}

/// The regularized incomplete beta function, by continued fraction.
fn incomplete_beta(a: f64, b: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let front =
        (ln_gamma(a + b) - ln_gamma(a) - ln_gamma(b) + a * x.ln() + b * (1.0 - x).ln()).exp();
    if x > (a + 1.0) / (a + b + 2.0) {
        return 1.0 - incomplete_beta(b, a, 1.0 - x);
    }
    let tiny = 1e-300;
    let mut f = 1.0;
    let mut c = 1.0;
    let mut d = 0.0;
    for i in 0..500 {
        let m = f64::from(i / 2);
        let numerator = if i == 0 {
            1.0
        } else if i % 2 == 0 {
            m * (b - m) * x / ((a + 2.0 * m - 1.0) * (a + 2.0 * m))
        } else {
            -(a + m) * (a + b + m) * x / ((a + 2.0 * m) * (a + 2.0 * m + 1.0))
        };
        d = 1.0 + numerator * d;
        if d.abs() < tiny {
            d = tiny;
        }
        d = 1.0 / d;
        c = 1.0 + numerator / c;
        if c.abs() < tiny {
            c = tiny;
        }
        let delta = c * d;
        f *= delta;
        if (delta - 1.0).abs() < 1e-12 {
            break;
        }
    }
    front * (f - 1.0) / a
}

/// The two-sided p-value of Student's t.
pub fn t_two_sided_p(t: f64, df: f64) -> f64 {
    incomplete_beta(df / 2.0, 0.5, df / (df + t * t))
}

/// The t value with this much probability above it, one-sided.
fn t_quantile_upper(probability: f64, df: f64) -> f64 {
    let (mut low, mut high) = (0.0_f64, 1000.0_f64);
    for _ in 0..200 {
        let mid = (low + high) / 2.0;
        if t_two_sided_p(mid, df) / 2.0 > probability {
            low = mid;
        } else {
            high = mid;
        }
    }
    (low + high) / 2.0
}

/// The smallest change this many runs per side could reliably detect, with 80% power, in percent.
pub fn min_detectable_percent(run_noise_percent: f64, runs_per_side: usize) -> Option<f64> {
    if runs_per_side < 2 {
        return None;
    }
    let n = runs_per_side as f64;
    let df = 2.0 * n - 2.0;
    let multiplier = t_quantile_upper(ALPHA / 2.0, df) + t_quantile_upper(0.2, df);
    Some(multiplier * (2.0 / n).sqrt() * run_noise_percent)
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

fn variance(values: &[f64]) -> f64 {
    let m = mean(values);
    values.iter().map(|v| (v - m).powi(2)).sum::<f64>() / (values.len() as f64 - 1.0)
}

/// Welch's t on log values, since a metric's run-to-run noise scales with its size.
fn welch_p(a: &[f64], b: &[f64]) -> Option<f64> {
    if a.len() < 2 || b.len() < 2 {
        return None;
    }
    let (va, vb) = (variance(a) / a.len() as f64, variance(b) / b.len() as f64);
    let standard_error = (va + vb).sqrt();
    if standard_error == 0.0 {
        return Some(if mean(a) == mean(b) { 1.0 } else { 0.0 });
    }
    let t = (mean(a) - mean(b)) / standard_error;
    let df = (va + vb).powi(2)
        / (va.powi(2) / (a.len() as f64 - 1.0) + vb.powi(2) / (b.len() as f64 - 1.0));
    Some(t_two_sided_p(t, df))
}

/// A paired t test on per-block differences. It cancels drift such as warming up, for A-B-B-A runs.
fn paired_p(differences: &[f64]) -> Option<f64> {
    if differences.len() < 2 {
        return None;
    }
    let standard_error = (variance(differences) / differences.len() as f64).sqrt();
    if standard_error == 0.0 {
        return Some(if mean(differences) == 0.0 { 1.0 } else { 0.0 });
    }
    let t = mean(differences) / standard_error;
    Some(t_two_sided_p(t, differences.len() as f64 - 1.0))
}

fn logs(values: &[f64]) -> Vec<f64> {
    values
        .iter()
        .filter(|v| **v > 0.0)
        .map(|v| v.ln())
        .collect()
}

/// Compares runs. `paired` means both sides were captured alternately and line up block by block.
pub fn compare(
    before: &[(Fingerprint, RunMetrics)],
    after: &[(Fingerprint, RunMetrics)],
    paired: bool,
) -> Comparison {
    let mut reasons = Vec::new();
    let all = || before.iter().chain(after);
    if let Some((first, _)) = all().next() {
        for (fingerprint, _) in all() {
            let mut differs = |label: &str, same: bool| {
                let text = format!("{label} differs between runs");
                if !same && !reasons.contains(&text) {
                    reasons.push(text);
                }
            };
            differs("The game", fingerprint.exe.eq_ignore_ascii_case(&first.exe));
            differs(
                "The Windows build",
                fingerprint.windows_build == first.windows_build,
            );
            differs("The NVIDIA driver", fingerprint.driver == first.driver);
            differs(
                "The display mode",
                fingerprint.display_mode == first.display_mode,
            );
            differs(
                "The power plan",
                fingerprint.power_scheme == first.power_scheme,
            );
        }
    }
    let modes: Vec<&Option<String>> = all().map(|(_, m)| &m.present_mode).collect();
    if modes.windows(2).any(|w| w[0] != w[1]) {
        reasons
            .push("The present mode differs between runs, which changes latency by itself".into());
    }
    let generated =
        |runs: &[(Fingerprint, RunMetrics)]| runs.iter().any(|(_, m)| m.generated_frames > 0);
    if generated(before) != generated(after) {
        reasons.push("Frame generation was on for only one side, which inflates FPS".into());
    }
    if !reasons.is_empty() {
        return Comparison::NotComparable { reasons };
    }

    let mut warnings: Vec<String> = all().filter_map(|(_, m)| m.warning.clone()).collect();
    warnings.dedup();
    let saturated = !before.is_empty()
        && !after.is_empty()
        && all().all(|(_, m)| {
            m.avg_fps > 0.0 && m.frame_time_sd_ms / (1000.0 / m.avg_fps) < SATURATED_SD_RATIO
        });

    let enough = before.len() >= MIN_RUNS && after.len() >= MIN_RUNS;
    // (metric, before, after, p). Secondary metrics are corrected for being tested together.
    let mut rows = Vec::new();
    for metric in MetricKind::ALL {
        let a: Vec<f64> = before.iter().filter_map(|(_, m)| metric.value(m)).collect();
        let b: Vec<f64> = after.iter().filter_map(|(_, m)| metric.value(m)).collect();
        if a.is_empty() || b.is_empty() {
            continue;
        }
        let p = if !enough || a.len() != before.len() || b.len() != after.len() {
            None
        } else if paired && a.len() == b.len() {
            let (la, lb) = (logs(&a), logs(&b));
            let differences: Vec<f64> = la.iter().zip(&lb).map(|(x, y)| y - x).collect();
            paired_p(&differences)
        } else {
            welch_p(&logs(&a), &logs(&b))
        };
        rows.push((metric, mean(&a), mean(&b), p));
    }

    // Holm correction across the secondary metrics. The primary is tested on its own.
    let mut secondary: Vec<usize> = (0..rows.len())
        .filter(|i| rows[*i].0 != MetricKind::P99FrameTime && rows[*i].3.is_some())
        .collect();
    secondary.sort_by(|x, y| {
        rows[*x]
            .3
            .partial_cmp(&rows[*y].3)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut significant = vec![false; rows.len()];
    let tests = secondary.len();
    for (rank, index) in secondary.into_iter().enumerate() {
        let threshold = ALPHA / (tests - rank) as f64;
        if rows[index].3.is_some_and(|p| p < threshold) {
            significant[index] = true;
        } else {
            break;
        }
    }
    for (i, row) in rows.iter().enumerate() {
        if row.0 == MetricKind::P99FrameTime {
            significant[i] = row.3.is_some_and(|p| p < ALPHA);
        }
    }

    let findings = rows
        .iter()
        .enumerate()
        .map(|(i, (metric, a, b, p))| {
            let change_percent = if *a != 0.0 { (b - a) / a * 100.0 } else { 0.0 };
            let verdict = if p.is_none() {
                Verdict::NotEnoughRuns
            } else if significant[i] {
                if (b > a) == metric.higher_is_better() {
                    Verdict::Improved
                } else {
                    Verdict::Worse
                }
            } else {
                let series: Vec<f64> = before.iter().filter_map(|(_, m)| metric.value(m)).collect();
                let noise = logs(&series);
                let could_detect_percent = (noise.len() >= 2)
                    .then(|| variance(&noise).sqrt() * 100.0)
                    .and_then(|sd| min_detectable_percent(sd, before.len().min(after.len())));
                Verdict::NoDetectableEffect {
                    could_detect_percent,
                }
            };
            Finding {
                metric: *metric,
                before: *a,
                after: *b,
                change_percent,
                p_value: *p,
                verdict,
            }
        })
        .collect();

    Comparison::Compared {
        findings,
        saturated,
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fingerprint() -> Fingerprint {
        Fingerprint {
            exe: "game.exe".into(),
            windows_build: 26100,
            driver: Some("610.88".into()),
            display_mode: Some("2560x1440@240".into()),
            power_scheme: Some("x".into()),
        }
    }

    fn run(p99: f64, fps: f64) -> (Fingerprint, RunMetrics) {
        (
            fingerprint(),
            RunMetrics {
                frames: 10_000,
                seconds: 90.0,
                avg_fps: fps,
                low_1_percentile_fps: 1000.0 / p99,
                low_1_average_fps: 900.0 / p99,
                p99_ms: p99,
                frame_time_sd_ms: 2.0,
                present_mode: Some("Hardware: Independent Flip".into()),
                ..RunMetrics::default()
            },
        )
    }

    fn close(a: f64, b: f64, tolerance: f64) {
        assert!((a - b).abs() < tolerance, "{a} vs {b}");
    }

    fn primary(comparison: &Comparison) -> Finding {
        let Comparison::Compared { findings, .. } = comparison else {
            panic!("{comparison:?}");
        };
        findings
            .iter()
            .find(|f| f.metric == MetricKind::P99FrameTime)
            .unwrap()
            .clone()
    }

    #[test]
    fn t_values_match_published_tables() {
        close(t_two_sided_p(2.306, 8.0), 0.05, 0.001);
        close(t_two_sided_p(2.228, 10.0), 0.05, 0.001);
        close(t_two_sided_p(1.96, 100_000.0), 0.05, 0.001);
        close(t_two_sided_p(0.0, 8.0), 1.0, 1e-9);
        close(t_two_sided_p(3.355, 8.0), 0.01, 0.001);
    }

    #[test]
    fn quantiles_invert_the_distribution() {
        close(t_quantile_upper(0.025, 8.0), 2.306, 0.002);
        close(t_quantile_upper(0.2, 8.0), 0.889, 0.002);
    }

    #[test]
    fn the_detectable_effect_matches_the_planned_numbers() {
        // From the plan: at 1.5% run noise, n=5 about 3%, n=10 about 2%, n=20 about 1.4%.
        close(min_detectable_percent(1.5, 5).unwrap(), 3.0, 0.1);
        close(min_detectable_percent(1.5, 10).unwrap(), 2.0, 0.1);
        close(min_detectable_percent(1.5, 20).unwrap(), 1.4, 0.1);
        assert_eq!(min_detectable_percent(1.5, 1), None);
    }

    #[test]
    fn a_clear_improvement_is_reported() {
        let before = [
            run(20.0, 100.0),
            run(20.2, 99.5),
            run(19.8, 100.5),
            run(20.1, 100.0),
        ];
        let after = [
            run(16.0, 120.0),
            run(16.2, 119.5),
            run(15.8, 120.5),
            run(16.1, 120.0),
        ];
        let finding = primary(&compare(&before, &after, false));
        assert_eq!(finding.verdict, Verdict::Improved);
        close(finding.change_percent, -20.0, 1.0);
    }

    #[test]
    fn a_clear_regression_is_reported_as_worse() {
        let before = [run(16.0, 120.0), run(16.2, 119.0), run(15.8, 121.0)];
        let after = [run(20.0, 100.0), run(20.2, 99.0), run(19.8, 101.0)];
        assert_eq!(
            primary(&compare(&before, &after, false)).verdict,
            Verdict::Worse
        );
    }

    #[test]
    fn a_change_within_the_noise_says_so_and_how_much_could_be_seen() {
        let before = [
            run(20.0, 100.0),
            run(21.0, 98.0),
            run(19.0, 102.0),
            run(20.5, 99.0),
            run(19.5, 101.0),
        ];
        let after = [
            run(20.2, 99.0),
            run(19.3, 101.0),
            run(20.9, 98.0),
            run(19.8, 100.0),
            run(20.4, 100.0),
        ];
        let finding = primary(&compare(&before, &after, false));
        let Verdict::NoDetectableEffect {
            could_detect_percent,
        } = finding.verdict
        else {
            panic!("{finding:?}");
        };
        assert!(could_detect_percent.unwrap() > 1.0);
    }

    #[test]
    fn identical_runs_are_never_a_gain() {
        let runs = [run(20.0, 100.0), run(20.0, 100.0), run(20.0, 100.0)];
        let finding = primary(&compare(&runs, &runs, false));
        assert!(
            matches!(finding.verdict, Verdict::NoDetectableEffect { .. }),
            "{finding:?}"
        );
        let finding = primary(&compare(&runs, &runs, true));
        assert!(
            matches!(finding.verdict, Verdict::NoDetectableEffect { .. }),
            "{finding:?}"
        );
    }

    #[test]
    fn too_few_runs_draw_no_conclusion_even_with_a_big_difference() {
        let before = [run(20.0, 100.0), run(20.1, 100.0)];
        let after = [run(10.0, 200.0), run(10.1, 200.0)];
        assert_eq!(
            primary(&compare(&before, &after, false)).verdict,
            Verdict::NotEnoughRuns
        );
    }

    #[test]
    fn different_conditions_are_not_compared() {
        let before = [run(20.0, 100.0), run(20.0, 100.0), run(20.0, 100.0)];
        let mut after = [run(10.0, 200.0), run(10.0, 200.0), run(10.0, 200.0)];
        after[1].0.driver = Some("621.14".into());
        let Comparison::NotComparable { reasons } = compare(&before, &after, false) else {
            panic!("should refuse");
        };
        assert!(reasons[0].contains("driver"), "{reasons:?}");

        let mut after = [run(10.0, 200.0), run(10.0, 200.0), run(10.0, 200.0)];
        after[0].1.present_mode = Some("Composed: Flip".into());
        let Comparison::NotComparable { reasons } = compare(&before, &after, false) else {
            panic!("should refuse");
        };
        assert!(reasons[0].contains("present mode"), "{reasons:?}");
    }

    #[test]
    fn frame_generation_on_one_side_is_not_compared() {
        let before = [run(20.0, 100.0), run(20.0, 100.0), run(20.0, 100.0)];
        let mut after = [run(10.0, 200.0), run(10.0, 200.0), run(10.0, 200.0)];
        for r in &mut after {
            r.1.generated_frames = 5000;
        }
        let Comparison::NotComparable { reasons } = compare(&before, &after, false) else {
            panic!("should refuse");
        };
        assert!(reasons[0].contains("Frame generation"), "{reasons:?}");
    }

    #[test]
    fn a_game_pinned_at_a_cap_is_flagged() {
        let pinned = |mut r: (Fingerprint, RunMetrics)| {
            r.1.frame_time_sd_ms = 0.01;
            r
        };
        let runs: Vec<_> = (0..3).map(|_| pinned(run(8.4, 120.0))).collect();
        let Comparison::Compared { saturated, .. } = compare(&runs, &runs, false) else {
            panic!();
        };
        assert!(saturated);
        let varied = [run(20.0, 100.0), run(20.0, 100.0), run(20.0, 100.0)];
        let Comparison::Compared { saturated, .. } = compare(&varied, &varied, false) else {
            panic!();
        };
        assert!(!saturated);
    }

    #[test]
    fn latency_appears_only_when_both_sides_have_it() {
        let before = [run(20.0, 100.0), run(20.0, 100.0), run(20.0, 100.0)];
        let mut after = before.clone();
        let Comparison::Compared { findings, .. } = compare(&before, &after, false) else {
            panic!();
        };
        assert!(!findings.iter().any(|f| f.metric == MetricKind::PcLatency));
        for r in &mut after {
            r.1.pc_latency_ms = Some(20.0);
        }
        let Comparison::Compared { findings, .. } = compare(&before, &after, false) else {
            panic!();
        };
        assert!(!findings.iter().any(|f| f.metric == MetricKind::PcLatency));
    }

    #[test]
    fn pairing_finds_an_effect_that_drift_would_hide() {
        // Drift across the session swamps the effect for an unpaired test, but pairs cancel it.
        let before: Vec<_> = [100.0, 105.0, 110.0, 115.0, 120.0]
            .iter()
            .map(|p| run(*p, 60.0))
            .collect();
        let after: Vec<_> = [98.0, 102.9, 107.7, 112.8, 117.5]
            .iter()
            .map(|p| run(*p, 60.0))
            .collect();
        let unpaired = primary(&compare(&before, &after, false));
        assert!(
            matches!(unpaired.verdict, Verdict::NoDetectableEffect { .. }),
            "{unpaired:?}"
        );
        let paired = primary(&compare(&before, &after, true));
        assert_eq!(paired.verdict, Verdict::Improved, "{paired:?}");
    }

    #[test]
    fn many_secondary_tests_need_stronger_evidence() {
        // One borderline secondary metric among several doesn't pass after correction.
        let mut before = vec![
            run(20.0, 100.0),
            run(20.3, 101.0),
            run(19.7, 99.0),
            run(20.1, 100.5),
        ];
        let mut after = vec![
            run(20.05, 100.0),
            run(20.2, 101.5),
            run(19.8, 98.5),
            run(20.15, 100.0),
        ];
        for (i, r) in before.iter_mut().enumerate() {
            r.1.frame_time_sd_ms = 2.00 + i as f64 * 0.02;
        }
        for (i, r) in after.iter_mut().enumerate() {
            r.1.frame_time_sd_ms = 2.05 + i as f64 * 0.02;
        }
        let Comparison::Compared { findings, .. } = compare(&before, &after, false) else {
            panic!();
        };
        let sd = findings
            .iter()
            .find(|f| f.metric == MetricKind::FrameTimeSd)
            .unwrap();
        assert!(sd.p_value.unwrap() < 0.2, "{sd:?}");
        assert!(
            matches!(sd.verdict, Verdict::NoDetectableEffect { .. }),
            "{sd:?}"
        );
    }
}
