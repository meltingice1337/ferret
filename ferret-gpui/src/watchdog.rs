//! The window not answering: noticed, timed and logged with what led to it.
//!
//! A heartbeat on the UI thread (every 100 ms) and a watchdog thread that checks it. When the UI
//! thread is held up for 250 ms or more, the hitch is logged once it's over, with how long and
//! the last things the user did (`act`: opened a folder, a preview, Ctrl+K…). Held up for a second
//! or more, the watchdog also looks at where the UI thread is: it pauses it for a moment, reads
//! its stack (a few times while it stays stuck) and logs the functions, with their files and
//! lines where the build's symbols are found (its PDB, next to a build made here), else as
//! `module+offset`, which a release's kept PDB (`target\symbols\<version>`, release.sh) turns into
//! names. A machine that went to sleep isn't a hang (the watchdog was paused too).
//!
//! Every `act` is also a log line (target `ferret::act`), so the log reads as what happened.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static START: OnceLock<Instant> = OnceLock::new();
/// When the UI thread last ran its heartbeat (ms since START).
static BEAT: AtomicU64 = AtomicU64::new(0);
/// The last things done, with when (ms since START).
static ACTS: Mutex<VecDeque<(u64, String)>> = Mutex::new(VecDeque::new());

fn now_ms() -> u64 {
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// Something the user did (or the app did for them): logged, and kept so that a hang right after
/// says what came before it.
pub fn act(what: impl Into<String>) {
    let what = what.into();
    log::info!(target: "ferret::act", "{what}");
    if let Ok(mut a) = ACTS.lock() {
        a.push_back((now_ms(), what));
        while a.len() > 16 {
            a.pop_front();
        }
    }
}

/// Starts the heartbeat and the watchdog (on the UI thread, once).
pub fn start(cx: &mut gpui::App) {
    static STARTED: std::sync::Once = std::sync::Once::new();
    if STARTED.is_completed() {
        return;
    }
    STARTED.call_once(|| {});
    BEAT.store(now_ms(), Ordering::Relaxed);
    let ui = stack::UiThread::current();
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            BEAT.store(now_ms(), Ordering::Relaxed);
        }
    })
    .detach();
    std::thread::Builder::new().name("watchdog".into()).spawn(move || watch(ui)).ok();
}

/// A hold-up of the UI thread in progress.
struct Stall {
    /// The last heartbeat before it.
    since: u64,
    /// Stacks sampled while it lasted (the UI thread's return addresses, innermost first).
    samples: Vec<(u64, Vec<u64>)>,
    /// How late the heartbeat must be before the next sample.
    next_sample: u64,
}

fn watch(ui: Option<stack::UiThread>) {
    let mut stall: Option<Stall> = None;
    let mut last = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(100));
        let gap = last.elapsed();
        last = Instant::now();
        // The whole process (or the computer) was paused: sleep, a debugger. Not a hang.
        if gap > Duration::from_millis(1500) {
            stall = None;
            continue;
        }
        let now = now_ms();
        let beat = BEAT.load(Ordering::Relaxed);
        let late = now.saturating_sub(beat);
        match stall.as_mut() {
            None if late >= 350 => stall = Some(Stall { since: beat, samples: Vec::new(), next_sample: 1000 }),
            Some(s) if late < 250 => {
                // Over: it lasted from the beat before it to the one after, less a beat's interval.
                let lasted = beat.saturating_sub(s.since).saturating_sub(100);
                if let Some(s) = stall.take() {
                    report(&s, lasted);
                }
            }
            Some(s) if late >= s.next_sample && s.samples.len() < 4 => {
                if let Some(frames) = ui.as_ref().and_then(|u| u.sample()) {
                    s.samples.push((late, frames));
                }
                s.next_sample = late * 2;
            }
            _ => {}
        }
    }
}

