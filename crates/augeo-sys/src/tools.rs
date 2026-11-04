//! The timer tool and the polling meter. Both only measure, apart from the timer tool's hold, which
//! lasts only while this program asks for it and ends by itself when the program exits.

use std::{
    ffi::c_void,
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Devices::HumanInterfaceDevice::{HID_USAGE_GENERIC_MOUSE, HID_USAGE_PAGE_GENERIC},
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        System::LibraryLoader::{GetModuleHandleW, GetProcAddress},
        UI::{
            Input::{
                GetRawInputData, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RID_INPUT,
                RIDEV_INPUTSINK, RIM_TYPEMOUSE, RegisterRawInputDevices,
            },
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, HWND_MESSAGE, MSG, PM_REMOVE,
                PeekMessageW, RegisterClassW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_INPUT, WNDCLASSW,
            },
        },
    },
    core::{HSTRING, PCWSTR, s},
};

/// Timer resolutions, in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimerResolution {
    pub coarsest_ms: f64,
    pub finest_ms: f64,
    pub current_ms: f64,
}

type QueryFn = unsafe extern "system" fn(*mut u32, *mut u32, *mut u32) -> i32;
type SetFn = unsafe extern "system" fn(u32, u8, *mut u32) -> i32;

fn ntdll_function(
    name: windows::core::PCSTR,
) -> Result<unsafe extern "system" fn() -> isize, String> {
    let module_name = HSTRING::from("ntdll.dll");
    // SAFETY: ntdll is always loaded, and the name outlives the call.
    let module = unsafe { GetModuleHandleW(&module_name) }.map_err(|e| e.to_string())?;
    // SAFETY: the module is valid, and the name is a terminated constant.
    unsafe { GetProcAddress(module, name) }
        .ok_or_else(|| "ntdll is missing a timer function".to_string())
}

/// Reads the timer resolution ranges, through the same call the common tools use.
pub fn timer_resolution() -> Result<TimerResolution, String> {
    let function = ntdll_function(s!("NtQueryTimerResolution"))?;
    // SAFETY: this signature matches the function looked up above.
    let query: QueryFn = unsafe { std::mem::transmute(function) };
    let (mut coarsest, mut finest, mut current) = (0u32, 0u32, 0u32);
    // SAFETY: all three pointers refer to locals that outlive the call.
    let status = unsafe { query(&mut coarsest, &mut finest, &mut current) };
    if status < 0 {
        return Err(format!("reading the timer resolution failed ({status:#x})"));
    }
    let ms = |units: u32| f64::from(units) / 10_000.0;
    Ok(TimerResolution {
        coarsest_ms: ms(coarsest),
        finest_ms: ms(finest),
        current_ms: ms(current),
    })
}

/// Asks for a timer resolution for this process, or releases the request. Windows drops it when the
/// process exits, so nothing is left behind.
pub fn hold_timer_resolution(milliseconds: f64, hold: bool) -> Result<(), String> {
    let function = ntdll_function(s!("NtSetTimerResolution"))?;
    // SAFETY: this signature matches the function looked up above.
    let set: SetFn = unsafe { std::mem::transmute(function) };
    let mut actual = 0u32;
    // SAFETY: the pointer refers to a local that outlives the call.
    let status = unsafe {
        set(
            (milliseconds * 10_000.0) as u32,
            u8::from(hold),
            &mut actual,
        )
    };
    if status < 0 {
        return Err(format!("setting the timer resolution failed ({status:#x})"));
    }
    Ok(())
}

/// How long `Sleep(1)` really takes on average, in milliseconds. This blocks for a moment.
pub fn measure_sleep_ms(samples: u32) -> f64 {
    let start = Instant::now();
    for _ in 0..samples {
        std::thread::sleep(Duration::from_millis(1));
    }
    start.elapsed().as_secs_f64() * 1000.0 / f64::from(samples.max(1))
}

const POLLING_RATES: [u32; 7] = [125, 250, 500, 1000, 2000, 4000, 8000];
const BUCKET: Duration = Duration::from_millis(100);

/// What the polling meter found.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Polling {
    pub measured_hz: f64,
    pub nearest_standard_hz: u32,
    pub events: usize,
}

/// Works out a rate from when mouse events arrived. Only stretches of steady movement count, because
/// a mouse sends nothing when still. The busiest buckets show what it can really do.
pub fn polling_from_times(times_ms: &[f64]) -> Option<Polling> {
    let bucket_ms = BUCKET.as_secs_f64() * 1000.0;
    let last = *times_ms.last()?;
    let first = *times_ms.first()?;
    let count = ((last - first) / bucket_ms) as usize + 1;
    let mut buckets = vec![0u32; count];
    for t in times_ms {
        buckets[((t - first) / bucket_ms) as usize] += 1;
    }
    let mut active: Vec<u32> = buckets.into_iter().filter(|c| *c >= 5).collect();
    if active.len() < 3 {
        return None;
    }
    active.sort_unstable();
    // The busiest tenth of buckets, where movement was fast enough for every poll to carry a report.
    // A larger share would understate the rate when most of the movement was slow.
    let top = &active[active.len() * 9 / 10..];
    let per_bucket = f64::from(top.iter().sum::<u32>()) / top.len() as f64;
    let measured_hz = per_bucket * 1000.0 / bucket_ms;
    let nearest = POLLING_RATES
        .into_iter()
        .min_by_key(|rate| (f64::from(*rate) - measured_hz).abs() as u32)?;
    Some(Polling {
        measured_hz,
        nearest_standard_hz: nearest,
        events: times_ms.len(),
    })
}

