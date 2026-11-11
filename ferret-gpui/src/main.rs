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
mod imgedit;
mod input;
mod intro;
mod introdemos;
mod kinds;
mod link;
mod menu;
mod mx;
mod ocr;
mod ops;
mod palette;
mod pane;
mod pdftext;
mod preview;
mod quality;
mod remote;
mod script;
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
    gpui_platform::application().with_assets(assets::Assets).run(move |cx: &mut App| {
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
        input::bind_keys(cx);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.open_window(options, move |window, cx| cx.new(|cx| view::Ferret::new(bench, core_events, window, cx)))
            .expect("opening the window");
        if !background {
            cx.activate(true);
        }
    });
}
