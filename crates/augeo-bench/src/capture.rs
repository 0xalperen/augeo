//! Reads a PresentMon CSV. Column names and availability vary by version and by game, so columns are
//! found by name, `NA` means missing, and a missing latency column reads "not available", never zero.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Frames are dropped from the start of a run, because the first seconds are often unrepresentative.
const TRIM_START_SECONDS: f64 = 5.0;
/// 0.1% lows from fewer frames than this rest on a handful of frames, so they aren't reported.
const MIN_FRAMES_FOR_LOW_01: usize = 10_000;
/// A gap this long means the game wasn't rendering, such as alt-tabbing away or a loading screen.
const GAP_MS: f64 = 1000.0;
/// A run with more than this fraction of its time in gaps isn't representative.
const MAX_GAP_FRACTION: f64 = 0.05;

/// What a capture contains, after choosing the game's main swap chain.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Capture {
    pub(crate) frame_ms: Vec<f64>,
    pub(crate) present_mode: Option<String>,
    pub(crate) swap_chains: usize,
    pub(crate) generated_frames: usize,
    pub(crate) latency: BTreeMap<String, f64>,
    pub(crate) gap_seconds: f64,
}

/// The numbers from one run.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunMetrics {
    pub frames: usize,
    pub seconds: f64,
    pub avg_fps: f64,
    /// FPS at the 99th percentile frame time.
    pub low_1_percentile_fps: f64,
    /// Average FPS over the slowest 1% of frames.
    pub low_1_average_fps: f64,
    /// Average FPS over the slowest 0.1% of frames. `None` with too few frames to mean anything.
    pub low_01_average_fps: Option<f64>,
    pub p99_ms: f64,
    pub frame_time_sd_ms: f64,
    pub present_mode: Option<String>,
    /// Frame generation inflates FPS, so comparing runs that differ here is meaningless.
    pub generated_frames: usize,
    pub pc_latency_ms: Option<f64>,
    pub click_to_photon_ms: Option<f64>,
    pub input_to_photon_ms: Option<f64>,
    pub render_present_ms: Option<f64>,
    /// Why this run shouldn't be trusted, if anything.
    pub warning: Option<String>,
}

fn split_line(line: &str) -> Vec<&str> {
    line.split(',').map(str::trim).collect()
}

fn number(text: &str) -> Option<f64> {
    if text.eq_ignore_ascii_case("NA") || text.is_empty() {
        return None;
    }
    text.parse().ok().filter(|v: &f64| v.is_finite())
}

/// Parses a capture, keeping only rows from `exe`.
pub(crate) fn parse_capture(csv: &str, exe: &str) -> Result<Capture, String> {
    let mut lines = csv.lines();
    let header = lines.next().ok_or("the capture is empty")?;
    let columns = split_line(header);
    let find = |name: &str| columns.iter().position(|c| c.eq_ignore_ascii_case(name));
    let application = find("Application").ok_or("the capture has no Application column")?;
    let between = find("MsBetweenPresents")
        .or_else(|| find("FrameTime"))
        .ok_or("the capture has no frame time column")?;
    let swap_chain = find("SwapChainAddress");
    let mode = find("PresentMode");
    let frame_type = find("FrameType");
    let time = find("TimeInMs");
    let latency_columns: Vec<(&str, Option<usize>)> = [
        "MsPCLatency",
        "MsClickToPhotonLatency",
        "MsAllInputToPhotonLatency",
        "MsRenderPresentLatency",
    ]
    .iter()
    .map(|name| (*name, find(name)))
    .collect();

    #[derive(Default)]
    struct Chain {
        frames: Vec<f64>,
        modes: BTreeMap<String, usize>,
        generated: usize,
        latency: BTreeMap<String, (f64, usize)>,
    }
    let mut chains: BTreeMap<String, Chain> = BTreeMap::new();
    let mut first_time: Option<f64> = None;

    for line in lines {
        let fields = split_line(line);
        let Some(app) = fields.get(application) else {
            continue;
        };
        if !app.eq_ignore_ascii_case(exe) {
            continue;
        }
        if let Some(t) = time.and_then(|i| fields.get(i)).and_then(|t| number(t)) {
            let start = *first_time.get_or_insert(t);
            if (t - start) / 1000.0 < TRIM_START_SECONDS {
                continue;
            }
        }
        let key = swap_chain
            .and_then(|i| fields.get(i))
            .map_or_else(String::new, |s| (*s).to_string());
        let chain = chains.entry(key).or_default();
        // Generated frames are counted but aren't rendered frames, so they're not in frame times.
        let generated = frame_type
            .and_then(|i| fields.get(i))
            .is_some_and(|t| !t.is_empty() && !t.eq_ignore_ascii_case("Application"));
        if generated {
            chain.generated += 1;
            continue;
        }
        let Some(ms) = fields.get(between).and_then(|t| number(t)) else {
            continue;
        };
        chain.frames.push(ms);
        if let Some(m) = mode.and_then(|i| fields.get(i)) {
            *chain.modes.entry((*m).to_string()).or_default() += 1;
        }
        for (name, column) in &latency_columns {
            if let Some(value) = column.and_then(|i| fields.get(i)).and_then(|t| number(t)) {
                let entry = chain.latency.entry((*name).to_string()).or_default();
                entry.0 += value;
                entry.1 += 1;
            }
        }
    }

    let swap_chains = chains.len();
    // A game can have helper swap chains, such as launchers or overlays. The busiest is the game.
    let Some(main) = chains.into_values().max_by_key(|c| c.frames.len()) else {
        return Err(format!(
            "no frames from {exe} were captured. Was it running, and rendering?"
        ));
    };
    let present_mode = main
        .modes
        .iter()
        .max_by_key(|(_, count)| **count)
        .map(|(mode, _)| mode.clone());
    let gap_seconds = main.frames.iter().filter(|ms| **ms >= GAP_MS).sum::<f64>() / 1000.0;
    Ok(Capture {
        latency: main
            .latency
            .iter()
            .map(|(k, (sum, n))| (k.clone(), sum / *n as f64))
            .collect(),
        frame_ms: main.frames,
        present_mode,
        swap_chains,
        generated_frames: main.generated,
        gap_seconds,
    })
}

