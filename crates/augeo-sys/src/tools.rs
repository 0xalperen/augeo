//! The timer tool and the polling meter. Both only measure, apart from the timer tool's hold, which
//! lasts only while this program asks for it and ends by itself when the program exits.

use std::{
    cell::RefCell,
    ffi::c_void,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Devices::HumanInterfaceDevice::{HID_USAGE_GENERIC_MOUSE, HID_USAGE_PAGE_GENERIC},
        Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::{
            BeginPaint, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreateSolidBrush,
            DEFAULT_CHARSET, DT_CENTER, DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW,
            EndPaint, FF_DONTCARE, FW_NORMAL, FW_SEMIBOLD, FillRect, HDC, InvalidateRect,
            OUT_DEFAULT_PRECIS, PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
        },
        System::LibraryLoader::{GetModuleHandleW, GetProcAddress},
        UI::{
            Input::{
                GetRawInputData, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RID_INPUT,
                RIDEV_REMOVE, RIM_TYPEMOUSE, RegisterRawInputDevices,
            },
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect,
                GetMessageW, GetSystemMetrics, IDC_ARROW, LoadCursorW, LoadIconW, MSG,
                PostQuitMessage, RegisterClassW, SM_CXSCREEN, SM_CYSCREEN, SW_SHOWNORMAL,
                SetForegroundWindow, SetTimer, ShowWindow, WM_ACTIVATE, WM_DESTROY, WM_ERASEBKGND,
                WM_INPUT, WM_KEYDOWN, WM_LBUTTONDOWN, WM_PAINT, WM_TIMER, WNDCLASSW, WS_CAPTION,
                WS_EX_TOPMOST, WS_OVERLAPPED, WS_SYSMENU,
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

const METER_CLASS: &str = "augeoPollingMeter";
const VK_ESCAPE_CODE: usize = 0x1B;
const METER_TIMER_MS: u32 = 100;

/// What the polling test has done.
#[derive(Debug, Clone, PartialEq)]
pub enum MeterEvent {
    /// The user clicked, and testing began.
    Started,
    /// When each report arrived, in milliseconds, or why the test couldn't finish.
    Finished(Result<Vec<f64>, String>),
    /// The window was closed.
    Cancelled,
}

/// A running polling test, in a small window of its own.
///
/// It has to be a real window the user clicks. Since July 2023, Windows 11 limits raw mouse input to
/// programs in the background to about 125 messages a second, so a hidden listener reads 125 Hz
/// whatever the mouse really does. A terminal window belongs to another process, so this one is the
/// only window of augeo's that can be in front.
#[derive(Debug)]
pub struct PollingMeter {
    events: Receiver<MeterEvent>,
    cancel: Arc<AtomicBool>,
}

impl PollingMeter {
    /// Opens the window. Testing lasts `seconds` from the user's first click inside it.
    #[must_use]
    pub fn start(seconds: u64) -> Self {
        let (send, events) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let thread_cancel = Arc::clone(&cancel);
        // Windows and their messages belong to the thread that made them.
        std::thread::spawn(move || {
            if let Err(e) = run_meter_window(seconds, send.clone(), thread_cancel) {
                let _ = send.send(MeterEvent::Finished(Err(e)));
            }
        });
        Self { events, cancel }
    }

    /// The next thing that happened, if anything did.
    #[must_use]
    pub fn poll(&self) -> Option<MeterEvent> {
        self.events.try_recv().ok()
    }

    /// Closes the window, and drops any result.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Drop for PollingMeter {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// A snapshot for painting, so nothing is borrowed while Windows calls back in.
#[derive(Debug, Clone, Copy)]
struct MeterView {
    seconds: u64,
    elapsed: Option<Duration>,
    reports_last_second: usize,
}

#[derive(Debug)]
struct MeterState {
    seconds: u64,
    started: Option<Instant>,
    times: Vec<f64>,
    events: Sender<MeterEvent>,
    cancel: Arc<AtomicBool>,
    done: bool,
}

impl MeterState {
    fn view(&self) -> MeterView {
        let elapsed = self.started.map(|s| s.elapsed());
        let now_ms = elapsed.map_or(0.0, |e| e.as_secs_f64() * 1000.0);
        MeterView {
            seconds: self.seconds,
            elapsed,
            reports_last_second: self
                .times
                .iter()
                .rev()
                .take_while(|t| **t > now_ms - 1000.0)
                .count(),
        }
    }

    fn finish(&mut self, result: Result<Vec<f64>, String>) {
        self.done = true;
        let _ = self.events.send(MeterEvent::Finished(result));
    }

    /// Whether the window should now close.
    fn on_timer(&mut self) -> bool {
        if self.cancel.load(Ordering::Relaxed) {
            self.done = true;
            return true;
        }
        if self
            .started
            .is_some_and(|s| s.elapsed() >= Duration::from_secs(self.seconds))
        {
            let times = std::mem::take(&mut self.times);
            self.finish(Ok(times));
            return true;
        }
        false
    }
}

thread_local! {
    static METER: RefCell<Option<MeterState>> = const { RefCell::new(None) };
}

/// Windows can call the window procedure again from inside a call it makes, so this must not be held
/// across anything that sends a message. If it's already held, nothing happens rather than a panic.
fn with_meter<R>(action: impl FnOnce(&mut MeterState) -> R) -> Option<R> {
    METER.with(|meter| {
        meter
            .try_borrow_mut()
            .ok()
            .and_then(|mut state| state.as_mut().map(action))
    })
}

fn repaint(window: HWND) {
    // SAFETY: the window is alive, as this runs inside its own window procedure.
    let _ = unsafe { InvalidateRect(Some(window), None, false) };
}

fn close(window: HWND) {
    // SAFETY: the window belongs to this thread, and isn't used after it's destroyed.
    let _ = unsafe { DestroyWindow(window) };
}

fn record_report(raw_handle: LPARAM) {
    let mut raw = RAWINPUT::default();
    let mut size = size_of::<RAWINPUT>() as u32;
    // SAFETY: the buffer and size match, and the handle came with this message.
    let copied = unsafe {
        GetRawInputData(
            HRAWINPUT(raw_handle.0 as *mut c_void),
            RID_INPUT,
            Some(std::ptr::addr_of_mut!(raw).cast()),
            &mut size,
            size_of::<RAWINPUTHEADER>() as u32,
        )
    };
    if copied != u32::MAX && raw.header.dwType == RIM_TYPEMOUSE.0 {
        with_meter(|m| {
            if let Some(started) = m.started {
                m.times.push(started.elapsed().as_secs_f64() * 1000.0);
            }
        });
    }
}

const fn colour(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | ((g as u32) << 8) | ((b as u32) << 16))
}

fn fill(dc: HDC, area: &RECT, color: COLORREF) {
    // SAFETY: the device context is valid while painting, and the brush is ours and freed here.
    unsafe {
        let brush = CreateSolidBrush(color);
        FillRect(dc, area, brush);
        let _ = DeleteObject(brush.into());
    }
}

fn draw_line(dc: HDC, text: &str, area: &RECT, height: i32, semibold: bool, color: COLORREF) {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    let face = HSTRING::from("Segoe UI");
    let weight = if semibold { FW_SEMIBOLD } else { FW_NORMAL };
    let mut area = *area;
    // SAFETY: the device context is valid while painting. The font is selected only for this call,
    // then the old one is put back and the new one freed. The text buffer outlives the draw.
    unsafe {
        let font = CreateFontW(
            height,
            0,
            0,
            0,
            weight.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            FF_DONTCARE.0.into(),
            &face,
        );
        let previous = SelectObject(dc, font.into());
        SetTextColor(dc, color);
        DrawTextW(
            dc,
            &mut wide,
            &mut area,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
        SelectObject(dc, previous);
        let _ = DeleteObject(font.into());
    }
}

/// The same colours as the terminal screens.
fn paint_meter(window: HWND, view: MeterView) {
    const BASE: COLORREF = colour(25, 23, 36);
    const TRACK: COLORREF = colour(57, 53, 82);
    const TEXT: COLORREF = colour(224, 222, 244);
    const MUTED: COLORREF = colour(144, 140, 170);
    const IRIS: COLORREF = colour(196, 167, 231);
    const PINK: COLORREF = colour(235, 111, 146);

    let mut paint = PAINTSTRUCT::default();
    let mut client = RECT::default();
    // SAFETY: this runs while handling this window's WM_PAINT, and both structures are locals.
    let dc = unsafe {
        let _ = GetClientRect(window, &mut client);
        BeginPaint(window, &mut paint)
    };
    fill(dc, &client, BASE);
    // SAFETY: the device context stays valid until EndPaint below.
    unsafe { SetBkMode(dc, TRANSPARENT) };

    let width = client.right;
    let line = |top: i32, bottom: i32| RECT {
        left: 16,
        top,
        right: width - 16,
        bottom,
    };
    match view.elapsed {
        None => {
            draw_line(dc, "Click here to start", &line(40, 90), 34, true, IRIS);
            draw_line(
                dc,
                &format!(
                    "Then keep moving your mouse quickly in circles for {} seconds.",
                    view.seconds
                ),
                &line(100, 130),
                19,
                false,
                TEXT,
            );
            draw_line(dc, "Esc cancels", &line(150, 180), 16, false, MUTED);
        }
        Some(elapsed) => {
            let total = view.seconds as f64;
            let left = (total - elapsed.as_secs_f64()).max(0.0).ceil();
            draw_line(
                dc,
                &format!("Testing: keep moving. {left:.0}s left"),
                &line(30, 80),
                30,
                true,
                PINK,
            );
            let bar = line(95, 111);
            fill(dc, &bar, TRACK);
            let done = (elapsed.as_secs_f64() / total).clamp(0.0, 1.0);
            let filled = RECT {
                right: bar.left + (f64::from(bar.right - bar.left) * done) as i32,
                ..bar
            };
            fill(dc, &filled, IRIS);
            draw_line(
                dc,
                &format!("{} reports in the last second", view.reports_last_second),
                &line(125, 155),
                19,
                false,
                TEXT,
            );
        }
    }
    // SAFETY: this pairs with the BeginPaint above.
    let _ = unsafe { EndPaint(window, &paint) };
}

extern "system" fn meter_proc(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match message {
        WM_INPUT => {
            record_report(l);
            // Windows needs this to clean up after raw input.
            // SAFETY: this passes along exactly what Windows gave this window procedure.
            unsafe { DefWindowProcW(window, message, w, l) }
        }
        WM_LBUTTONDOWN => {
            with_meter(|m| {
                if m.started.is_none() {
                    m.started = Some(Instant::now());
                    let _ = m.events.send(MeterEvent::Started);
                }
            });
            repaint(window);
            LRESULT(0)
        }
        WM_TIMER => {
            if with_meter(MeterState::on_timer) == Some(true) {
                close(window);
            } else {
                repaint(window);
            }
            LRESULT(0)
        }
        // Losing focus would bring the background limit back, so the numbers couldn't be trusted.
        WM_ACTIVATE if w.0 & 0xFFFF == 0 => {
            let abandoned = with_meter(|m| {
                let testing = m.started.is_some() && !m.done;
                if testing {
                    m.finish(Err(
                        "The test window lost focus, so the result couldn't be trusted. Please \
                         try again, and keep the window in front."
                            .to_string(),
                    ));
                }
                testing
            });
            if abandoned == Some(true) {
                close(window);
            }
            LRESULT(0)
        }
        WM_KEYDOWN if w.0 == VK_ESCAPE_CODE => {
            close(window);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            if let Some(view) = with_meter(|m| m.view()) {
                paint_meter(window, view);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            with_meter(|m| {
                if !m.done {
                    m.done = true;
                    let _ = m.events.send(MeterEvent::Cancelled);
                }
            });
            let device = RAWINPUTDEVICE {
                usUsagePage: HID_USAGE_PAGE_GENERIC,
                usUsage: HID_USAGE_GENERIC_MOUSE,
                dwFlags: RIDEV_REMOVE,
                hwndTarget: HWND::default(),
            };
            // SAFETY: the device array outlives the call, and its size is passed correctly. Quitting
            // only posts a message to this thread's own loop.
            unsafe {
                let _ = RegisterRawInputDevices(&[device], size_of::<RAWINPUTDEVICE>() as u32);
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        // SAFETY: this passes along exactly what Windows gave this window procedure.
        _ => unsafe { DefWindowProcW(window, message, w, l) },
    }
}

fn run_meter_window(
    seconds: u64,
    events: Sender<MeterEvent>,
    cancel: Arc<AtomicBool>,
) -> Result<(), String> {
    const WIDTH: i32 = 560;
    const HEIGHT: i32 = 240;

    METER.with(|meter| {
        *meter.borrow_mut() = Some(MeterState {
            seconds,
            started: None,
            times: Vec::new(),
            events,
            cancel,
            done: false,
        });
    });

    let class_name = HSTRING::from(METER_CLASS);
    let title = HSTRING::from("augeo: mouse polling test");
    // SAFETY: a null name asks for this program's own module, which stays loaded.
    let module = unsafe { GetModuleHandleW(None) }.map_err(|e| e.to_string())?;
    let class = WNDCLASSW {
        lpfnWndProc: Some(meter_proc),
        hInstance: module.into(),
        // SAFETY: this loads a cursor every Windows has, shared by the whole system.
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
        // SAFETY: icon 1 in this program's own resources, passed by number as the API expects. A test
        // program has no icon, and Windows then uses a plain one.
        hIcon: unsafe { LoadIconW(Some(module.into()), PCWSTR(std::ptr::without_provenance(1))) }
            .unwrap_or_default(),
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };
    // SAFETY: the class structure and its name outlive the call. Registering twice just fails, and
    // creating the window below still works with the first registration.
    unsafe { RegisterClassW(&class) };
    // SAFETY: these only read the size of the screen.
    let (screen_width, screen_height) =
        unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    // SAFETY: the class was registered above, and no parameters are passed through the pointer.
    // It stays on top, because Windows may open a background program's window behind the terminal.
    let window = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST,
            &class_name,
            &title,
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
            (screen_width - WIDTH) / 2,
            (screen_height - HEIGHT) / 2,
            WIDTH,
            HEIGHT,
            None,
            None,
            Some(module.into()),
            None,
        )
    }
    .map_err(|e| format!("opening the test window: {e}"))?;

    let device = RAWINPUTDEVICE {
        usUsagePage: HID_USAGE_PAGE_GENERIC,
        usUsage: HID_USAGE_GENERIC_MOUSE,
        dwFlags: Default::default(),
        hwndTarget: window,
    };
    // SAFETY: the device array outlives the call, and its size is passed correctly.
    if let Err(e) =
        unsafe { RegisterRawInputDevices(&[device], size_of::<RAWINPUTDEVICE>() as u32) }
    {
        close(window);
        return Err(format!("listening to the mouse: {e}"));
    }
    // SAFETY: the window belongs to this thread. Whether Windows lets it come to the front doesn't
    // matter, since the click that starts the test does that anyway.
    unsafe {
        let _ = ShowWindow(window, SW_SHOWNORMAL);
        let _ = SetForegroundWindow(window);
        SetTimer(Some(window), 1, METER_TIMER_MS, None);
    }

    let mut message = MSG::default();
    // SAFETY: `message` is a valid place for the result. The loop ends when the window is destroyed.
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {
        // SAFETY: the message came from GetMessageW just above.
        unsafe { DispatchMessageW(&message) };
    }
    Ok(())
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

    /// Only one test window at a time, since they're found by class name.
    static WINDOWS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn find_meter_window() -> HWND {
        let class = HSTRING::from(METER_CLASS);
        for _ in 0..500 {
            // SAFETY: the class name outlives the call.
            if let Ok(window) = unsafe {
                windows::Win32::UI::WindowsAndMessaging::FindWindowW(&class, PCWSTR::null())
            } {
                return window;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the test window never opened");
    }

    fn next_event(meter: &PollingMeter) -> MeterEvent {
        for _ in 0..1000 {
            if let Some(event) = meter.poll() {
                return event;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the meter went quiet");
    }

    #[test]
    fn the_meter_waits_for_a_click_and_then_runs_for_its_time() {
        let _one_at_a_time = WINDOWS.lock().unwrap();
        let meter = PollingMeter::start(1);
        let window = find_meter_window();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(meter.poll(), None, "it must not start by itself");

        let clicked = Instant::now();
        // SAFETY: the window is alive, and this message carries no pointers.
        unsafe {
            windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                Some(window),
                WM_LBUTTONDOWN,
                WPARAM(0),
                LPARAM(0),
            )
        }
        .unwrap();
        assert_eq!(next_event(&meter), MeterEvent::Started);
        assert!(matches!(next_event(&meter), MeterEvent::Finished(Ok(_))));
        assert!(clicked.elapsed() >= Duration::from_secs(1));
    }

    #[test]
    fn cancelling_closes_the_window_without_a_result() {
        let _one_at_a_time = WINDOWS.lock().unwrap();
        let meter = PollingMeter::start(60);
        let _ = find_meter_window();
        meter.cancel();
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(meter.poll(), None);
        let class = HSTRING::from(METER_CLASS);
        // SAFETY: the class name outlives the call.
        let found =
            unsafe { windows::Win32::UI::WindowsAndMessaging::FindWindowW(&class, PCWSTR::null()) };
        assert!(found.is_err(), "the window should be gone");
    }

    #[test]
    fn closing_the_window_says_so() {
        let _one_at_a_time = WINDOWS.lock().unwrap();
        let meter = PollingMeter::start(60);
        let window = find_meter_window();
        // SAFETY: the window is alive, and this message carries no pointers.
        unsafe {
            windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                Some(window),
                WM_KEYDOWN,
                WPARAM(VK_ESCAPE_CODE),
                LPARAM(0),
            )
        }
        .unwrap();
        assert_eq!(next_event(&meter), MeterEvent::Cancelled);
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
