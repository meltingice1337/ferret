//! Ferret, a file manager for Windows, drawn with GPUI (Zed's Rust UI framework) on ferret-core,
//! which it calls directly (no web view, no IPC). The Tauri app it replaces stays in src-tauri
//! and src as an archive, for comparison.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod addons;
mod appdata;
mod assets;
mod bench;
mod chrome;
mod codetheme;
mod commands;
mod dialogs;
mod doctext;
mod docx;
mod editor;
mod files;
mod filter;
mod heapstat;
mod imgedit;
mod imgmem;
mod input;
mod intro;
mod introdemos;
mod kinds;
mod link;
mod menu;
mod mx;
mod ocr;
mod ocrhelper;
mod ops;
mod palette;
mod pane;
mod pdftext;
mod preview;
mod quality;
mod remote;
mod script;
mod searchable;
mod render;
mod selectable;
mod settings;
mod settings_view;
mod sidebar;
mod slidedraw;
mod sortgroup;
mod theme;
mod ui;
mod update;
mod updateview;
mod util;
mod video;
mod videoview;
mod view;
mod watchdog;

use gpui::{
    App, AppContext, Bounds, TitlebarOptions, WindowBackgroundAppearance, WindowBounds, WindowOptions, point, px,
    size,
};

fn main() {
    // While Ferret is the default file manager, a background copy of it (`--explorer-redirect`,
    // started then and at every sign-in) turns Explorer windows into Ferret ones. As in the app,
    // that's this exe without a window.
    if std::env::args().any(|a| a == ferret_core::redirect::ARG) {
        return ferret_core::redirect::run();
    }
    // The uninstaller gives folder windows back to Explorer (when Ferret was the default file
    // manager) and stops the redirect before removing Ferret.
    if std::env::args().any(|a| a == ferret_core::integration::UNREGISTER_ARG) {
        return ferret_core::integration::unregister(appdata::IDENTIFIER);
    }
    // A window's text reader (ocrhelper.rs): no window, its log lines go to the window's log.
    if std::env::args().any(|a| a == ocrhelper::ARG) {
        return ocrhelper::run();
    }
    bench::mark("main");
    gpui_windows::ferret::on_mark(bench::mark);
    // A Ferret kept ready (ferret-core's standby.rs) takes this start when one waits: its window
    // is there in a fraction of the time, and this process has nothing more to do.
    let standby = std::env::args().any(|a| a == ferret_core::standby::ARG);
    if !standby && handed_over() {
        return;
    }
    // `--standby`: this process is the one kept ready. It does all of a start but the window,
    // then waits for a launch. Its pipe is made first, so one that comes meanwhile waits for it.
    let woken = if standby {
        let (tell, heard) = futures::channel::oneshot::channel();
        let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        // It leaves when the redirect that keeps it does (a test keeps its own).
        let keep: fn() -> bool = if std::env::var_os(STANDBY_TEST).is_some() { || true } else { ferret_core::redirect::running };
        let waits = ferret_core::standby::wait(&exe, keep, move |launch| {
            let _ = tell.send(launch);
        });
        // Another one waits already.
        if !waits {
            return;
        }
        Some(heard)
    } else {
        None
    };
    // The graphics device is over half of a start, and nothing needs it before the window: it's
    // made on a thread of its own from here on, while the rest goes on.
    gpui_windows::ferret::make_devices_early();
    appdata::start_log();
    // Before anything could load an add-on: a removal that waited for a restart happens now.
    addons::startup();
    let bench = bench::Bench::from_env();
    let background = std::env::var_os("FERRET_GPUI_BACKGROUND").is_some();
    // ferret-core reports jobs and asks for UI-thread work through this.
    let core_events = link::connect();
    // As the app: while Ferret is the default file manager, keep Windows pointing at this copy
    // (an update or a move changes the exe) and the Explorer redirect running. Off the main
    // thread; builds run from `target\` never take over (ferret-core's `test_build`).
    if appdata::real_install() && !ferret_core::integration::window_info().elevated {
        std::thread::spawn(ferret_core::integration::refresh_registration);
    }
    bench::mark("ready for GPUI");
    gpui_platform::application().with_assets(assets::Assets).run(move |cx: &mut App| {
        bench::mark("GPUI running");
        input::bind_keys(cx);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        match woken {
            None => open_window(cx, bench, core_events, background),
            // Kept ready: everything above is done, and the window waits for a launch. What it
            // opens (the launch's arguments), the settings and the session are read then.
            Some(heard) => {
                log::info!(target: "ferret::start", "ready {:.0} ms after the process began: waiting for a launch", bench::ms_since_process_start());
                // What the start-up touched (some 55 MB) isn't needed while it waits: handed
                // back to Windows a few seconds in, and read back in as the window is made.
                let waiting = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
                let still = waiting.clone();
                cx.spawn(async move |cx| {
                    cx.background_executor().timer(std::time::Duration::from_secs(4)).await;
                    if still.load(std::sync::atomic::Ordering::Acquire) {
                        give_memory_back();
                    }
                })
                .detach();
                cx.spawn(async move |cx| {
                    let Ok(launch) = heard.await else { return };
                    waiting.store(false, std::sync::atomic::Ordering::Release);
                    bench::mark("handed a launch");
                    ferret_core::integration::hand_launch(launch);
                    cx.update(|cx| open_window(cx, bench, core_events, background));
                })
                .detach();
            }
        }
    });
}

