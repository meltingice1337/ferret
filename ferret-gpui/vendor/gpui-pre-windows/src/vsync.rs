use std::{
    sync::LazyLock,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use gpui::FrameRequestSource;
use gpui_util::ResultExt;
use windows::Win32::{
    Foundation::HWND,
    Graphics::Dwm::{DWM_TIMING_INFO, DwmFlush, DwmGetCompositionTimingInfo},
    System::Performance::QueryPerformanceFrequency,
};

static QPC_TICKS_PER_SECOND: LazyLock<u64> = LazyLock::new(|| {
    let mut frequency = 0;
    // On systems that run Windows XP or later, the function will always succeed and
    // will thus never return zero.
    unsafe { QueryPerformanceFrequency(&mut frequency).unwrap() };
    frequency as u64
});

const VSYNC_INTERVAL_THRESHOLD: Duration = Duration::from_millis(1);
const DEFAULT_VSYNC_INTERVAL: Duration = Duration::from_micros(16_666); // ~60Hz

// Ferret patch: demand-driven vsync (`FrameDemand` below, its use in platform.rs, window.rs,
// events.rs and direct_manipulation.rs). Upstream wakes every window at every display refresh.

/// How often the vsync thread looks around while no window wants frames (device loss, stop).
pub(crate) const IDLE_CHECK_INTERVAL: Duration = Duration::from_millis(250);

/// The running vsync thread, to wake when a window wants frames again.
static VSYNC_THREAD: std::sync::Mutex<Option<std::thread::Thread>> = std::sync::Mutex::new(None);

pub(crate) fn set_vsync_thread(thread: std::thread::Thread) {
    *VSYNC_THREAD.lock().unwrap_or_else(|e| e.into_inner()) = Some(thread);
}

pub(crate) fn wake_vsync_thread() {
    if let Some(thread) = VSYNC_THREAD.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        thread.unpark();
    }
}

/// Whether a window wants frames: GPUI's frame demand, as `PlatformWindow::frame_waker` reports it
/// (a view changed, an animation or a video wants the next frame). The vsync thread only wakes
/// windows that want frames and sleeps while none do, so an idle window costs nothing instead of
/// a WM_PAINT at every display refresh. A window rests after a frame request that neither drew
/// nor asked for another frame (the macOS display link pauses the same way).
pub(crate) struct FrameDemand {
    wanted: std::sync::atomic::AtomicBool,
}

impl FrameDemand {
    pub(crate) fn new() -> Self {
        Self { wanted: std::sync::atomic::AtomicBool::new(true) }
    }

    pub(crate) fn wanted(&self) -> bool {
        self.wanted.load(std::sync::atomic::Ordering::Acquire)
    }

    pub(crate) fn want(&self) {
        if !self.wanted.swap(true, std::sync::atomic::Ordering::AcqRel) {
            wake_vsync_thread();
        }
    }

    pub(crate) fn rest(&self) {
        // FERRET_GPUI_VSYNC_ALWAYS: every window gets every vsync, as upstream GPUI does (an
        // escape hatch, and for comparisons).
        static ALWAYS: LazyLock<bool> =
            LazyLock::new(|| std::env::var_os("FERRET_GPUI_VSYNC_ALWAYS").is_some());
        if !*ALWAYS {
            self.wanted.store(false, std::sync::atomic::Ordering::Release);
        }
    }
}

pub(crate) struct VSyncProvider {
    interval: Duration,
    f: Box<dyn Fn() -> bool>,
}

impl VSyncProvider {
    pub(crate) fn new() -> Self {
        let interval = get_dwm_interval()
            .context("Failed to get DWM interval")
            .log_err()
            .unwrap_or(DEFAULT_VSYNC_INTERVAL);
        let f = Box::new(|| unsafe { DwmFlush().is_ok() });
        Self { interval, f }
    }

    pub(crate) fn wait_for_vsync(&self) -> FrameRequestSource {
        let vsync_start = Instant::now();
        let wait_succeeded = (self.f)();
        let elapsed = vsync_start.elapsed();
        // DwmFlush and DCompositionWaitForCompositorClock returns very early
        // instead of waiting until vblank when the monitor goes to sleep or is
        // unplugged (nothing to present due to desktop occlusion). We use 1ms as
        // a threshold for the duration of the wait functions and fallback to
        // Sleep() if it returns before that. This could happen during normal
        // operation for the first call after the vsync thread becomes non-idle,
        // but it shouldn't happen often.
        if !wait_succeeded || elapsed < VSYNC_INTERVAL_THRESHOLD {
            log::trace!("VSyncProvider::wait_for_vsync() took less time than expected");
            std::thread::sleep(self.interval);
            FrameRequestSource::LocalSchedule
        } else {
            FrameRequestSource::NativeCallback
        }
    }
}

fn get_dwm_interval() -> Result<Duration> {
    let mut timing_info = DWM_TIMING_INFO {
        cbSize: std::mem::size_of::<DWM_TIMING_INFO>() as u32,
        ..Default::default()
    };
    unsafe { DwmGetCompositionTimingInfo(HWND::default(), &mut timing_info) }?;
    let interval = retrieve_duration(timing_info.qpcRefreshPeriod, *QPC_TICKS_PER_SECOND);
    // Check for interval values that are impossibly low. A 29 microsecond
    // interval was seen (from a qpcRefreshPeriod of 60).
    if interval < VSYNC_INTERVAL_THRESHOLD {
        Ok(retrieve_duration(
            timing_info.rateRefresh.uiDenominator as u64,
            timing_info.rateRefresh.uiNumerator as u64,
        ))
    } else {
        Ok(interval)
    }
}

#[inline]
fn retrieve_duration(counts: u64, ticks_per_second: u64) -> Duration {
    let ticks_per_microsecond = ticks_per_second / 1_000_000;
    Duration::from_micros(counts / ticks_per_microsecond)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successful_compositor_wait_reports_native_callback() {
        let provider = VSyncProvider {
            interval: Duration::ZERO,
            f: Box::new(|| {
                std::thread::sleep(VSYNC_INTERVAL_THRESHOLD * 2);
                true
            }),
        };

        assert_eq!(
            provider.wait_for_vsync(),
            FrameRequestSource::NativeCallback
        );
    }

    #[test]
    fn failed_compositor_wait_reports_local_schedule_even_after_threshold() {
        let provider = VSyncProvider {
            interval: Duration::ZERO,
            f: Box::new(|| {
                std::thread::sleep(VSYNC_INTERVAL_THRESHOLD * 2);
                false
            }),
        };

        assert_eq!(provider.wait_for_vsync(), FrameRequestSource::LocalSchedule);
    }
}
