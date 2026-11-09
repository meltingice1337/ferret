//! Network folders and SSH connections.

use ferret_core::network::{self, Computer};
use ferret_core::remote::{self, config::HostConfig, edit, prompt, Connection, ConnectionState, TestResult};

type Res<T> = Result<T, String>;

#[tauri::command(async)]
pub fn network_connect(path: String, user: Option<String>, password: Option<String>, remember: bool) -> Res<bool> {
    network::network_connect(path, user, password, remember)
}

#[tauri::command(async)]
pub fn network_forget(path: String) -> Res<()> {
    network::network_forget(path)
}

#[tauri::command]
pub async fn network_computers() -> Res<Vec<Computer>> {
    network::network_computers().await
}

#[tauri::command]
pub fn remote_connections() -> Vec<Connection> {
    remote::remote_connections()
}

#[tauri::command]
pub async fn remote_save_connection(connection: Connection) -> Res<Vec<Connection>> {
    remote::remote_save_connection(connection).await
}

#[tauri::command]
pub async fn remote_delete_connection(id: String) -> Res<Vec<Connection>> {
    remote::remote_delete_connection(id).await
}

#[tauri::command]
pub fn remote_config_hosts() -> Vec<HostConfig> {
    remote::remote_config_hosts()
}

#[tauri::command]
pub async fn remote_connect(id: String) -> Res<String> {
    remote::remote_connect(id).await
}

#[tauri::command]
pub async fn remote_disconnect(id: String) {
    remote::remote_disconnect(id).await
}

#[tauri::command]
pub async fn remote_test(connection: Connection) -> Res<TestResult> {
    remote::remote_test(connection).await
}

#[tauri::command]
pub fn remote_status() -> Vec<ConnectionState> {
    remote::remote_status()
}

#[tauri::command]
pub async fn remote_materialize(path: String) -> Res<String> {
    remote::remote_materialize(path).await
}

#[tauri::command]
pub fn remote_answer(id: u64, values: Option<Vec<String>>, remember: bool) {
    prompt::remote_answer(id, values, remember)
}

#[tauri::command]
pub async fn remote_open(path: String) -> Res<()> {
    edit::remote_open(path).await
}