fn average_of_worst(sorted_ascending: &[f64], fraction: f64) -> f64 {
    let count = ((sorted_ascending.len() as f64 * fraction).ceil() as usize)
        .clamp(1, sorted_ascending.len());
    let worst = &sorted_ascending[sorted_ascending.len() - count..];
    worst.iter().sum::<f64>() / count as f64
}

fn percentile(sorted_ascending: &[f64], fraction: f64) -> f64 {
    let index = ((sorted_ascending.len() - 1) as f64 * fraction).round() as usize;
    sorted_ascending[index]
}

/// Turns a capture into a run's numbers.
pub(crate) fn metrics(capture: &Capture) -> Result<RunMetrics, String> {
    let frames = &capture.frame_ms;
    if frames.len() < 100 {
        return Err(format!(
            "only {} frames were captured, which is too few to use",
            frames.len()
        ));
    }
    let mut sorted = frames.clone();
    sorted.sort_by(f64::total_cmp);
    let total_ms: f64 = frames.iter().sum();
    if total_ms <= 0.0 {
        return Err("the frame times add up to nothing".to_string());
    }
    let mean = total_ms / frames.len() as f64;
    let variance = frames.iter().map(|ms| (ms - mean).powi(2)).sum::<f64>() / frames.len() as f64;
    let seconds = total_ms / 1000.0;
    let fps = |ms: f64| if ms > 0.0 { 1000.0 / ms } else { 0.0 };

    let warning = if capture.gap_seconds / seconds > MAX_GAP_FRACTION {
        Some(format!(
            "{:.1} seconds of this run had no frames. It may include alt-tabbing or a loading screen.",
            capture.gap_seconds
        ))
    } else {
        None
    };
    let latency = |name: &str| capture.latency.get(name).copied();
    Ok(RunMetrics {
        frames: frames.len(),
        seconds,
        // Frames over total time. Averaging per-frame FPS would overstate it.
        avg_fps: frames.len() as f64 / seconds,
        low_1_percentile_fps: fps(percentile(&sorted, 0.99)),
        low_1_average_fps: fps(average_of_worst(&sorted, 0.01)),
        low_01_average_fps: (frames.len() >= MIN_FRAMES_FOR_LOW_01)
            .then(|| fps(average_of_worst(&sorted, 0.001))),
        p99_ms: percentile(&sorted, 0.99),
        frame_time_sd_ms: variance.sqrt(),
        present_mode: capture.present_mode.clone(),
        generated_frames: capture.generated_frames,
        pc_latency_ms: latency("MsPCLatency"),
        click_to_photon_ms: latency("MsClickToPhotonLatency"),
        input_to_photon_ms: latency("MsAllInputToPhotonLatency"),
        render_present_ms: latency("MsRenderPresentLatency"),
        warning,
    })
}

