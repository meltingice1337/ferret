//! ferret-core's way back to the app (its `AppLink`): events and work for the UI thread go over a
//! channel that a GPUI task on the UI thread drains. The Tauri shell does the same with Tauri's
//! event system; nothing in the core knows which UI it talks to.

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};

pub enum Msg {
    /// An event, as the web page would have received it (`job-update`, `media-sample-progress`…).
    Event(String, serde_json::Value),
    /// Work the core wants run on the UI thread.
    Run(Box<dyn FnOnce() + Send>),
}

struct GpuiLink(UnboundedSender<Msg>);

impl ferret_core::app::AppLink for GpuiLink {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        let _ = self.0.unbounded_send(Msg::Event(event.to_string(), payload));
    }

    fn run_on_main_thread(&self, f: Box<dyn FnOnce() + Send>) -> Result<(), String> {
        self.0.unbounded_send(Msg::Run(f)).map_err(|e| e.to_string())
    }
}

/// Connects ferret-core to this app, with its settings database (appdata.rs: the app's own when
/// installed, a folder of its own otherwise). Returns the receiving end for the window to drain.
pub fn connect() -> UnboundedReceiver<Msg> {
    let (tx, rx) = unbounded();
    let dir = crate::appdata::data_dir();
    let _ = std::fs::create_dir_all(&dir);
    match ferret_core::db::open(&dir.join("ferret.db")) {
        Ok(db) => ferret_core::app::init(Box::new(GpuiLink(tx)), db),
        Err(e) => eprintln!("ferret-gpui: no database ({e}); jobs won't report progress"),
    }
    rx
}
