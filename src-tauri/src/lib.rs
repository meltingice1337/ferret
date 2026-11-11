//! The Tauri shell around ferret-core: the window, the commands the page calls (cmd/), the
//! fs:// and thumb:// schemes (protocol.rs), logging, and the link the core reaches the app
//! through (link.rs).

mod cmd;
mod filedrop;
mod link;
mod log_setup;
mod protocol;
mod window;

use ferret_core::{app, db, integration, logging, redirect, rt, shellmenu};
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    integration::STARTED.get_or_init(std::time::Instant::now);
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
    if admin {
        if let Some(dir) = dirs::data_local_dir() {
            unsafe { std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", dir.join(&id).join("EBWebView-admin")) };
        }
    }
    // One async runtime for the core and Tauri, started here: after the `set_var` calls above
    // (its threads would read the environment) and before Tauri would start its own.
    tauri::async_runtime::set(rt::handle());
    tauri::Builder::default()
        // First, so everything after it (other plugins' setup included) is recorded.
        .plugin(log_setup::plugin())
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
            log_setup::log_startup(app.handle(), admin);
            log_setup::watch_main_thread(app.handle().clone());
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let conn = db::open(&dir.join("ferret.db"))?;
            app::init(Box::new(link::TauriLink(app.handle().clone())), conn);
            // The window starts hidden so a restarted window appears right where the old one
            // was, and so it first appears with the app drawn (integration::reveal).
            if let Some(window) = app.get_webview_window("main") {
                window::set_big_icon(&window);
                // Files dropped in from other apps (Tauri's own handler is off: see filedrop.rs).
                filedrop::install(&window);
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
                window::apply_placement(&window);
                if integration::e2e() {
                    let _ = window.set_skip_taskbar(true);
                    let _ = window.set_position(tauri::PhysicalPosition::new(-32000, -32000));
                }
                std::thread::spawn(move || {
                    std::thread::sleep(integration::SHOW_AFTER);
                    if !window::reveal(&window) {
                        log::warn!("the page didn't show the window within {:?}", integration::SHOW_AFTER);
                    }
                });
            }
            // Off the main thread: until the window is up, it serves the page its files and calls.
            std::thread::spawn(move || {
                if !admin {
                    integration::refresh_registration();
                }
                shellmenu::warm_up();
            });
            Ok(())
        })
        .invoke_handler(log_setup::timed(tauri::generate_handler![
            cmd::browse::bootstrap,
            cmd::browse::get_places,
            cmd::browse::shared_state,
            cmd::browse::list_dir,
            cmd::browse::resolve_input,
            cmd::browse::open_path,
            cmd::browse::watch_dirs,
            cmd::browse::save_settings,
            cmd::browse::save_session,
            cmd::browse::set_folder_view,
            cmd::browse::clear_folder_view,
            cmd::browse::list_folder_views,
            cmd::browse::clear_all_folder_views,
            cmd::browse::add_favorite,
            cmd::browse::remove_favorite,
            cmd::browse::update_favorite,
            cmd::browse::reorder_favorites,
            cmd::browse::push_recent,
            cmd::browse::remove_recent,
            cmd::browse::clear_recents,
            cmd::browse::set_backdrop,
            cmd::browse::drag_icon,
            filedrop::drop_feedback,
            filedrop::e2e_drop,
            log_setup::log_folder,
            cmd::fileops::cancel_job,
            cmd::fileops::answer_job,
            cmd::fileops::create_folder,
            cmd::fileops::remove_empty_folder,
            cmd::fileops::create_file,
            cmd::fileops::rename_item,
            cmd::fileops::check_conflicts,
            cmd::fileops::transfer,
            cmd::fileops::undo_moves,
            cmd::fileops::delete_items,
            cmd::fileops::restore_from_trash,
            cmd::fileops::find_in_trash,
            cmd::fileops::item_info,
            cmd::fileops::folder_stats,
            cmd::fileops::folder_sizes,
            cmd::fileops::folder_sizes_invalidate,
            cmd::fileops::set_attributes,
            cmd::fileops::read_text,
            cmd::fileops::read_bytes,
            cmd::fileops::file_details,
            cmd::fileops::shell_op,
            // Explorer's third-party entries in Ferret's menu
            cmd::shell::shell_menu_items,
            cmd::shell::shell_menu_invoke,
            // Windows shell
            cmd::shell::show_native_menu,
            cmd::shell::show_native_background_menu,
            cmd::shell::show_properties,
            cmd::shell::open_with_dialog,
            cmd::shell::list_open_with,
            cmd::shell::open_with,
            cmd::shell::clipboard_write_files,
            cmd::shell::clipboard_after_cut,
            cmd::shell::clipboard_read_files,
            cmd::shell::clipboard_sequence,
            cmd::shell::open_terminal,
            cmd::shell::open_in_vscode,
            cmd::shell::reveal_in_explorer,
            // archives
            cmd::archive::archive_materialize,
            cmd::archive::archive_extract,
            cmd::archive::archive_extract_smart,
            cmd::archive::compress_zip,
            cmd::archive::archive_info,
            cmd::archive::archive_unlock,
            // video / picture compression, office previews
            cmd::archive::video_info,
            cmd::archive::audio_info,
            cmd::archive::compress_videos,
            cmd::archive::video_sample,
            cmd::archive::image_info,
            cmd::archive::image_preview,
            cmd::archive::sheet_preview,
            cmd::archive::slides_preview,
            cmd::archive::compress_images,
            cmd::archive::media_release,
            // network folders, SSH connections
            cmd::remote::network_connect,
            cmd::remote::network_forget,
            cmd::remote::network_computers,
            cmd::remote::remote_connections,
            cmd::remote::remote_save_connection,
            cmd::remote::remote_delete_connection,
            cmd::remote::remote_config_hosts,
            cmd::remote::remote_connect,
            cmd::remote::remote_disconnect,
            cmd::remote::remote_test,
            cmd::remote::remote_status,
            cmd::remote::remote_materialize,
            cmd::remote::remote_answer,
            cmd::remote::remote_open,
            // search (whole-disk index)
            cmd::system::search_query,
            cmd::system::search_status,
            cmd::system::search_cancel,
            cmd::system::search_install_service,
            cmd::system::search_service_outdated,
            // Recycle Bin and Windows integration
            cmd::system::recycle_info,
            cmd::system::no_recycle_bin,
            cmd::system::empty_recycle_bin,
            cmd::system::launch_targets,
            cmd::system::default_manager_status,
            cmd::system::set_default_manager,
            cmd::system::open_in_explorer,
            cmd::system::window_info,
            cmd::system::show_window,
            cmd::system::new_window,
            cmd::system::restart_elevated,
        ]))
        .run(context)
        .expect("error while running Ferret");
}
