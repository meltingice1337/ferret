mod archive;
mod commands;
mod db;
mod details;
mod fileops;
mod foldersize;
mod integration;
mod jobs;
mod listing;
mod logging;
mod media;
mod paths;
mod places;
mod protocol;
mod recycle;
mod redirect;
pub mod search;
mod shell;
mod shellmenu;
mod shellops;
mod watch;
mod wsl;

use std::sync::Mutex;
use tauri::Manager;

use commands::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // The background Explorer redirect (redirect.rs) is this exe without a window.
    if std::env::args().any(|a| a == redirect::ARG) {
        return redirect::run();
    }
    let context = tauri::generate_context!();
    // The dev build runs under its own identifier (tauri.dev.conf.json), so it keeps its own
    // settings and WebView2 profiles apart from the installed app.
    let id = context.config().identifier.clone();
    // The uninstaller gives folder windows back to Explorer before removing Ferret.
    if std::env::args().any(|a| a == integration::UNREGISTER_ARG) {
        return integration::unregister(&id);
    }
    // SAFETY (the `set_var` calls below): they run before Tauri or Ferret start any thread.
    // UI tests drive a dev build over the WebView2 DevTools protocol. Any local program
    // could use that port, so it only opens when explicitly asked for (FERRET_DEVTOOLS=1),
    // and never in release builds.
    #[cfg(debug_assertions)]
    if std::env::var_os("FERRET_DEVTOOLS").is_some() && std::env::var_os("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS").is_none() {
        unsafe { std::env::set_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", "--remote-debugging-port=9333") };
        // A WebView2 profile runs with one set of options at a time; keep this one apart so it
        // can run next to normal Ferret windows.
        if let Some(dir) = dirs::data_local_dir() {
            unsafe { std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", dir.join(&id).join("EBWebView-devtools")) };
        }
    }
    logging::install_panic_hook();
    integration::init_launch_targets();
    integration::init_window_registry(&id);

    // Every launch is its own window. An elevated window (restarted as administrator, or
    // started from one) uses its own WebView2 profile: a profile can't be shared across
    // privilege levels.
    let admin = integration::admin_mode();
    // Debug builds only: the end-to-end tests start Ferret with FERRET_E2E=1.
    let e2e = cfg!(debug_assertions) && std::env::var_os("FERRET_E2E").is_some();
    if admin {
        if let Some(dir) = dirs::data_local_dir() {
            unsafe { std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", dir.join(&id).join("EBWebView-admin")) };
        }
    }
    tauri::Builder::default()
        // First, so everything after it (other plugins' setup included) is recorded.
        .plugin(logging::plugin())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_drag::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .register_asynchronous_uri_scheme_protocol("fs", |_ctx, req, responder| {
            tauri::async_runtime::spawn_blocking(move || responder.respond(protocol::serve_file(req)));
        })
        .register_asynchronous_uri_scheme_protocol("thumb", |_ctx, req, responder| {
            tauri::async_runtime::spawn_blocking(move || {
                // Shell thumbnail extraction (esp. video) is heavy; cap concurrency.
                let _permit = protocol::THUMB_SLOTS.acquire();
                responder.respond(protocol::serve_thumb(req))
            });
        })
        .setup(move |app| {
            logging::log_startup(app.handle(), admin);
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let conn = db::open(&dir.join("ferret.db"))?;
            app.manage(AppState {
                db: Mutex::new(conn),
                watcher: Mutex::new(watch::DirWatcher::new(app.handle().clone())),
            });
            // The window starts hidden so a restarted window appears right where the old one was.
            if let Some(window) = app.get_webview_window("main") {
                shell::set_big_icon(&window);
                // Development builds say so in the taskbar, so they can't be mistaken for the installed app.
                let title = match (admin, cfg!(debug_assertions)) {
                    (true, true) => Some("Ferret (Dev, Administrator)"),
                    (true, false) => Some("Ferret (Administrator)"),
                    (false, true) => Some("Ferret (Dev)"),
                    (false, false) => None,
                };
                if let Some(title) = title {
                    let _ = window.set_title(title);
                }
                integration::apply_placement(&window);
                if e2e {
                    // End-to-end test run (scripts/e2e): a real, rendering window, but off-screen,
                    // out of the taskbar and never activated, so it doesn't disturb the desktop.
                    let _ = window.set_skip_taskbar(true);
                    let _ = window.set_position(tauri::PhysicalPosition::new(-32000, -32000));
                    #[cfg(windows)]
                    if let Ok(hwnd) = window.hwnd() {
                        use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_SHOWNOACTIVATE};
                        unsafe {
                            let _ = ShowWindow(windows::Win32::Foundation::HWND(hwnd.0 as *mut _), SW_SHOWNOACTIVATE);
                        }
                    }
                } else {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            if !admin {
                integration::refresh_registration(&app.state::<AppState>());
            }
            shellmenu::warm_up();
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::bootstrap,
            commands::get_places,
            commands::shared_state,
            commands::list_dir,
            commands::resolve_input,
            commands::open_path,
            commands::watch_dirs,
            commands::save_settings,
            commands::save_session,
            commands::set_folder_view,
            commands::clear_folder_view,
            commands::list_folder_views,
            commands::clear_all_folder_views,
            commands::add_favorite,
            commands::remove_favorite,
            commands::update_favorite,
            commands::reorder_favorites,
            commands::push_recent,
            commands::remove_recent,
            commands::clear_recents,
            commands::set_backdrop,
            commands::drag_icon,
            logging::open_log_folder,
            jobs::cancel_job,
            jobs::answer_job,
            fileops::create_folder,
            fileops::create_file,
            fileops::rename_item,
            fileops::check_conflicts,
            fileops::transfer,
            fileops::undo_moves,
            fileops::delete_items,
            fileops::restore_from_trash,
            fileops::find_in_trash,
            fileops::item_info,
            fileops::folder_stats,
            foldersize::folder_sizes,
            foldersize::folder_sizes_invalidate,
            fileops::set_attributes,
            fileops::read_text,
            fileops::read_bytes,
            details::file_details,
            shellops::shell_op,
            // shellmenu.rs (Explorer's third-party entries in Ferret's menu)
            shellmenu::shell_menu_items,
            shellmenu::shell_menu_invoke,
            // shell.rs commands (registered by the shell agent)
            shell::show_native_menu,
            shell::show_native_background_menu,
            shell::show_properties,
            shell::open_with_dialog,
            shell::list_open_with,
            shell::open_with,
            shell::clipboard_write_files,
            shell::clipboard_read_files,
            shell::open_terminal,
            shell::open_in_vscode,
            shell::reveal_in_explorer,
            // archive.rs commands (registered by the archive agent)
            archive::archive_materialize,
            archive::archive_extract,
            archive::archive_extract_smart,
            archive::compress_zip,
            archive::archive_info,
            archive::archive_unlock,
            // media.rs (video / picture compression)
            media::video_info,
            media::compress_videos,
            media::video_sample,
            media::image_info,
            media::image_preview,
            media::compress_images,
            media::media_release,
            // search (whole-disk index)
            search::search_query,
            search::search_status,
            search::search_cancel,
            search::search_install_service,
            search::search_service_outdated,
            // Recycle Bin and Windows integration
            recycle::recycle_info,
            recycle::empty_recycle_bin,
            integration::launch_targets,
            integration::default_manager_status,
            integration::set_default_manager,
            integration::open_in_explorer,
            integration::window_info,
            integration::new_window,
            integration::restart_elevated,
        ])
        .run(context)
        .expect("error while running Ferret");
}
