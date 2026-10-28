//! A small SSH server for Ferret's end-to-end tests: SFTP over a local folder, on 127.0.0.1.
//!
//!   ferret-test-sshd keygen <file>                     a test key (<file> and <file>.pub)
//!   ferret-test-sshd serve <root> <authorized.pub> [user:password]
//!
//! `serve` prints `port <n>` once it listens. It lets in whoever has the authorized key (any user
//! name), or the one user with that password. The SFTP root is <root>; a user's home is
//! /home/<user>. Commands aren't run (an exec request ends with exit 127), so Ferret browses it
//! as an SFTP-only server. It ends when its stdin closes (the test runner went away).

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use russh::keys::{PrivateKey, PublicKey, ssh_key::Algorithm};
use russh::server::{Auth, Msg, Server as _, Session};
use russh::{Channel, ChannelId};
use russh_sftp::protocol::{Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode, Version};
use tokio::sync::Mutex;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("keygen") => {
            let file = PathBuf::from(&args[1]);
            let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
            std::fs::write(&file, key.to_openssh(Default::default()).unwrap().as_bytes()).unwrap();
            std::fs::write(file.with_extension("pub"), key.public_key().to_openssh().unwrap() + "\n").unwrap();
        }
        Some("serve") => {
            let root = PathBuf::from(&args[1]);
            let authorized = PublicKey::from_openssh(std::fs::read_to_string(&args[2]).unwrap().trim()).unwrap();
            let password = args.get(3).and_then(|p| p.split_once(':')).map(|(u, p)| (u.to_string(), p.to_string()));
            tokio::runtime::Runtime::new().unwrap().block_on(serve(root, authorized, password));
        }
        _ => eprintln!("usage: ferret-test-sshd keygen <file> | serve <root> <authorized.pub> [user:password]"),
    }
}

async fn serve(root: PathBuf, authorized: PublicKey, password: Option<(String, String)>) {
    let config = Arc::new(russh::server::Config {
        auth_rejection_time: Duration::from_millis(50),
        auth_rejection_time_initial: Some(Duration::ZERO),
        keys: vec![PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap()],
        ..Default::default()
    });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    println!("port {}", listener.local_addr().unwrap().port());
    std::io::stdout().flush().unwrap();
    // The runner keeps stdin open while it needs the server.
    std::thread::spawn(|| {
        let _ = std::io::stdin().read_to_end(&mut Vec::new());
        std::process::exit(0);
    });
    let mut server = Server { root: Arc::new(root), authorized: Arc::new(authorized), password: Arc::new(password) };
    server.run_on_socket(config, &listener).await.unwrap();
}

#[derive(Clone)]
struct Server {
    root: Arc<PathBuf>,
    authorized: Arc<PublicKey>,
    password: Arc<Option<(String, String)>>,
}

impl russh::server::Server for Server {
    type Handler = Client;
    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Client {
        Client { server: self.clone(), user: String::new(), channels: Arc::default() }
    }
}

struct Client {
    server: Server,
    user: String,
    channels: Arc<Mutex<HashMap<ChannelId, Channel<Msg>>>>,
}

impl russh::server::Handler for Client {
    type Error = russh::Error;

    async fn auth_publickey(&mut self, user: &str, key: russh::server::VerifiedPublicKey<'_>) -> Result<Auth, Self::Error> {
        if key.public_key.key_data() == self.server.authorized.key_data() {
            self.user = user.to_string();
            return Ok(Auth::Accept);
        }
        Ok(Auth::reject())
    }

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        match &*self.server.password {
            Some((u, p)) if u == user && p == password => {
                self.user = user.to_string();
                Ok(Auth::Accept)
            }
            _ => Ok(Auth::reject()),
        }
    }

    async fn channel_open_session(&mut self, channel: Channel<Msg>, reply: russh::server::ChannelOpenHandle, _: &mut Session) -> Result<(), Self::Error> {
        self.channels.lock().await.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn exec_request(&mut self, channel: ChannelId, _data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        session.exit_status_request(channel, 127)?;
        session.eof(channel)?;
        session.close(channel)?;
        Ok(())
    }

    async fn subsystem_request(&mut self, channel: ChannelId, name: &str, session: &mut Session) -> Result<(), Self::Error> {
        let Some(ch) = self.channels.lock().await.remove(&channel) else { return Ok(()) };
        if name != "sftp" {
            session.channel_failure(channel)?;
            return Ok(());
        }
        let home = format!("/home/{}", if self.user.is_empty() { "tester" } else { &self.user });
        let _ = std::fs::create_dir_all(local(&self.server.root, &home));
        session.channel_success(channel)?;
        let sftp = Sftp { root: self.server.root.clone(), home, handles: HashMap::new(), next: 0 };
        tokio::spawn(russh_sftp::server::run(ch.into_stream(), sftp));
        Ok(())
    }
}