fn report(s: &Stall, lasted: u64) {
    if lasted < 250 {
        return;
    }
    // What was done in the 10 s before it.
    let acts: Vec<String> = ACTS
        .lock()
        .map(|a| {
            a.iter()
                .filter(|(at, _)| *at + 10_000 >= s.since && *at <= s.since + lasted)
                .map(|(at, w)| if *at <= s.since { format!("{:.1} s before: {w}", (s.since - at) as f32 / 1000.) } else { format!("during: {w}") })
                .collect()
        })
        .unwrap_or_default();
    let before = if acts.is_empty() { "nothing done just before".to_string() } else { acts.join("; ") };
    if s.samples.is_empty() {
        log::info!(target: "ferret::hang", "the window was busy for {lasted} ms ({before})");
        return;
    }
    let mut text = format!("the window didn't answer for {:.1} s ({before}); where the UI thread was:", lasted as f32 / 1000.);
    let mut seen_same = None;
    for (late, frames) in &s.samples {
        if seen_same.as_ref() == Some(frames) {
            text.push_str(&format!("\n  at {:.1} s: the same", *late as f32 / 1000.));
            continue;
        }
        text.push_str(&format!("\n  at {:.1} s:", *late as f32 / 1000.));
        for line in stack::describe(frames) {
            text.push_str("\n    ");
            text.push_str(&line);
        }
        seen_same = Some(frames.clone());
    }
    log::warn!(target: "ferret::hang", "{text}");
}

#[cfg(all(windows, target_arch = "x86_64"))]
mod stack {
    use windows::Win32::Foundation::{HANDLE, HMODULE};
    use windows::Win32::System::Diagnostics::Debug::{
        CONTEXT, CONTEXT_FULL_AMD64, GetThreadContext, IMAGEHLP_LINEW64, RtlLookupFunctionEntry, RtlVirtualUnwind, SYMBOL_INFOW, SYMOPT_DEFERRED_LOADS,
        SYMOPT_LOAD_LINES, SYMOPT_UNDNAME, SymFromAddrW, SymGetLineFromAddrW64, SymInitializeW, SymRefreshModuleList, SymSetOptions, SymSetSearchPathW,
        UNW_FLAG_NHANDLER,
    };
    use windows::Win32::System::LibraryLoader::{GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, GetModuleFileNameW, GetModuleHandleExW};
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentThreadId, OpenThread, ResumeThread, SuspendThread, THREAD_GET_CONTEXT, THREAD_QUERY_INFORMATION, THREAD_SUSPEND_RESUME,
    };

    /// The UI thread, to look at from the watchdog.
    pub struct UiThread(HANDLE);
    // A thread handle is usable from any thread.
    unsafe impl Send for UiThread {}

    /// GetThreadContext wants it 16-byte aligned on x64.
    #[repr(C, align(16))]
    struct Aligned(CONTEXT);

    const MAX_FRAMES: usize = 48;

    impl UiThread {
        pub fn current() -> Option<UiThread> {
            unsafe { OpenThread(THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT | THREAD_QUERY_INFORMATION, false, GetCurrentThreadId()).ok().map(UiThread) }
        }

        /// Its return addresses, innermost first. While it's paused nothing here allocates or
        /// takes a lock (it might hold the one we'd wait for).
        pub fn sample(&self) -> Option<Vec<u64>> {
            let mut frames = [0u64; MAX_FRAMES];
            let mut n = 0;
            unsafe {
                if SuspendThread(self.0) == u32::MAX {
                    return None;
                }
                let mut ctx: Aligned = std::mem::zeroed();
                ctx.0.ContextFlags = CONTEXT_FULL_AMD64;
                if GetThreadContext(self.0, &mut ctx.0).is_ok() {
                    while n < MAX_FRAMES && ctx.0.Rip != 0 {
                        frames[n] = ctx.0.Rip;
                        n += 1;
                        let mut base = 0u64;
                        let entry = RtlLookupFunctionEntry(ctx.0.Rip, &mut base, None);
                        if entry.is_null() {
                            // A leaf function: its return address is on top of the stack.
                            if ctx.0.Rsp == 0 {
                                break;
                            }
                            ctx.0.Rip = *(ctx.0.Rsp as *const u64);
                            ctx.0.Rsp += 8;
                        } else {
                            let mut data = std::ptr::null_mut();
                            let mut frame = 0u64;
                            RtlVirtualUnwind(UNW_FLAG_NHANDLER, base, ctx.0.Rip, entry, &mut ctx.0, &mut data, &mut frame, None);
                        }
                    }
                }
                ResumeThread(self.0);
            }
            (n > 0).then(|| frames[..n].to_vec())
        }
    }

    /// Symbols for the process: searched for beside the exe (where a build made here has its PDB),
    /// then where the exe says it was built. Rust's own backtraces may have set dbghelp up already:
    /// then only the search path is changed.
    fn symbols() -> bool {
        static READY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *READY.get_or_init(|| unsafe {
            SymSetOptions(SYMOPT_UNDNAME | SYMOPT_DEFERRED_LOADS | SYMOPT_LOAD_LINES);
            let dir = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.display().to_string())).unwrap_or_default();
            let path = windows::core::HSTRING::from(dir);
            if SymInitializeW(GetCurrentProcess(), &path, true).is_ok() {
                return true;
            }
            let _ = SymSetSearchPathW(GetCurrentProcess(), &path);
            SymRefreshModuleList(GetCurrentProcess()).is_ok()
        })
    }

    /// Each address as `module+offset`, with its function, file and line where symbols are found.
    /// Frames outside Ferret's own code past the first few are left out (the event loop's
    /// plumbing), so the part that matters reads first.
    pub fn describe(frames: &[u64]) -> Vec<String> {
        let sym = symbols();
        let mut out = Vec::new();
        for &addr in frames {
            let mut module = HMODULE::default();
            let (name, base) = unsafe {
                if GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, windows::core::PCWSTR(addr as *const u16), &mut module).is_ok() {
                    let mut buf = [0u16; 260];
                    let n = GetModuleFileNameW(Some(module), &mut buf) as usize;
                    let full = String::from_utf16_lossy(&buf[..n]);
                    (full.rsplit('\\').next().unwrap_or("?").to_string(), module.0 as u64)
                } else {
                    ("?".to_string(), 0)
                }
            };
            let mut line = format!("{name}+0x{:x}", addr.saturating_sub(base));
            if sym {
                unsafe {
                    let mut buf = vec![0u64; (std::mem::size_of::<SYMBOL_INFOW>() + 512 * 2) / 8 + 1];
                    let info = buf.as_mut_ptr() as *mut SYMBOL_INFOW;
                    (*info).SizeOfStruct = std::mem::size_of::<SYMBOL_INFOW>() as u32;
                    (*info).MaxNameLen = 512;
                    let mut disp = 0u64;
                    if SymFromAddrW(GetCurrentProcess(), addr, Some(&mut disp), info).is_ok() {
                        let len = ((*info).NameLen as usize).min(512);
                        let fname = String::from_utf16_lossy(std::slice::from_raw_parts((*info).Name.as_ptr(), len));
                        line.push_str(&format!(" {fname}"));
                        let mut l = IMAGEHLP_LINEW64 { SizeOfStruct: std::mem::size_of::<IMAGEHLP_LINEW64>() as u32, ..Default::default() };
                        let mut d32 = 0u32;
                        if SymGetLineFromAddrW64(GetCurrentProcess(), addr, &mut d32, &mut l).is_ok() {
                            let file = l.FileName.to_string().unwrap_or_default();
                            let short = file.rsplit(['\\', '/']).take(2).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("/");
                            line.push_str(&format!(" ({short}:{})", l.LineNumber));
                        }
                    }
                }
            }
            out.push(line);
        }
        out
    }
}

