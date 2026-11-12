//! Benchmark hooks, off unless FERRET_GPUI_BENCH_OUT names a file. They write `key=value` lines:
//! when the first frame was drawn (ms after the OS created the process), then, a second later,
//! how long FERRET_GPUI_BENCH_DIR took to list and to show, then frame times while scrolling it
//! 300 px a frame for 3 s. The harness measures memory from outside and closes the window.

use std::fmt::Display;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub struct Bench {
    pub out: PathBuf,
    /// The folder to open a second after the first frame.
    pub dir: Option<String>,
    pub first_frame_seen: bool,
    /// Set when the bench folder's listing arrives: when navigation started, how long listing took.
    pub listed: Option<(Instant, Duration)>,
    pub scroll: Option<Scroll>,
}

pub struct Scroll {
    pub start: Instant,
    pub frames: Vec<Instant>,
    pub y: f32,
}

impl Bench {
    pub fn from_env() -> Option<Self> {
        let out = PathBuf::from(std::env::var_os("FERRET_GPUI_BENCH_OUT")?);
        Some(Self {
            out,
            dir: std::env::var("FERRET_GPUI_BENCH_DIR").ok(),
            first_frame_seen: false,
            listed: None,
            scroll: None,
        })
    }

    pub fn write(&self, key: &str, value: impl Display) {
        write_to(&self.out, key, value);
    }
}

pub fn write_to(out: &PathBuf, key: &str, value: impl Display) {
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(out) {
        let _ = writeln!(f, "{key}={value}");
    }
}

impl Scroll {
    pub fn new() -> Self {
        Self { start: Instant::now(), frames: Vec::with_capacity(1024), y: 0. }
    }

    /// Frame count, then the gaps between frames: mean, median, 95th and 99th percentile, worst,
    /// and how many took longer than 25 ms (a visible hitch at 60 Hz).
    pub fn report(&self) -> Vec<(&'static str, String)> {
        let mut gaps: Vec<f64> = self.frames.windows(2).map(|w| (w[1] - w[0]).as_secs_f64() * 1000.).collect();
        if gaps.is_empty() {
            return vec![("scroll_frames", "0".into())];
        }
        let total: f64 = gaps.iter().sum();
        let mean = total / gaps.len() as f64;
        let over = gaps.iter().filter(|g| **g > 25.).count();
        gaps.sort_by(|a, b| a.total_cmp(b));
        let pct = |p: f64| gaps[((gaps.len() - 1) as f64 * p).round() as usize];
        vec![
            ("scroll_frames", gaps.len().to_string()),
            ("scroll_fps", format!("{:.1}", gaps.len() as f64 / (total / 1000.))),
            ("scroll_mean_ms", format!("{mean:.2}")),
            ("scroll_p50_ms", format!("{:.2}", pct(0.5))),
            ("scroll_p95_ms", format!("{:.2}", pct(0.95))),
            ("scroll_p99_ms", format!("{:.2}", pct(0.99))),
            ("scroll_max_ms", format!("{:.2}", gaps[gaps.len() - 1])),
            ("scroll_over_25ms", over.to_string()),
        ]
    }
}

/// Views drawn, counted for the video benchmark's redraw scope (`video_renders`): the window's
/// root, the sidebar, the file lists, the video.
pub static RENDERS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
pub const ROOT: usize = 0;
pub const SIDEBAR: usize = 1;
pub const FILES: usize = 2;
pub const VIDEO: usize = 3;

pub fn rendered(view: usize) {
    RENDERS[view].fetch_add(1, Ordering::Relaxed);
}

pub fn renders() -> [u64; 4] {
    std::array::from_fn(|i| RENDERS[i].load(Ordering::Relaxed))
}

/// Milliseconds since Windows created this process (the same start a harness measures from).
pub fn ms_since_process_start() -> f64 {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut created, mut exited, mut kernel, mut user, mut now) = (zero, zero, zero, zero, zero);
    // SAFETY: plain out-parameters for the current process's own handle.
    unsafe {
        GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user);
        GetSystemTimePreciseAsFileTime(&mut now);
    }
    let ticks = |f: FILETIME| ((f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64) as f64;
    (ticks(now) - ticks(created)) / 10_000.
}

/// Start-up, timed: `mark` notes a step (ms after Windows created the process), and once the
/// window's first frame is on screen and its first folder is listed they all go to the log in
/// one line (target `ferret::start`), so a slow start says where it went.
static MARKS: std::sync::Mutex<Vec<(&'static str, f64)>> = std::sync::Mutex::new(Vec::new());
const SHOWN: &str = "first frame shown";
pub const LISTED: &str = "first folder listed";

pub fn mark(step: &'static str) {
    let Ok(mut marks) = MARKS.lock() else { return };
    if marks.len() >= 24 || marks.iter().any(|(s, _)| *s == step) {
        return;
    }
    marks.push((step, ms_since_process_start()));
    let has = |step: &str| marks.iter().any(|(s, _)| *s == step);
    if (step == SHOWN || step == LISTED) && has(SHOWN) && has(LISTED) {
        let line: Vec<String> = marks.iter().map(|(s, ms)| format!("{s} {ms:.0}")).collect();
        log::info!(target: "ferret::start", "ms after the process began: {}", line.join(", "));
    }
}

/// Called by each draw of the window: the first one books the log line for when it's shown.
pub fn first_frame(window: &mut gpui::Window) {
    static SEEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if SEEN.swap(true, Ordering::Relaxed) {
        return;
    }
    mark("first draw");
    window.on_next_frame(|_, _| mark(SHOWN));
}
