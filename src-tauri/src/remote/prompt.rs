//! Questions a connection asks the user (a password, a key's passphrase, whether to trust a
//! server's key): sent to the window as `remote-prompt`, answered with `remote_answer`.

use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use tokio::sync::oneshot;

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Field {
    pub label: String,
    /// Typed text shows as dots.
    pub secret: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Prompt {
    pub id: u64,
    /// The connection's name, for the title.
    pub connection: String,
    /// "secret" (fields to fill in), "hostKey" (trust a new server key?), "confirm" (yes / no).
    pub kind: &'static str,
    pub title: String,
    pub message: String,
    pub fields: Vec<Field>,
    /// Offers a Remember box (stores the answer in Credential Manager).
    pub can_remember: bool,
    pub fingerprint: Option<String>,
    pub confirm_label: Option<String>,
    /// The confirm button warns (red).
    pub danger: bool,
}

impl Prompt {
    pub fn secret(connection: &str, title: impl Into<String>, message: impl Into<String>, label: &str, can_remember: bool) -> Self {
        Prompt {
            id: 0,
            connection: connection.to_string(),
            kind: "secret",
            title: title.into(),
            message: message.into(),
            fields: vec![Field { label: label.to_string(), secret: true }],
            can_remember,
            fingerprint: None,
            confirm_label: None,
            danger: false,
        }
    }

    pub fn confirm(connection: &str, title: impl Into<String>, message: impl Into<String>, confirm: &str) -> Self {
        Prompt {
            kind: "confirm",
            fields: vec![],
            can_remember: false,
            confirm_label: Some(confirm.to_string()),
            ..Prompt::secret(connection, title, message, "", false)
        }
    }
}

#[derive(Debug, Clone)]
pub struct Answer {
    pub values: Vec<String>,
    pub remember: bool,
}

fn pending() -> &'static Mutex<HashMap<u64, oneshot::Sender<Option<Answer>>>> {
    static P: OnceLock<Mutex<HashMap<u64, oneshot::Sender<Option<Answer>>>>> = OnceLock::new();
    P.get_or_init(Default::default)
}

/// Asks the user and waits for the answer; None when they cancelled (or no window can ask:
/// tests, a window closing).
pub async fn ask(mut prompt: Prompt) -> Option<Answer> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    if !super::has_window() {
        return None;
    }
    prompt.id = NEXT.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = oneshot::channel();
    pending().lock().unwrap().insert(prompt.id, tx);
    log::info!("remote: asking \"{}\" ({})", prompt.title, prompt.kind);
    super::emit("remote-prompt", &prompt);
    rx.await.ok().flatten()
}

/// The user's answer to a `remote-prompt` (None: cancelled).
#[tauri::command]
pub fn remote_answer(id: u64, values: Option<Vec<String>>, remember: bool) {
    if let Some(tx) = pending().lock().unwrap().remove(&id) {
        let _ = tx.send(values.map(|values| Answer { values, remember }));
    }
}