#[cfg(not(all(windows, target_arch = "x86_64")))]
mod stack {
    pub struct UiThread;
    impl UiThread {
        pub fn current() -> Option<UiThread> {
            None
        }
        pub fn sample(&self) -> Option<Vec<u64>> {
            None
        }
    }
    pub fn describe(_: &[u64]) -> Vec<String> {
        Vec::new()
    }
}

#[cfg(all(test, windows, target_arch = "x86_64"))]
mod tests {
    use super::*;

    #[test]
    fn a_busy_thread_is_caught_where_it_is() {
        // A thread spinning in a known function: its sampled stack names it (tests have symbols).
        let (tx, rx) = std::sync::mpsc::channel();
        let busy = std::thread::spawn(move || {
            tx.send(stack::UiThread::current()).unwrap();
            spin_for_the_watchdog_test(Duration::from_millis(1500));
        });
        let ui = rx.recv().unwrap().expect("a handle on the thread");
        std::thread::sleep(Duration::from_millis(300));
        let frames = ui.sample().expect("a stack");
        busy.join().unwrap();
        let text = stack::describe(&frames).join("\n");
        assert!(text.contains("spin_for_the_watchdog_test"), "{text}");
    }

    #[inline(never)]
    fn spin_for_the_watchdog_test(d: Duration) {
        let t = Instant::now();
        let mut x = 0u64;
        while t.elapsed() < d {
            x = std::hint::black_box(x.wrapping_mul(31).wrapping_add(7));
        }
        std::hint::black_box(x);
    }
}