// ---------- SFTP over a local folder ----------

enum Open {
    File(std::fs::File),
    Dir(Option<Vec<File>>),
}

struct Sftp {
    root: Arc<PathBuf>,
    home: String,
    handles: HashMap<String, Open>,
    next: u64,
}

/// `/a/b` as a path under the root (`..` can't leave it).
fn local(root: &Path, sftp: &str) -> PathBuf {
    let mut out = root.to_path_buf();
    for c in Path::new(&sftp.replace('\\', "/")).components() {
        if let Component::Normal(n) = c {
            out.push(n);
        }
    }
    out
}

/// An absolute, tidy SFTP path (`.` and `..` resolved; relative ones from the home).
fn absolute(home: &str, p: &str) -> String {
    let full = if p.starts_with('/') { p.to_string() } else { format!("{home}/{p}") };
    let mut parts: Vec<&str> = Vec::new();
    for part in full.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            x => parts.push(x),
        }
    }
    format!("/{}", parts.join("/"))
}

fn err(e: std::io::Error) -> StatusCode {
    match e.kind() {
        std::io::ErrorKind::NotFound => StatusCode::NoSuchFile,
        std::io::ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
        _ => StatusCode::Failure,
    }
}

fn ok(id: u32) -> Status {
    Status { id, status_code: StatusCode::Ok, error_message: "Ok".into(), language_tag: "en-US".into() }
}

impl Sftp {
    fn path(&self, p: &str) -> PathBuf {
        local(&self.root, &absolute(&self.home, p))
    }

    fn handle(&mut self, open: Open) -> String {
        self.next += 1;
        let h = format!("h{}", self.next);
        self.handles.insert(h.clone(), open);
        h
    }
}

fn set(path: &Path, attrs: &FileAttributes) -> Result<(), StatusCode> {
    if let Some(size) = attrs.size {
        std::fs::OpenOptions::new().write(true).open(path).and_then(|f| f.set_len(size)).map_err(err)?;
    }
    if let Some(mtime) = attrs.mtime {
        let t = UNIX_EPOCH + Duration::from_secs(mtime as u64);
        let dir = path.is_dir();
        if let Ok(f) = std::fs::File::options().write(!dir).read(dir).open(path) {
            let _ = f.set_modified(t);
        }
    }
    Ok(())
}

impl russh_sftp::server::Handler for Sftp {
    type Error = StatusCode;

    fn unimplemented(&self) -> StatusCode {
        StatusCode::OpUnsupported
    }

