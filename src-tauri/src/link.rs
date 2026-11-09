//! The core's way to the app (`ferret_core::app::AppLink`), through Tauri: events go to the
//! page, UI-thread work to the event loop.

use tauri::{AppHandle, Emitter};

use ferret_core::app::AppLink;

pub struct TauriLink(pub AppHandle);

impl AppLink for TauriLink {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        let _ = self.0.emit(event, payload);
    }

    fn run_on_main_thread(&self, f: Box<dyn FnOnce() + Send>) -> Result<(), String> {
        self.0.run_on_main_thread(f).map_err(|e| e.to_string())
    }
}
