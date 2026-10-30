//! SSH sessions: one per connection in each Ferret window, opened when the user opens one of its
//! folders (never for background work), shared by everything that uses it (listing, copies,
//! previews, folder sizes, search) through SFTP and command channels, and closed after a while
//! unused.
//!
//! Logging in tries what `ssh` would: the SSH agent (Windows' OpenSSH agent, then Pageant), the
//! key files (the connection's, the config's, then ~/.ssh/id_*), asking for a key's passphrase if
//! it has one, then a password or the server's own questions (keyboard-interactive). A server's
//! key is checked against ~/.ssh/known_hosts; a new one is shown to the user to trust, a changed
//! one is refused.

use russh::client::{self, Handle, KeyboardInteractiveAuthResponse};
use russh::keys::agent::client::AgentClient;
use russh::keys::ssh_key::PublicKey;
use russh::keys::{PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::{ChannelMsg, Disconnect, MethodKind};
use russh_sftp::client::SftpSession;
use serde::Serialize;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::config::{self, Connection, HostConfig};
use super::known_hosts::{self, Verdict};
use super::prompt::{self, Prompt};
use super::secrets;

type Res<T> = Result<T, String>;

/// A session nothing used for this long is closed (tabs showing one of its folders, copies and
/// previews keep it in use).
pub const IDLE_AFTER: Duration = Duration::from_secs(15 * 60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

struct Client {
    name: String,
    /// What the server's key is recorded under, and where.
    host: String,
    port: u16,
    known: Vec<PathBuf>,
    /// Why the key was refused, for the error message.
    refused: Arc<Mutex<Option<String>>>,
}

impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(&mut self, server: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let key: PublicKey = match server {
            PublicKeyOrCertificate::PublicKey { key, .. } => key.clone(),
            PublicKeyOrCertificate::Certificate(cert) => PublicKey::from(cert.public_key().clone()),
        };
        let refuse = |why: String| {
            *self.refused.lock().unwrap() = Some(why);
            Ok(false)
        };
        match known_hosts::check(&self.known, &self.host, self.port, &key) {
            Verdict::Known => Ok(true),
            Verdict::Revoked => refuse(format!("The key of “{}” is marked revoked in your known_hosts file", self.host)),
            Verdict::Changed { file, line } => refuse(format!(
                "The key of “{}” isn't the one you trusted before. It may have been reinstalled, or someone may be \
                 between you and it. If you know why it changed, remove line {line} of {file} and connect again.",
                self.host
            )),
            Verdict::Unknown => {
                let fp = known_hosts::fingerprint(&key);
                let mut q = Prompt::confirm(
                    &self.name,
                    format!("Trust {}?", self.host),
                    format!(
                        "Ferret hasn't connected to {} before. Check that its key fingerprint is the one the server shows \
                         (ssh-keygen -lf /etc/ssh/ssh_host_{}_key.pub on it). Trusting it adds it to your known_hosts file, as ssh does.",
                        known_hosts::host_entry(&self.host, self.port),
                        key.algorithm().as_str().trim_start_matches("ssh-").split('-').next().unwrap_or("ed25519")
                    ),
                    "Trust and connect",
                );
                q.kind = "hostKey";
                q.fingerprint = Some(format!("{} {fp}", key.algorithm().as_str()));
                if prompt::ask(q).await.is_none() {
                    return refuse("You didn't trust the server's key".into());
                }
                if let Some(file) = self.known.first()
                    && let Err(e) = known_hosts::learn(file, &self.host, self.port, &key)
                {
                    log::warn!("remote: couldn't add {} to {}: {e}", self.host, file.display());
                }
                log::info!("remote: trusted the key of {} ({fp})", self.host);
                Ok(true)
            }
        }
    }
}

/// An open session.
pub struct Session {
    pub id: String,
    pub name: String,
    handle: Handle<Client>,
    pub sftp: SftpSession,
    /// The user's home on the server.
    pub home: String,
    /// `uname -m`, when the server runs commands (None: SFTP only, no helper).
    pub arch: Option<String>,
    last_used: Mutex<Instant>,
    /// Copies and other jobs using it right now.
    busy: AtomicUsize,
    /// The jump host's session this one goes through (ProxyJump), kept as long as it is.
    _jump: Option<Box<Session>>,
}

impl Session {
    pub fn touch(&self) {
        *self.last_used.lock().unwrap() = Instant::now();
    }

    pub fn is_closed(&self) -> bool {
        self.handle.is_closed()
    }

    /// Keeps the session from closing while the guard lives (a copy, say).
    pub fn hold(self: &Arc<Self>) -> Hold {
        self.busy.fetch_add(1, Ordering::Relaxed);
        self.touch();
        Hold(self.clone())
    }

    /// Runs `command` on the server (through the user's login shell, as `ssh host command`).
    pub async fn exec(&self, command: &str) -> Res<Proc> {
        self.touch();
        let channel = self.handle.channel_open_session().await.map_err(err)?;
        channel.exec(true, command).await.map_err(err)?;
        Ok(Proc::start(channel))
    }

    /// Runs `command` and gives back what it printed, if it ends well within `timeout`.
    pub async fn run(&self, command: &str, timeout: Duration) -> Res<String> {
        let mut p = self.exec(command).await?;
        let out = tokio::task::spawn_blocking(move || {
            let mut s = String::new();
            let _ = p.stdout.take().map(|mut o| o.read_to_string(&mut s));
            (s, p.wait())
        });
        match tokio::time::timeout(timeout, out).await {
            Ok(Ok((s, Some(0)))) => Ok(s),
            Ok(Ok((_, code))) => Err(format!("exit {}", code.map_or("none".to_string(), |c| c.to_string()))),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err("timed out".into()),
        }
    }
}

pub struct Hold(Arc<Session>);

impl Drop for Hold {
    fn drop(&mut self) {
        self.0.busy.fetch_sub(1, Ordering::Relaxed);
        self.0.touch();
    }
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

// ---------- command channels as processes ----------

/// A command running on the server, with blocking pipes (as `std::process::Child`), for the
/// code that drives Linux programs the same way in WSL and over SSH (linux.rs).
pub struct Proc {
    pub stdin: Option<ProcIn>,
    pub stdout: Option<ProcOut>,
    pub stderr: Option<ProcOut>,
    exit: std::sync::mpsc::Receiver<Option<u32>>,
    close: Option<tokio::sync::oneshot::Sender<()>>,
}

pub struct ProcIn(tokio::sync::mpsc::UnboundedSender<Vec<u8>>);

impl Write for ProcIn {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.send(buf.to_vec()).map_err(|_| std::io::Error::from(std::io::ErrorKind::BrokenPipe))?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub struct ProcOut {
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    buf: Vec<u8>,
    at: usize,
}

impl Read for ProcOut {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while self.at >= self.buf.len() {
            match self.rx.recv() {
                Ok(chunk) => {
                    self.buf = chunk;
                    self.at = 0;
                }
                Err(_) => return Ok(0),
            }
        }
        let n = out.len().min(self.buf.len() - self.at);
        out[..n].copy_from_slice(&self.buf[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

impl Proc {
    fn start(channel: russh::Channel<client::Msg>) -> Proc {
        let (mut read, write) = channel.split();
        let (out_tx, out_rx) = std::sync::mpsc::channel();
        let (err_tx, err_rx) = std::sync::mpsc::channel();
        let (exit_tx, exit_rx) = std::sync::mpsc::channel();
        let (in_tx, mut in_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        let (close_tx, mut close_rx) = tokio::sync::oneshot::channel::<()>();
        tauri::async_runtime::spawn(async move {
            let mut code = None;
            let mut input_open = true;
            loop {
                tokio::select! {
                    msg = read.wait() => match msg {
                        Some(ChannelMsg::Data { data }) => { let _ = out_tx.send(data.to_vec()); }
                        Some(ChannelMsg::ExtendedData { data, ext: 1 }) => { let _ = err_tx.send(data.to_vec()); }
                        Some(ChannelMsg::ExitStatus { exit_status }) => code = Some(exit_status),
                        Some(ChannelMsg::Close) | None => break,
                        Some(_) => {}
                    },
                    chunk = in_rx.recv(), if input_open => match chunk {
                        Some(bytes) => { let _ = write.data_bytes(bytes).await; }
                        None => { input_open = false; let _ = write.eof().await; }
                    },
                    _ = &mut close_rx => { let _ = write.close().await; break; }
                }
            }
            drop(out_tx);
            drop(err_tx);
            let _ = exit_tx.send(code);
        });
        Proc {
            stdin: Some(ProcIn(in_tx)),
            stdout: Some(ProcOut { rx: out_rx, buf: Vec::new(), at: 0 }),
            stderr: Some(ProcOut { rx: err_rx, buf: Vec::new(), at: 0 }),
            exit: exit_rx,
            close: Some(close_tx),
        }
    }

    /// Ends the command (its channel closes, so it loses its input and output).
    pub fn kill(&mut self) {
        if let Some(c) = self.close.take() {
            let _ = c.send(());
        }
    }

    /// Waits for the end; its exit code (None: no code, as when it was ended).
    pub fn wait(&mut self) -> Option<u32> {
        self.stdin = None;
        self.exit.recv().ok().flatten()
    }
}

// ---------- the sessions ----------

fn sessions() -> &'static Mutex<HashMap<String, Arc<Session>>> {
    static S: OnceLock<Mutex<HashMap<String, Arc<Session>>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

/// One connect at a time per connection: a second caller waits and gets the same session.
fn connect_lock(id: &str) -> Arc<tokio::sync::Mutex<()>> {
    static L: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    L.get_or_init(Default::default).lock().unwrap().entry(id.to_string()).or_default().clone()
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub id: String,
    /// "connecting" | "connected" | "disconnected"
    pub state: &'static str,
    pub error: Option<String>,
}

fn announce(id: &str, state: &'static str, error: Option<String>) {
    super::emit("remote-status", Status { id: id.to_string(), state, error });
}

/// The open session of a connection, if any (never connects: for background work).
pub fn connected(id: &str) -> Option<Arc<Session>> {
    let mut all = sessions().lock().unwrap();
    match all.get(id) {
        Some(s) if s.is_closed() => {
            all.remove(id);
            drop(all);
            announce(id, "disconnected", None);
            None
        }
        Some(s) => Some(s.clone()),
        None => None,
    }
}

pub fn connected_ids() -> Vec<String> {
    let ids: Vec<String> = sessions().lock().unwrap().keys().cloned().collect();
    ids.into_iter().filter(|id| connected(id).is_some()).collect()
}

/// The session of a connection, connecting first if need be (the user opened one of its folders).
pub async fn session(id: &str) -> Res<Arc<Session>> {
    if let Some(s) = connected(id) {
        s.touch();
        return Ok(s);
    }
    let lock = connect_lock(id);
    let _one = lock.lock().await;
    if let Some(s) = connected(id) {
        return Ok(s);
    }
    let conn = super::find_connection(id).ok_or_else(|| format!("There's no connection named “{id}” any more"))?;
    announce(id, "connecting", None);
    let started = Instant::now();
    match open(&conn).await {
        Ok(s) => {
            log::info!("remote: connected to {} ({}@{}:{}) in {:?}, home {}, {}", conn.name, s.0, conn.host, conn.port, started.elapsed(), s.1.home,
                s.1.arch.as_deref().map(|a| format!("runs commands ({a})")).unwrap_or_else(|| "SFTP only".into()));
            let s = Arc::new(s.1);
            sessions().lock().unwrap().insert(id.to_string(), s.clone());
            reaper();
            announce(id, "connected", None);
            Ok(s)
        }
        Err(e) => {
            log::warn!("remote: couldn't connect to {}: {e}", conn.name);
            announce(id, "disconnected", Some(e.clone()));
            Err(e)
        }
    }
}

pub async fn disconnect(id: &str) {
    let s = sessions().lock().unwrap().remove(id);
    if let Some(s) = s {
        let _ = s.handle.disconnect(Disconnect::ByApplication, "", "en").await;
        log::info!("remote: disconnected from {}", s.name);
    }
    super::forget_session(id);
    announce(id, "disconnected", None);
}

/// Closes sessions nothing used for IDLE_AFTER (checked every half minute).
fn reaper() {
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    tauri::async_runtime::spawn(async {
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            super::touch_shown();
            let idle: Vec<String> = sessions()
                .lock()
                .unwrap()
                .values()
                .filter(|s| s.busy.load(Ordering::Relaxed) == 0 && s.last_used.lock().unwrap().elapsed() >= IDLE_AFTER)
                .map(|s| s.id.clone())
                .collect();
            for id in idle {
                log::info!("remote: {id} unused for {} minutes, disconnecting", IDLE_AFTER.as_secs() / 60);
                disconnect(&id).await;
            }
        }
    });
}

// ---------- connecting ----------

/// Where to connect, after the config: (user, host, port, config).
fn target(conn: &Connection) -> (String, String, u16, HostConfig) {
    let cfg = conn.ssh_alias.as_deref().map(config::resolve).unwrap_or_default();
    let host = if conn.host.trim().is_empty() { cfg.host_name.clone().unwrap_or_else(|| cfg.alias.clone()) } else { conn.host.trim().to_string() };
    let user = if conn.user.trim().is_empty() { cfg.user.clone().unwrap_or_else(local_user) } else { conn.user.trim().to_string() };
    (user, host, conn.port, cfg)
}

fn local_user() -> String {
    std::env::var("USERNAME").unwrap_or_else(|_| "root".into()).to_lowercase()
}

fn client_config() -> Arc<client::Config> {
    Arc::new(client::Config {
        // A dead network shows within a minute and a half; a quiet session stays open.
        inactivity_timeout: None,
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 3,
        nodelay: true,
        ..Default::default()
    })
}

/// Connects and logs in; (user, session).
async fn open(conn: &Connection) -> Res<(String, Session)> {
    let (user, host, port, cfg) = target(conn);
    let known = config::known_hosts_files(&cfg);
    let key_host = cfg.host_key_alias.clone().unwrap_or_else(|| host.clone());
    let refused = Arc::new(Mutex::new(None));
    let client = Client { name: conn.name.clone(), host: key_host, port, known, refused: refused.clone() };
    let mut jump = None;
    let connected = match cfg.proxy_jump.as_deref() {
        Some(spec) => through_jump(spec, &host, port, client).await.map(|(h, j)| {
            jump = Some(Box::new(j));
            h
        }),
        None => {
            let tcp = tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::TcpStream::connect((host.as_str(), port)))
                .await
                .map_err(|_| format!("{host} didn't answer (port {port})"))?
                .map_err(|e| reach_error(&host, port, &e))?;
            let _ = tcp.set_nodelay(true);
            client::connect_stream(client_config(), tcp, client).await.map_err(err)
        }
    };
    let mut handle = connected.map_err(|e| refused.lock().unwrap().clone().unwrap_or(e))?;
    login(&mut handle, conn, &user, &host, &cfg).await?;

    let channel = handle.channel_open_session().await.map_err(err)?;
    channel.request_subsystem(true, "sftp").await.map_err(|e| format!("The server doesn't offer SFTP ({e})"))?;
    let sftp = SftpSession::new(channel.into_stream()).await.map_err(|e| format!("The server doesn't offer SFTP ({e})"))?;
    sftp.set_timeout(30);
    let home = sftp.canonicalize(".").await.unwrap_or_else(|_| "/".into());
    let mut s = Session {
        id: conn.id.clone(),
        name: conn.name.clone(),
        handle,
        sftp,
        home,
        arch: None,
        last_used: Mutex::new(Instant::now()),
        busy: AtomicUsize::new(0),
        _jump: jump,
    };
    // Servers that only allow SFTP (or a restricted shell) are browsed without the helper.
    s.arch = s.run("uname -sm", Duration::from_secs(8)).await.ok().and_then(|out| {
        let mut w = out.split_whitespace();
        (w.next() == Some("Linux")).then(|| w.next().unwrap_or_default().to_string())
    });
    Ok((user, s))
}

fn reach_error(host: &str, port: u16, e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::ConnectionRefused => format!("{host} refused the connection (nothing listens on port {port})"),
        std::io::ErrorKind::TimedOut => format!("{host} didn't answer (port {port})"),
        _ if e.raw_os_error() == Some(11001) => format!("Can't find “{host}” on the network"),
        _ => format!("Can't reach {host}: {e}"),
    }
}

/// ProxyJump: logs in to the jump host (as the config describes it) and goes on through it.
async fn through_jump(jump: &str, host: &str, port: u16, client: Client) -> Result<(Handle<Client>, Session), String> {
    // `user@host:port`, or a config alias; only the first of a chain (`a,b`) is used per hop.
    let hop = jump.split(',').next().unwrap_or(jump).trim();
    let (juser, rest) = hop.rsplit_once('@').map(|(u, r)| (Some(u.to_string()), r)).unwrap_or((None, hop));
    let (jhost, jport) = match rest.rsplit_once(':') {
        Some((h, p)) if p.parse::<u16>().is_ok() => (h.to_string(), p.parse().ok()),
        _ => (rest.to_string(), None),
    };
    let cfg = config::resolve(&jhost);
    let jconn = Connection {
        id: format!("jump:{jhost}"),
        name: jhost.clone(),
        kind: "ssh".into(),
        host: cfg.host_name.clone().unwrap_or_else(|| jhost.clone()),
        port: jport.or(cfg.port).unwrap_or(22),
        user: juser.or(cfg.user.clone()).unwrap_or_default(),
        ssh_alias: Some(jhost.clone()),
        auth: "auto".into(),
        identity_file: None,
        start_dir: None,
        search: false,
    };
    let (_, jsession) = Box::pin(open(&jconn)).await.map_err(|e| format!("Couldn't reach the jump host {jhost}: {e}"))?;
    let channel = jsession
        .handle
        .channel_open_direct_tcpip(host, port as u32, "127.0.0.1", 0)
        .await
        .map_err(|e| format!("The jump host {jhost} couldn't reach {host}: {e}"))?;
    let refused = client.refused.clone();
    let h = client::connect_stream(client_config(), channel.into_stream(), client).await;
    h.map(|h| (h, jsession)).map_err(|e| refused.lock().unwrap().clone().unwrap_or_else(|| e.to_string()))
}

/// Logs in, trying what `ssh` would (see the module's notes).
async fn login(h: &mut Handle<Client>, conn: &Connection, user: &str, host: &str, cfg: &HostConfig) -> Res<()> {
    let mut methods: Option<Vec<MethodKind>> = None;
    let allows = |m: &Option<Vec<MethodKind>>, k: MethodKind| m.as_ref().is_none_or(|set| set.contains(&k));
    let rsa_hash = h.best_supported_rsa_hash().await.ok().flatten().flatten();
    let files = if conn.auth == "password" { vec![] } else { config::identity_files(conn, cfg) };

    if conn.auth != "password" {
        // The agent's keys (only those of the key files, with IdentitiesOnly).
        let only: Option<Vec<PublicKey>> = (cfg.identities_only && !files.is_empty()).then(|| {
            files.iter().filter_map(|f| russh::keys::load_public_key(f.with_extension("pub")).ok()).collect()
        });
        // (The end-to-end tests use their own key only.)
        if conn.auth == "auto"
            && config::test_ssh_dir().is_none()
            && let Some(done) = agent_login(h, user, rsa_hash, only.as_deref(), &mut methods).await?
        {
            return Ok(done);
        }
        for file in &files {
            if !allows(&methods, MethodKind::PublicKey) {
                break;
            }
            let Some(key) = load_key(&conn.name, file).await else { continue };
            let res = h
                .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), rsa_hash))
                .await
                .map_err(err)?;
            match res {
                russh::client::AuthResult::Success => {
                    log::info!("remote: logged in to {} with {}", conn.name, file.display());
                    return Ok(());
                }
                russh::client::AuthResult::Failure { remaining_methods, .. } => methods = Some(remaining_methods.to_vec()),
            }
        }
        if conn.auth == "key" {
            return Err(format!("{host} didn't accept the key file"));
        }
    }

    // A password, or the server's own questions.
    let target = secrets::password_target(user, host, conn.port);
    let mut remembered = secrets::get(&target);
    let mut wrong_password = false;
    for attempt in 0..3 {
        if allows(&methods, MethodKind::Password) {
            let (password, remember) = match remembered.take() {
                Some(p) => (p, false),
                None => {
                    let message = if attempt == 0 { format!("Password for {user}@{host}") } else { format!("That password didn't work. Password for {user}@{host}") };
                    let q = Prompt::secret(&conn.name, format!("Connect to {}", conn.name), message, "Password", true);
                    let Some(a) = prompt::ask(q).await else { return Err("Cancelled".into()) };
                    (a.values.into_iter().next().unwrap_or_default(), a.remember)
                }
            };
            match h.authenticate_password(user, password.clone()).await.map_err(err)? {
                russh::client::AuthResult::Success => {
                    if remember {
                        let _ = secrets::set(&target, &password);
                    }
                    log::info!("remote: logged in to {} with a password", conn.name);
                    return Ok(());
                }
                russh::client::AuthResult::Failure { remaining_methods, .. } => {
                    secrets::forget(&target);
                    wrong_password = true;
                    methods = Some(remaining_methods.to_vec());
                }
            }
        } else if allows(&methods, MethodKind::KeyboardInteractive) {
            if keyboard_interactive(h, conn, user).await? {
                return Ok(());
            }
        } else {
            break;
        }
    }
    // Some servers take one password per connection: connecting again asks again.
    Err(if wrong_password { format!("{host} didn't take the password for {user}") } else { format!("{host} didn't let {user} in") })
}

/// The SSH agent's keys; Some(()) when one let the user in.
async fn agent_login(
    h: &mut Handle<Client>,
    user: &str,
    rsa_hash: Option<russh::keys::HashAlg>,
    only: Option<&[PublicKey]>,
    methods: &mut Option<Vec<MethodKind>>,
) -> Res<Option<()>> {
    #[cfg(windows)]
    {
        let mut agents: Vec<AgentClient<Box<dyn russh::keys::agent::client::AgentStream + Send + Unpin>>> = Vec::new();
        if let Ok(a) = AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent").await {
            agents.push(a.dynamic());
        }
        if let Ok(a) = AgentClient::connect_pageant().await {
            agents.push(a.dynamic());
        }
        for mut agent in agents {
            let Ok(ids) = agent.request_identities().await else { continue };
            for id in ids {
                let key = id.public_key().into_owned();
                if only.is_some_and(|keys| !keys.iter().any(|k| k.key_data() == key.key_data())) {
                    continue;
                }
                match h.authenticate_publickey_with(user, key, rsa_hash, &mut agent).await {
                    Ok(russh::client::AuthResult::Success) => {
                        log::info!("remote: logged in with a key from the SSH agent");
                        return Ok(Some(()));
                    }
                    Ok(russh::client::AuthResult::Failure { remaining_methods, .. }) => *methods = Some(remaining_methods.to_vec()),
                    Err(_) => {}
                }
            }
        }
    }
    #[cfg(not(windows))]
    let _ = (h, user, rsa_hash, only, methods);
    Ok(None)
}