    async fn init(&mut self, _version: u32, _ext: HashMap<String, String>) -> Result<Version, StatusCode> {
        Ok(Version::new())
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, StatusCode> {
        Ok(Name { id, files: vec![File::dummy(absolute(&self.home, &path))] })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        let m = std::fs::metadata(self.path(&path)).map_err(err)?;
        Ok(Attrs { id, attrs: FileAttributes::from(&m) })
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        let m = std::fs::symlink_metadata(self.path(&path)).map_err(err)?;
        Ok(Attrs { id, attrs: FileAttributes::from(&m) })
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, StatusCode> {
        match self.handles.get(&handle) {
            Some(Open::File(f)) => Ok(Attrs { id, attrs: FileAttributes::from(&f.metadata().map_err(err)?) }),
            _ => Err(StatusCode::Failure),
        }
    }

    async fn setstat(&mut self, id: u32, path: String, attrs: FileAttributes) -> Result<Status, StatusCode> {
        set(&self.path(&path), &attrs).map(|_| ok(id))
    }

    async fn fsetstat(&mut self, id: u32, handle: String, attrs: FileAttributes) -> Result<Status, StatusCode> {
        match self.handles.get(&handle) {
            Some(Open::File(f)) => {
                if let Some(mtime) = attrs.mtime {
                    let _ = f.set_modified(UNIX_EPOCH + Duration::from_secs(mtime as u64));
                }
                Ok(ok(id))
            }
            _ => Err(StatusCode::Failure),
        }
    }

    async fn open(&mut self, id: u32, filename: String, pflags: OpenFlags, _attrs: FileAttributes) -> Result<Handle, StatusCode> {
        let mut o = std::fs::OpenOptions::new();
        o.read(pflags.contains(OpenFlags::READ))
            .write(pflags.contains(OpenFlags::WRITE) || pflags.contains(OpenFlags::APPEND))
            .append(pflags.contains(OpenFlags::APPEND));
        if pflags.contains(OpenFlags::EXCLUDE) {
            o.create_new(true);
        } else if pflags.contains(OpenFlags::CREATE) {
            o.create(true);
        }
        if pflags.contains(OpenFlags::TRUNCATE) {
            o.truncate(true);
        }
        let f = o.open(self.path(&filename)).map_err(|e| if e.kind() == std::io::ErrorKind::AlreadyExists { StatusCode::Failure } else { err(e) })?;
        Ok(Handle { id, handle: self.handle(Open::File(f)) })
    }

    async fn read(&mut self, id: u32, handle: String, offset: u64, len: u32) -> Result<Data, StatusCode> {
        let Some(Open::File(f)) = self.handles.get_mut(&handle) else { return Err(StatusCode::Failure) };
        f.seek(SeekFrom::Start(offset)).map_err(err)?;
        let mut buf = vec![0; len as usize];
        let n = f.read(&mut buf).map_err(err)?;
        if n == 0 && len > 0 {
            return Err(StatusCode::Eof);
        }
        buf.truncate(n);
        Ok(Data { id, data: buf })
    }

    async fn write(&mut self, id: u32, handle: String, offset: u64, data: Vec<u8>) -> Result<Status, StatusCode> {
        let Some(Open::File(f)) = self.handles.get_mut(&handle) else { return Err(StatusCode::Failure) };
        f.seek(SeekFrom::Start(offset)).map_err(err)?;
        f.write_all(&data).map_err(err)?;
        Ok(ok(id))
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, StatusCode> {
        self.handles.remove(&handle);
        Ok(ok(id))
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, StatusCode> {
        let mut files = Vec::new();
        for e in std::fs::read_dir(self.path(&path)).map_err(err)?.flatten() {
            if let Ok(m) = e.metadata() {
                files.push(File::new(e.file_name().to_string_lossy(), FileAttributes::from(&m)));
            }
        }
        Ok(Handle { id, handle: self.handle(Open::Dir(Some(files))) })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, StatusCode> {
        match self.handles.get_mut(&handle) {
            Some(Open::Dir(files)) => match files.take() {
                Some(files) => Ok(Name { id, files }),
                None => Err(StatusCode::Eof),
            },
            _ => Err(StatusCode::Failure),
        }
    }

    async fn mkdir(&mut self, id: u32, path: String, _attrs: FileAttributes) -> Result<Status, StatusCode> {
        std::fs::create_dir(self.path(&path)).map_err(err).map(|_| ok(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, StatusCode> {
        std::fs::remove_dir(self.path(&path)).map_err(err).map(|_| ok(id))
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, StatusCode> {
        std::fs::remove_file(self.path(&filename)).map_err(err).map(|_| ok(id))
    }

    async fn rename(&mut self, id: u32, oldpath: String, newpath: String) -> Result<Status, StatusCode> {
        // SFTP's rename doesn't replace (as OpenSSH's sftp-server).
        let to = self.path(&newpath);
        if std::fs::symlink_metadata(&to).is_ok() {
            return Err(StatusCode::Failure);
        }
        std::fs::rename(self.path(&oldpath), to).map_err(err).map(|_| ok(id))
    }
}
