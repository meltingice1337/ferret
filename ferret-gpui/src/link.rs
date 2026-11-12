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
    // Tried a few times: right after another Ferret was ended by force (an update's installer
    // does that) the database can refuse to open for a moment ("disk I/O error", seen in a test
    // that ended one window and started the next at once), and a window without it saves nothing.
    let mut tries = 0;
    let db = loop {
        match ferret_core::db::open(&dir.join("ferret.db")) {
            Ok(db) => break Some(db),
            Err(e) if tries < 4 => {
                tries += 1;
                log::warn!("the database didn't open ({e}): trying again");
                std::thread::sleep(std::time::Duration::from_millis(80));
            }
            Err(e) => {
                log::error!("no database ({e}): settings, recents and jobs' progress aren't kept in this window");
                break None;
            }
        }
    };
    if let Some(db) = db {
        ferret_core::app::init(Box::new(GpuiLink(tx)), db);
    }
    rx
}