/// A key file, with its passphrase (remembered, or asked for) if it has one.
async fn load_key(name: &str, file: &std::path::Path) -> Option<russh::keys::PrivateKey> {
    match russh::keys::load_secret_key(file, None) {
        Ok(k) => return Some(k),
        Err(russh::keys::Error::KeyIsEncrypted) => {}
        Err(e) => {
            log::info!("remote: skipping key {}: {e}", file.display());
            return None;
        }
    }
    let target = secrets::passphrase_target(file);
    if let Some(p) = secrets::get(&target) {
        if let Ok(k) = russh::keys::load_secret_key(file, Some(&p)) {
            return Some(k);
        }
        secrets::forget(&target);
    }
    let shown = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    for attempt in 0..3 {
        let message = if attempt == 0 { format!("The key {shown} is protected with a passphrase") } else { format!("That passphrase didn't open {shown}. Try again") };
        let q = Prompt::secret(name, format!("Unlock {shown}"), message, "Passphrase", true);
        let a = prompt::ask(q).await?;
        let pass = a.values.into_iter().next().unwrap_or_default();
        if let Ok(k) = russh::keys::load_secret_key(file, Some(&pass)) {
            if a.remember {
                let _ = secrets::set(&target, &pass);
            }
            return Some(k);
        }
    }
    None
}

/// The server's own questions (one-time codes, a password asked its own way).
async fn keyboard_interactive(h: &mut Handle<Client>, conn: &Connection, user: &str) -> Res<bool> {
    let mut reply = h.authenticate_keyboard_interactive_start(user, None).await.map_err(err)?;
    for _ in 0..6 {
        match reply {
            KeyboardInteractiveAuthResponse::Success => return Ok(true),
            KeyboardInteractiveAuthResponse::Failure { .. } => return Ok(false),
            KeyboardInteractiveAuthResponse::InfoRequest { name, instructions, prompts } => {
                let answers = if prompts.is_empty() {
                    vec![]
                } else {
                    let mut q = Prompt::secret(&conn.name, if name.is_empty() { format!("Connect to {}", conn.name) } else { name }, instructions, "", false);
                    q.fields = prompts.iter().map(|p| prompt::Field { label: p.prompt.trim().to_string(), secret: !p.echo }).collect();
                    match prompt::ask(q).await {
                        Some(a) => a.values,
                        None => return Err("Cancelled".into()),
                    }
                };
                reply = h.authenticate_keyboard_interactive_respond(answers).await.map_err(err)?;
            }
        }
    }
    Ok(false)
}

/// Quotes a word for the server's shell.
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}