extern "system" fn window_proc(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: this passes along exactly what Windows gave this window procedure.
    unsafe { DefWindowProcW(window, message, w, l) }
}

fn record_mouse_times(seconds: u64) -> Result<Vec<f64>, String> {
    let class_name = HSTRING::from("augeoPollingMeter");
    let class = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };
    // SAFETY: the class structure and its name outlive the call. Registering twice just fails, and
    // creating the window below still works with the first registration.
    unsafe { RegisterClassW(&class) };
    // SAFETY: a message-only window with the class registered above. No parameters are passed.
    let window = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            &class_name,
            &class_name,
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            None,
            None,
        )
    }
    .map_err(|e| format!("creating the meter window: {e}"))?;

    let device = RAWINPUTDEVICE {
        usUsagePage: HID_USAGE_PAGE_GENERIC,
        usUsage: HID_USAGE_GENERIC_MOUSE,
        dwFlags: RIDEV_INPUTSINK,
        hwndTarget: window,
    };
    // SAFETY: the device array outlives the call, and its size is passed correctly.
    unsafe { RegisterRawInputDevices(&[device], size_of::<RAWINPUTDEVICE>() as u32) }
        .map_err(|e| format!("listening to the mouse: {e}"))?;

    let start = Instant::now();
    let end = start + Duration::from_secs(seconds);
    let mut times = Vec::new();
    let mut message = MSG::default();
    while Instant::now() < end {
        // SAFETY: `message` is a valid place for the result, and the window is ours.
        let got = unsafe { PeekMessageW(&mut message, Some(window), 0, 0, PM_REMOVE) };
        if !got.as_bool() {
            std::thread::sleep(Duration::from_micros(100));
            continue;
        }
        if message.message != WM_INPUT {
            continue;
        }
        let mut raw = RAWINPUT::default();
        let mut size = size_of::<RAWINPUT>() as u32;
        // SAFETY: the buffer and size match, and the handle came with this message.
        let copied = unsafe {
            GetRawInputData(
                HRAWINPUT(message.lParam.0 as *mut c_void),
                RID_INPUT,
                Some(std::ptr::addr_of_mut!(raw).cast()),
                &mut size,
                size_of::<RAWINPUTHEADER>() as u32,
            )
        };
        if copied != u32::MAX && raw.header.dwType == RIM_TYPEMOUSE.0 {
            times.push(start.elapsed().as_secs_f64() * 1000.0);
        }
    }
    // SAFETY: the window is ours and is no longer used.
    let _ = unsafe { DestroyWindow(window) };
    Ok(times)
}

/// Starts measuring on its own thread, because raw input needs a message loop.
pub fn start_polling_meter(seconds: u64) -> Receiver<Result<Vec<f64>, String>> {
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = send.send(record_mouse_times(seconds));
    });
    receive
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Evenly spaced events at this rate, in bursts as a hand would make, with pauses between.
    fn times(hz: f64, seconds: f64) -> Vec<f64> {
        let step = 1000.0 / hz;
        let mut out = Vec::new();
        let mut t = 0.0;
        while t < seconds * 1000.0 {
            // Half a second of movement in each second.
            if (t % 1000.0) < 500.0 {
                out.push(t);
            }
            t += step;
        }
        out
    }

    #[test]
    fn standard_rates_are_recognised() {
        for rate in [125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0] {
            let polling = polling_from_times(&times(rate, 5.0)).unwrap();
            assert_eq!(f64::from(polling.nearest_standard_hz), rate, "{polling:?}");
            assert!(
                (polling.measured_hz - rate).abs() / rate < 0.1,
                "{polling:?}"
            );
        }
    }

    #[test]
    fn slow_movement_does_not_understate_the_rate() {
        // Mostly slow movement with some fast bursts: the fast bursts show the true rate.
        let mut samples = Vec::new();
        for i in 0..200 {
            samples.push(f64::from(i) * 16.0);
        }
        for i in 0..500 {
            samples.push(3200.0 + f64::from(i));
        }
        let polling = polling_from_times(&samples).unwrap();
        assert_eq!(polling.nearest_standard_hz, 1000, "{polling:?}");
    }

    #[test]
    fn too_little_movement_gives_no_answer() {
        assert_eq!(polling_from_times(&[]), None);
        assert_eq!(polling_from_times(&[1.0, 2.0, 3.0]), None);
        // A mouse barely moved over a long time.
        let sparse: Vec<f64> = (0..20).map(|i| f64::from(i) * 500.0).collect();
        assert_eq!(polling_from_times(&sparse), None);
    }

    #[test]
    fn the_timer_can_be_read_and_holding_works() {
        let before = timer_resolution().unwrap();
        assert!(
            before.finest_ms > 0.0 && before.finest_ms <= before.coarsest_ms,
            "{before:?}"
        );
        hold_timer_resolution(before.finest_ms.max(0.5), true).unwrap();
        let held = timer_resolution().unwrap();
        assert!(held.current_ms <= before.coarsest_ms, "{held:?}");
        hold_timer_resolution(before.finest_ms.max(0.5), false).unwrap();
    }

    #[test]
    fn sleep_measurement_is_sensible() {
        let ms = measure_sleep_ms(5);
        assert!((0.5..100.0).contains(&ms), "{ms}");
    }
}