/// Lets Windows take this process's memory out of RAM (it comes back as it's touched).
fn give_memory_back() {
    use windows::Win32::System::ProcessStatus::EmptyWorkingSet;
    use windows::Win32::System::Threading::GetCurrentProcess;
    // SAFETY: this process's own handle.
    let _ = unsafe { EmptyWorkingSet(GetCurrentProcess()) };
}

/// A test hands its starts over too (an installed Ferret does anyway), and keeps its own standby.
const STANDBY_TEST: &str = "FERRET_GPUI_STANDBY_TEST";

/// Hands this start to the Ferret kept ready, if one waits: true when it took it. Only an
/// installed Ferret's ordinary starts: not an elevated one (the one waiting isn't), not a
/// window coming back as administrator (`--restore`), and not a test's (FERRET_GPUI_… set).
fn handed_over() -> bool {
    let testing = std::env::var_os(STANDBY_TEST).is_some();
    if !testing && !appdata::real_install() {
        return false;
    }
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let hooks = || std::env::vars_os().any(|(k, _)| k.to_str().is_some_and(|k| k.starts_with("FERRET_GPUI_") && k != "FERRET_GPUI_OPEN"));
    if args.iter().any(|a| a == "--restore") || (!testing && hooks()) {
        return false;
    }
    if ferret_core::integration::window_info().elevated {
        return false;
    }
    let Ok(exe) = std::env::current_exe() else { return false };
    // "New window" names its folder this way: the same as the folder as an argument.
    if let Ok(open) = std::env::var("FERRET_GPUI_OPEN") {
        args = vec![open];
    }
    let cwd = std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    ferret_core::standby::hand_over(&exe.to_string_lossy(), &ferret_core::standby::Launch { args, cwd }).is_some()
}

/// Opens the window: what a start ends with, and what the Ferret kept ready does when a launch
/// is handed to it.
fn open_window(cx: &mut App, bench: Option<bench::Bench>, core_events: futures::channel::mpsc::UnboundedReceiver<link::Msg>, background: bool) {
    // FERRET_GPUI_BOUNDS=x,y,w,h places the window (benchmarks and screenshots).
    let placed = std::env::var("FERRET_GPUI_BOUNDS").ok().and_then(|v| {
        let n: Vec<f32> = v.split(',').filter_map(|s| s.trim().parse().ok()).collect();
        (n.len() == 4).then(|| Bounds { origin: point(px(n[0]), px(n[1])), size: size(px(n[2]), px(n[3])) })
    });
    // FERRET_GPUI_DISPLAY=left|right|primary: the monitor (tests open away from where the user
    // works); FERRET_GPUI_BOUNDS is then measured from that monitor's corner.
    let display = std::env::var("FERRET_GPUI_DISPLAY").ok().and_then(|which| {
        let mut all = cx.displays();
        all.sort_by(|a, b| a.bounds().origin.x.as_f32().total_cmp(&b.bounds().origin.x.as_f32()));
        match which.as_str() {
            "left" => all.first().cloned(),
            "right" => all.last().cloned(),
            _ => cx.primary_display(),
        }
    });
    let bounds = match (&display, placed) {
        (Some(d), p) => {
            let area = d.bounds();
            let want = p.unwrap_or(Bounds { origin: point(px(40.), px(40.)), size: size(px(1400.), px(860.)) });
            // Kept inside the monitor (GPUI centers a window that doesn't fit).
            let w = want.size.width.min(area.size.width - px(40.));
            let h = want.size.height.min(area.size.height - px(80.));
            Bounds { origin: point(area.origin.x + want.origin.x.min(area.size.width - w), area.origin.y + want.origin.y.min(area.size.height - h)), size: size(w, h) }
        }
        (None, Some(p)) => p,
        (None, None) => Bounds::centered(None, size(px(1280.), px(800.)), cx),
    };
    let defaults = WindowOptions::default();
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        display_id: display.as_ref().map(|d| d.id()),
        titlebar: Some(TitlebarOptions {
            title: Some("Ferret (GPUI prototype)".into()),
            appears_transparent: true,
            traffic_light_position: None,
        }),
        window_background: WindowBackgroundAppearance::MicaBackdrop,
        window_min_size: Some(size(px(640.), px(420.))),
        app_id: Some("ferret-gpui".into()),
        // GPUI draws an unfocused window at ~30 fps to save power. A benchmark measures what the
        // list can do, so it lifts that cap (the real app would keep it).
        inactive_frame_interval: if bench.is_some() || background { None } else { defaults.inactive_frame_interval },
        // FERRET_GPUI_BACKGROUND: tests open behind whatever the user is doing, without taking
        // the keyboard (they drive the window with FERRET_GPUI_SCRIPT).
        focus: !background,
        ..defaults
    };
    cx.open_window(options, move |window, cx| {
        bench::mark("window made");
        let view = cx.new(|cx| view::Ferret::new(bench, core_events, window, cx));
        bench::mark("view made");
        view
    })
    .expect("opening the window");
    bench::mark("window opened");
    if !background {
        cx.activate(true);
    }
    // The window not answering is noticed and logged with what led to it (watchdog.rs).
    watchdog::start(cx);
}