/// Parses a capture and works out its numbers.
pub fn metrics_from_csv(csv: &str, exe: &str) -> Result<RunMetrics, String> {
    metrics(&parse_capture(csv, exe)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "Application,ProcessID,SwapChainAddress,PresentMode,TimeInMs,MsBetweenPresents,MsRenderPresentLatency,MsPCLatency,MsClickToPhotonLatency";

    fn row(app: &str, chain: &str, time: f64, ms: f64) -> String {
        format!("{app},1,{chain},Hardware: Independent Flip,{time},{ms},1.5,20.0,NA")
    }

    /// `count` frames at `ms` each, after the trimmed first seconds.
    fn csv(app: &str, count: usize, ms: f64) -> String {
        let mut text = String::from(HEADER);
        text.push('\n');
        text.push_str(&row(app, "0x1", 0.0, ms));
        for i in 0..count {
            text.push('\n');
            text.push_str(&row(app, "0x1", 6000.0 + i as f64 * ms, ms));
        }
        text
    }

    #[test]
    fn steady_frames_give_exact_numbers() {
        let m = metrics_from_csv(&csv("game.exe", 1000, 10.0), "game.exe").unwrap();
        assert_eq!(m.frames, 1000);
        assert!((m.avg_fps - 100.0).abs() < 1e-9);
        assert!((m.low_1_percentile_fps - 100.0).abs() < 1e-9);
        assert!((m.low_1_average_fps - 100.0).abs() < 1e-9);
        assert!(m.frame_time_sd_ms < 1e-9);
        assert_eq!(
            m.present_mode.as_deref(),
            Some("Hardware: Independent Flip")
        );
    }

    #[test]
    fn lows_reflect_the_slowest_frames_under_both_definitions() {
        // 990 frames at 10 ms, and 10 at 50 ms.
        let mut frames = vec![10.0; 990];
        frames.extend(vec![50.0; 10]);
        let capture = Capture {
            frame_ms: frames,
            ..Capture::default()
        };
        let m = metrics(&capture).unwrap();
        assert!((m.low_1_average_fps - 20.0).abs() < 1e-9, "{m:?}");
        // The percentile falls right at the boundary between the two groups.
        assert!(m.p99_ms == 50.0 || m.p99_ms == 10.0, "{m:?}");
        // Total time is 9900 + 500 ms over 1000 frames.
        assert!((m.avg_fps - 1000.0 / 10.4).abs() < 1e-6, "{m:?}");
    }

    #[test]
    fn low_01_needs_enough_frames() {
        let few = metrics_from_csv(&csv("game.exe", 9_999, 10.0), "game.exe").unwrap();
        assert_eq!(few.low_01_average_fps, None);
        let many = metrics_from_csv(&csv("game.exe", 10_000, 10.0), "game.exe").unwrap();
        assert!(many.low_01_average_fps.is_some());
    }

    #[test]
    fn na_columns_mean_not_available_rather_than_zero() {
        let m = metrics_from_csv(&csv("game.exe", 500, 10.0), "game.exe").unwrap();
        assert_eq!(m.click_to_photon_ms, None);
        assert_eq!(m.input_to_photon_ms, None);
        assert_eq!(m.pc_latency_ms, Some(20.0));
        assert_eq!(m.render_present_ms, Some(1.5));
    }

    #[test]
    fn other_applications_and_the_first_seconds_are_ignored() {
        let mut text = csv("game.exe", 500, 10.0);
        text.push('\n');
        text.push_str(&row("dwm.exe", "0x9", 7000.0, 1.0));
        let m = metrics_from_csv(&text, "GAME.EXE").unwrap();
        assert_eq!(m.frames, 500);
    }

    #[test]
    fn the_busiest_swap_chain_is_the_game() {
        let mut text = csv("game.exe", 500, 10.0);
        for i in 0..50 {
            text.push('\n');
            text.push_str(&row("game.exe", "0x2", 6000.0 + f64::from(i), 99.0));
        }
        let capture = parse_capture(&text, "game.exe").unwrap();
        assert_eq!(capture.swap_chains, 2);
        assert_eq!(metrics(&capture).unwrap().frames, 500);
    }

    #[test]
    fn generated_frames_are_counted_but_not_treated_as_rendered() {
        let text = "Application,TimeInMs,MsBetweenPresents,FrameType\n\
                    game.exe,0,8,Application\n"
            .to_string()
            + &(0..200)
                .map(|i| {
                    let kind = if i % 2 == 0 {
                        "Application"
                    } else {
                        "NvidiaDLSS"
                    };
                    format!("game.exe,{},8,{kind}", 6000 + i * 8)
                })
                .collect::<Vec<_>>()
                .join("\n");
        let m = metrics_from_csv(&text, "game.exe").unwrap();
        assert_eq!(m.frames, 100);
        assert_eq!(m.generated_frames, 100);
    }

    #[test]
    fn long_gaps_raise_a_warning() {
        let mut frames = vec![10.0; 1000];
        frames.push(5000.0);
        let gap = frames.iter().filter(|ms| **ms >= GAP_MS).sum::<f64>() / 1000.0;
        let m = metrics(&Capture {
            frame_ms: frames,
            gap_seconds: gap,
            ..Capture::default()
        })
        .unwrap();
        assert!(m.warning.unwrap().contains("no frames"));
    }

    #[test]
    fn useless_captures_say_why() {
        assert!(metrics_from_csv("", "g.exe").unwrap_err().contains("empty"));
        assert!(
            metrics_from_csv("Foo,Bar\n1,2", "g.exe")
                .unwrap_err()
                .contains("Application")
        );
        assert!(
            metrics_from_csv("Application,Foo\ng.exe,2", "g.exe")
                .unwrap_err()
                .contains("frame time")
        );
        assert!(
            metrics_from_csv(HEADER, "g.exe")
                .unwrap_err()
                .contains("no frames")
        );
        assert!(
            metrics_from_csv(&csv("g.exe", 50, 10.0), "g.exe")
                .unwrap_err()
                .contains("too few")
        );
        // Only "NA" frame times.
        let text = format!("{HEADER}\ng.exe,1,0x1,Mode,6000,NA,NA,NA,NA");
        assert!(metrics_from_csv(&text, "g.exe").is_err());
    }

    #[test]
    fn older_presentmon_column_names_work() {
        let mut text = String::from("Application,FrameTime,CPUBusy,GPUBusy");
        for _ in 0..200 {
            text.push_str("\ngame.exe,16.6,1,1");
        }
        let m = metrics_from_csv(&text, "game.exe").unwrap();
        assert_eq!(m.frames, 200);
    }

    #[test]
    fn a_real_capture_from_presentmon_260_parses() {
        // The columns and row format from a real v2.6.0 capture.
        let header = "Application,ProcessID,SwapChainAddress,PresentRuntime,SyncInterval,PresentFlags,AllowsTearing,PresentMode,TimeInMs,MsBetweenSimulationStart,MsBetweenPresents,MsBetweenDisplayChange,MsInPresentAPI,MsRenderPresentLatency,MsUntilDisplayed,CPUStartTimeInMs,MsBetweenAppStart,MsCPUBusy,MsCPUWait,MsGPULatency,MsGPUTime,MsGPUBusy,MsGPUWait,MsAnimationError,AnimationTime,MsFlipDelay,MsAllInputToPhotonLatency,MsClickToPhotonLatency";
        let mut text = String::from(header);
        // Frames in the first five seconds are trimmed, so the timestamps start at 0 in this row.
        text.push_str("\nclaude.exe,17556,0x266FB2D3760,DXGI,0,0,0,Composed: Flip,0,NA,16.6,16.6,0.1,0.9,7.6,7356.7,16.6,16.5,0.1,16.5,0.7,0.7,0.0,0.2,7356.7,NA,NA,NA");
        for i in 0..300 {
            text.push_str(&format!(
                "\nclaude.exe,17556,0x266FB2D3760,DXGI,0,0,0,Composed: Flip,{},NA,16.6,16.6,0.1,0.9,7.6,7356.7,16.6,16.5,0.1,16.5,0.7,0.7,0.0,0.2,7356.7,NA,NA,NA",
                6000 + i * 17
            ));
        }
        let m = metrics_from_csv(&text, "claude.exe").unwrap();
        assert_eq!(m.frames, 300);
        assert_eq!(m.present_mode.as_deref(), Some("Composed: Flip"));
        assert_eq!(m.pc_latency_ms, None);
        assert!((m.render_present_ms.unwrap() - 0.9).abs() < 1e-9, "{m:?}");
    }
}
