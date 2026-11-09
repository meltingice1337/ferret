//! Named-pipe protocol between Ferret and the FerretIndexer service.
//!
//! Pipe: `\\.\pipe\ferret-index` (local clients only; DACL: Authenticated
//! Users read/write, SYSTEM + Administrators full). Messages are a u32
//! little-endian length followed by JSON (`Request` → `Response`).
//!
//! Privacy: the service can see every file name on the machine. Before
//! returning results it impersonates the pipe client
//! (`ImpersonateNamedPipeClient`) and drops hits whose parent folder the
//! caller cannot list (cached per folder for 60 s per connection).

use super::engine::{SearchRequest, SearchResults, VolumeStatus};
use serde::{Deserialize, Serialize};

pub const PIPE_NAME: &str = r"\\.\pipe\ferret-index";
/// Bump when the service binary must be replaced: a change to the indexer, the snapshot
/// format or this protocol. The app offers to update an installed service that reports a
/// lower number; app releases that leave it alone need no update. From 2, the service updates
/// itself (`Request::Update`, no UAC prompt); older ones are reinstalled (one UAC prompt). 3:
/// folder sizes (`Request::Sizes`).
pub const INDEXER_VERSION: u32 = 3;
const MAX_MSG: usize = 64 << 20;

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Request {
    Query(SearchRequest),
    Status,
    Ping,
    /// Replace the service with this `ferret-indexer.exe` (Ferret's own copy), if it's a newer
    /// release, signed with Ferret's release key (see `service::update_from`).
    Update { exe: String },
    /// The sizes of these folders, from the file table (indexer 3 on).
    Sizes { paths: Vec<String> },
}

/// A folder's size as the asking user would count it: what's inside, without what's in folders
/// inside that they can't open (then `partial`).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct FolderTotal {
    pub bytes: u64,
    pub files: u64,
    pub partial: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct ServiceStatus {
    pub version: String,
    /// [`INDEXER_VERSION`] of the running service; 0 for services that predate the field.
    #[serde(default)]
    pub indexer: u32,
    pub volumes: Vec<VolumeStatus>,
    pub building: bool,
    pub errors: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Response {
    Results(SearchResults),
    Status(ServiceStatus),
    Pong,
    /// The update was checked and starts now: the service restarts in a few seconds.
    Updating,
    /// For each folder asked for: its size, or none (not on a drive the service has read yet,
    /// or one the user can't open).
    Sizes { sizes: Vec<Option<FolderTotal>> },
    Error { message: String },
}

pub fn encode<T: Serialize>(v: &T) -> Vec<u8> {
    let body = serde_json::to_vec(v).unwrap_or_default();
    let mut out = Vec::with_capacity(body.len() + 4);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

pub fn read_frame(r: &mut impl std::io::Read) -> std::io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let n = u32::from_le_bytes(len) as usize;
    if n > MAX_MSG {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "message too large"));
    }
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

#[cfg(windows)]
pub use win::*;

#[cfg(windows)]
mod win {
    use super::*;
    use crate::search::engine::Engine;
    use parking_lot::Mutex;
    use std::collections::HashMap;
    use std::fs::File;
    use std::io::Write;
    use std::os::windows::io::FromRawHandle;
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{
        CloseHandle, LocalFree, ERROR_IO_PENDING, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE,
        HANDLE, HLOCAL, WAIT_OBJECT_0,
    };
    use windows::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
    use windows::Win32::Security::{RevertToSelf, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, ReadFile, WriteFile, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
        FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_MODE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        PIPE_ACCESS_DUPLEX,
    };
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, ImpersonateNamedPipeClient, WaitNamedPipeW,
        PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
    use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    /// Owned security descriptor built from SDDL.
    pub struct Sddl(PSECURITY_DESCRIPTOR);
    impl Sddl {
        pub fn new(sddl: &str) -> windows::core::Result<Sddl> {
            let mut sd = PSECURITY_DESCRIPTOR::default();
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(PCWSTR(wide(sddl).as_ptr()), SDDL_REVISION_1, &mut sd, None)?;
            }
            Ok(Sddl(sd))
        }
        pub fn ptr(&self) -> PSECURITY_DESCRIPTOR {
            self.0
        }
    }
    impl Drop for Sddl {
        fn drop(&mut self) {
            unsafe {
                let _ = LocalFree(Some(HLOCAL(self.0 .0)));
            }
        }
    }

    /// Authenticated Users: read/write without FILE_CREATE_PIPE_INSTANCE
    /// (0x12019b), so nobody else can squat server instances.
    const PIPE_SDDL: &str = "D:(A;;0x12019b;;;AU)(A;;GA;;;SY)(A;;GA;;;BA)";

    // ------------------------------------------------------------ server

    /// Accept loop; returns when `stop` is set (call `wake_server()` after).
    pub fn serve(
        engine: Arc<Engine>,
        stop: Arc<AtomicBool>,
        errors: Arc<Mutex<Vec<String>>>,
        log: Arc<dyn Fn(String) + Send + Sync>,
    ) -> std::io::Result<()> {
        let sd = Sddl::new(PIPE_SDDL).map_err(|e| std::io::Error::other(e.to_string()))?;
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.ptr().0,
            bInheritHandle: false.into(),
        };
        let name = wide(PIPE_NAME);
        let mut first = true;
        while !stop.load(Relaxed) {
            let mode = if first { PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE } else { PIPE_ACCESS_DUPLEX };
            let h = unsafe {
                CreateNamedPipeW(
                    PCWSTR(name.as_ptr()),
                    mode,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    PIPE_UNLIMITED_INSTANCES,
                    64 * 1024,
                    64 * 1024,
                    0,
                    Some(&sa),
                )
            };
            if h.is_invalid() {
                let e = std::io::Error::last_os_error();
                if first {
                    // Another process owns the pipe name: refuse to run beside it.
                    return Err(std::io::Error::other(format!("cannot create {PIPE_NAME}: {e}")));
                }
                log(format!("CreateNamedPipe: {e}"));
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
            first = false;
            let connected = match unsafe { ConnectNamedPipe(h, None) } {
                Ok(()) => true,
                Err(e) => e.code() == ERROR_PIPE_CONNECTED.to_hresult(),
            };
            if stop.load(Relaxed) || !connected {
                unsafe {
                    let _ = CloseHandle(h);
                }
                continue;
            }
            let eng = engine.clone();
            let errs = errors.clone();
            let raw = h.0 as isize;
            let _ = std::thread::Builder::new()
                .name("ferret-pipe-client".into())
                .spawn(move || handle_client(raw, &eng, &errs));
        }
        Ok(())
    }

    /// Unblocks a server waiting in ConnectNamedPipe (after setting stop).
    pub fn wake_server() {
        if let Ok(h) = unsafe {
            CreateFileW(
                PCWSTR(wide(PIPE_NAME).as_ptr()),
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_MODE(0),
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )
        } {
            unsafe {
                let _ = CloseHandle(h);
            }
        }
    }

    fn handle_client(raw: isize, engine: &Arc<Engine>, errors: &Arc<Mutex<Vec<String>>>) {
        let h = HANDLE(raw as *mut _);
        // `File` gives us blocking Read/Write over the pipe handle.
        let mut f = unsafe { File::from_raw_handle(raw as *mut _) };
        let mut access = AccessCache::default();
        let mut size_access = SizeAccess::default();
        loop {
            let Ok(msg) = read_frame(&mut f) else { break };
            let resp = match serde_json::from_slice::<Request>(&msg) {
                Ok(Request::Sizes { paths }) => Response::Sizes { sizes: sizes_filtered(engine, &paths, h, &mut size_access) },
                Ok(Request::Query(q)) => match query_filtered(engine, &q, h, &mut access) {
                    Ok(r) => Response::Results(r),
                    Err(e) => Response::Error { message: e },
                },
                Ok(Request::Status) => Response::Status(ServiceStatus {
                    version: env!("CARGO_PKG_VERSION").into(),
                    indexer: INDEXER_VERSION,
                    volumes: engine.status(),
                    building: engine.building(),
                    errors: errors.lock().clone(),
                }),
                Ok(Request::Ping) => Response::Pong,
                Ok(Request::Update { exe }) => match crate::search::service::update_from(&exe) {
                    Ok(()) => Response::Updating,
                    Err(message) => Response::Error { message },
                },
                Err(e) => Response::Error { message: format!("bad request: {e}") },
            };
            if f.write_all(&encode(&resp)).is_err() {
                break;
            }
        }
        unsafe {
            let _ = DisconnectNamedPipe(h);
        }
        drop(f); // closes the handle
    }

    /// Per-connection cache: folder (lower-case) → (caller may list it, when).
    #[derive(Default)]
    struct AccessCache(HashMap<String, (bool, Instant)>);

    impl AccessCache {
        /// Must be called while impersonating the client.
        fn can_list(&mut self, dir: &str) -> bool {
            let key = dir.to_lowercase();
            if let Some(&(ok, t)) = self.0.get(&key) {
                if t.elapsed() < Duration::from_secs(60) {
                    return ok;
                }
            }
            const FILE_LIST_DIRECTORY: u32 = 1;
            let ok = match unsafe {
                CreateFileW(
                    PCWSTR(wide(&crate::paths::for_io(dir)).as_ptr()),
                    FILE_LIST_DIRECTORY,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    None,
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS,
                    None,
                )
            } {
                Ok(h) => {
                    unsafe {
                        let _ = CloseHandle(h);
                    }
                    true
                }
                Err(_) => false,
            };
            if self.0.len() > 50_000 {
                self.0.clear();
            }
            self.0.insert(key, (ok, Instant::now()));
            ok
        }
    }

    /// Per connection: whether the client may list folders with a given security id, per drive.
    #[derive(Default)]
    struct SizeAccess(HashMap<(u8, u32), bool>);

    /// A folder opened to look at it (as whoever is asking): its volume serial number and file
    /// reference. With `list`, only if it may be listed.
    fn folder_id(path: &str, list: bool) -> Option<(u32, u64)> {
        use windows::Win32::Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION};
        const FILE_LIST_DIRECTORY: u32 = 1;
        const FILE_READ_ATTRIBUTES: u32 = 0x80;
        let h = unsafe {
            CreateFileW(
                PCWSTR(wide(&crate::paths::for_io(path)).as_ptr()),
                if list { FILE_LIST_DIRECTORY } else { FILE_READ_ATTRIBUTES },
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                None,
            )
        }
        .ok()?;
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        let got = unsafe { GetFileInformationByHandle(h, &mut info) };
        unsafe {
            let _ = CloseHandle(h);
        }
        got.ok()?;
        Some((info.dwVolumeSerialNumber, (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow)))
    }

    /// May whoever is asking list the folder with this file reference? (`hint`: any handle on its
    /// drive.) None if it's not there any more.
    fn can_list_by_id(hint: HANDLE, frn: u64) -> Option<bool> {
        use windows::Win32::Foundation::ERROR_ACCESS_DENIED;
        use windows::Win32::Storage::FileSystem::{FileIdType, OpenFileById, FILE_ID_DESCRIPTOR, FILE_ID_DESCRIPTOR_0};
        const FILE_LIST_DIRECTORY: u32 = 1;
        let id = FILE_ID_DESCRIPTOR {
            dwSize: std::mem::size_of::<FILE_ID_DESCRIPTOR>() as u32,
            Type: FileIdType,
            Anonymous: FILE_ID_DESCRIPTOR_0 { FileId: frn as i64 },
        };
        match unsafe {
            OpenFileById(hint, &id, FILE_LIST_DIRECTORY, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, None, FILE_FLAG_BACKUP_SEMANTICS)
        } {
            Ok(h) => {
                unsafe {
                    let _ = CloseHandle(h);
                }
                Some(true)
            }
            Err(e) if e.code() == ERROR_ACCESS_DENIED.to_hresult() => Some(false),
            Err(_) => None,
        }
    }

    /// `Request::Sizes`: each folder's total from the file table (sizes.rs), as Ferret's own walk
    /// would count it as the client: only for a folder the client may list, and without what's
    /// in the folders inside that they may not (then `partial`). The table has everything; who
    /// may list what is asked as the client, once for each security id a folder can have (a drive
    /// has a few thousand), and the topmost folders the client can't list come off the totals.
    fn sizes_filtered(engine: &Engine, paths: &[String], pipe: HANDLE, access: &mut SizeAccess) -> Vec<Option<FolderTotal>> {
        let mut out = vec![None; paths.len()];
        for vol in engine.volumes() {
            let mine: Vec<usize> = (0..paths.len()).filter(|&i| paths[i].get(..vol.root.len()).is_some_and(|p| p.eq_ignore_ascii_case(&vol.root))).collect();
            if mine.is_empty() {
                continue;
            }
            let guard = vol.sizes.read();
            let Some(tree) = guard.as_ref() else { continue };
            let letter = vol.root.as_bytes().first().copied().unwrap_or(b'C').to_ascii_uppercase();
            // As the service: a handle on the drive (OpenFileById needs one), and its serial.
            let Some((serial, _)) = folder_id(&vol.root, false) else { continue };
            let Ok(hint) = (unsafe {
                CreateFileW(
                    PCWSTR(wide(&vol.root).as_ptr()),
                    0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    None,
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS,
                    None,
                )
            }) else {
                continue;
            };
            {
                let Ok(_as_client) = Impersonation::start(pipe) else {
                    unsafe {
                        let _ = CloseHandle(hint);
                    }
                    break;
                };
                let mut denied = super::super::index::FastSet::default();
                for (r, d) in tree.dirs() {
                    let ok = match access.0.get(&(letter, d.security)) {
                        Some(&ok) => ok,
                        None => match can_list_by_id(hint, tree.frn(r)) {
                            Some(ok) => *access.0.entry((letter, d.security)).or_insert(ok),
                            None => true,
                        },
                    };
                    if !ok {
                        denied.insert(r);
                    }
                }
                let topmost: Vec<u32> = denied
                    .iter()
                    .copied()
                    .filter(|&d| {
                        let mut cur = d;
                        while let Some(p) = tree.up(cur) {
                            if denied.contains(&p) {
                                return false;
                            }
                            cur = p;
                        }
                        true
                    })
                    .collect();
                for i in mine {
                    let Some((s, frn)) = folder_id(&paths[i], true) else { continue };
                    let r = (frn & 0x0000_FFFF_FFFF_FFFF) as u32;
                    let Some(d) = tree.dir(r).filter(|_| s == serial) else { continue };
                    let mut total = FolderTotal { bytes: d.bytes, files: d.files, partial: false };
                    for &x in &topmost {
                        if let Some(xd) = tree.dir(x).filter(|_| tree.inside(x, r)) {
                            total.bytes = total.bytes.saturating_sub(xd.bytes);
                            total.files = total.files.saturating_sub(xd.files);
                            total.partial = true;
                        }
                    }
                    out[i] = Some(total);
                }
            }
            unsafe {
                let _ = CloseHandle(hint);
            }
        }
        out
    }

    /// Reverts impersonation even on early return / panic.
    struct Impersonation;
    impl Impersonation {
        fn start(pipe: HANDLE) -> Result<Self, String> {
            unsafe { ImpersonateNamedPipeClient(pipe) }.map_err(|e| format!("cannot impersonate client: {e}"))?;
            Ok(Impersonation)
        }
    }
    impl Drop for Impersonation {
        fn drop(&mut self) {
            if unsafe { RevertToSelf() }.is_err() {
                // Never keep running under a client's token.
                std::process::abort();
            }
        }
    }

    /// Runs the query and drops results in folders the client can't list,
    /// over-fetching so `limit` results survive the filter when possible.
    fn query_filtered(engine: &Engine, q: &SearchRequest, pipe: HANDLE, cache: &mut AccessCache) -> Result<SearchResults, String> {
        let t0 = Instant::now();
        let want = if q.limit == 0 { 50 } else { q.limit.min(2_000) } as usize;
        let mut fetch = want * 2 + 20;
        loop {
            let mut r = engine.search(&SearchRequest { limit: fetch as u32, ..q.clone() });
            let got = r.results.len();
            {
                let _imp = Impersonation::start(pipe)?;
                r.results.retain(|h| {
                    let parent = crate::paths::parent(&h.path).unwrap_or_else(|| h.path.clone());
                    cache.can_list(&parent)
                });
            }
            let dropped = got - r.results.len();
            if r.results.len() >= want || got < fetch || fetch >= 8_000 {
                r.results.truncate(want);
                // `total` can't be filtered exactly without checking every
                // candidate; subtract what we saw being dropped.
                r.total = r.total.saturating_sub(dropped as u64);
                r.took_us = t0.elapsed().as_micros() as u64;
                r.source = "service".into();
                return Ok(r);
            }
            fetch *= 4;
        }
    }

    // ------------------------------------------------------------ client

    /// Overlapped pipe client with per-call timeouts.
    pub struct PipeClient {
        h: HANDLE,
        ev: HANDLE,
    }

    unsafe impl Send for PipeClient {}

    impl Drop for PipeClient {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.h);
                let _ = CloseHandle(self.ev);
            }
        }
    }

    impl PipeClient {
        pub fn connect(timeout: Duration) -> std::io::Result<PipeClient> {
            let name = wide(PIPE_NAME);
            let deadline = Instant::now() + timeout;
            loop {
                let r = unsafe {
                    CreateFileW(
                        PCWSTR(name.as_ptr()),
                        GENERIC_READ.0 | GENERIC_WRITE.0,
                        FILE_SHARE_MODE(0),
                        None,
                        OPEN_EXISTING,
                        FILE_FLAG_OVERLAPPED,
                        None,
                    )
                };
                match r {
                    Ok(h) => {
                        let ev = match unsafe { CreateEventW(None, true, false, PCWSTR::null()) } {
                            Ok(ev) => ev,
                            Err(e) => {
                                unsafe {
                                    let _ = CloseHandle(h);
                                }
                                return Err(std::io::Error::other(e.to_string()));
                            }
                        };
                        return Ok(PipeClient { h, ev });
                    }
                    Err(e) if e.code() == ERROR_PIPE_BUSY.to_hresult() => {
                        let left = deadline.saturating_duration_since(Instant::now());
                        if left.is_zero() {
                            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "pipe busy"));
                        }
                        unsafe {
                            let _ = WaitNamedPipeW(PCWSTR(name.as_ptr()), left.as_millis().max(1) as u32);
                        }
                    }
                    Err(e) => return Err(std::io::Error::from_raw_os_error((e.code().0 & 0xFFFF) as i32)),
                }
            }
        }

        /// Connects only when the pipe is served by the FerretIndexer service itself (the
        /// process the service manager started). While the service isn't running, any
        /// program could create a pipe with this name; Ferret must never talk to that one.
        pub fn connect_to_service(timeout: Duration) -> std::io::Result<PipeClient> {
            let c = PipeClient::connect(timeout)?;
            if c.served_by_service() {
                Ok(c)
            } else {
                Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "the search pipe isn't served by the FerretIndexer service"))
            }
        }

        fn served_by_service(&self) -> bool {
            use windows::Win32::System::Pipes::GetNamedPipeServerProcessId;
            use windows_service::service::ServiceAccess;
            use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
            let mut pid = 0u32;
            if unsafe { GetNamedPipeServerProcessId(self.h, &mut pid) }.is_err() || pid == 0 {
                return false;
            }
            let Ok(m) = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT) else { return false };
            let Ok(s) = m.open_service(crate::search::service::SERVICE_NAME, ServiceAccess::QUERY_STATUS) else { return false };
            s.query_status().ok().and_then(|st| st.process_id) == Some(pid)
        }

        /// One overlapped ReadFile (`rd`) or WriteFile (`wr`) with a deadline.
        fn io(&self, rd: Option<&mut [u8]>, wr: Option<&[u8]>, deadline: Instant) -> std::io::Result<usize> {
            let mut ov = OVERLAPPED { hEvent: self.ev, ..Default::default() };
            let mut n = 0u32;
            let r = unsafe {
                match (rd, wr) {
                    (Some(b), _) => ReadFile(self.h, Some(b), Some(&mut n), Some(&mut ov)),
                    (None, Some(b)) => WriteFile(self.h, Some(b), Some(&mut n), Some(&mut ov)),
                    (None, None) => return Ok(0),
                }
            };
            match r {
                Ok(()) => {}
                Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => {
                    let left = deadline.saturating_duration_since(Instant::now()).as_millis() as u32;
                    if unsafe { WaitForSingleObject(self.ev, left) } != WAIT_OBJECT_0 {
                        unsafe {
                            let _ = CancelIoEx(self.h, Some(&ov));
                            let _ = GetOverlappedResult(self.h, &ov, &mut n, true);
                        }
                        return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "indexer service timed out"));
                    }
                }
                Err(e) => return Err(std::io::Error::other(e.to_string())),
            }
            unsafe { GetOverlappedResult(self.h, &ov, &mut n, false) }.map_err(|e| std::io::Error::other(e.to_string()))?;
            Ok(n as usize)
        }

        fn write_all(&self, mut b: &[u8], deadline: Instant) -> std::io::Result<()> {
            while !b.is_empty() {
                let n = self.io(None, Some(b), deadline)?;
                if n == 0 {
                    return Err(std::io::ErrorKind::WriteZero.into());
                }
                b = &b[n..];
            }
            Ok(())
        }

        fn read_exact(&self, buf: &mut [u8], deadline: Instant) -> std::io::Result<()> {
            let mut off = 0;
            while off < buf.len() {
                let n = self.io(Some(&mut buf[off..]), None, deadline)?;
                if n == 0 {
                    return Err(std::io::ErrorKind::UnexpectedEof.into());
                }
                off += n;
            }
            Ok(())
        }

        pub fn request(&mut self, req: &Request, timeout: Duration) -> std::io::Result<Response> {
            let deadline = Instant::now() + timeout;
            self.write_all(&encode(req), deadline)?;
            let mut len = [0u8; 4];
            self.read_exact(&mut len, deadline)?;
            let n = u32::from_le_bytes(len) as usize;
            if n > MAX_MSG {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "message too large"));
            }
            let mut body = vec![0u8; n];
            self.read_exact(&mut body, deadline)?;
            serde_json::from_slice(&body).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_ipc_wire_format() {
        let req = Request::Query(SearchRequest { query: "node".into(), limit: 5, scope: None, frecency: None });
        let bytes = encode(&req);
        assert_eq!(u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize, bytes.len() - 4);
        let json = std::str::from_utf8(&bytes[4..]).unwrap();
        assert!(json.contains(r#""type":"query""#) && json.contains(r#""query":"node""#), "{json}");
        let back: Request = serde_json::from_slice(&read_frame(&mut bytes.as_slice()).unwrap()).unwrap();
        assert!(matches!(back, Request::Query(q) if q.limit == 5));
        let st: Request = serde_json::from_str(r#"{"type":"status"}"#).unwrap();
        assert!(matches!(st, Request::Status));
        // An update: a service from before it answers "bad request" (the app then reinstalls it).
        let up = serde_json::to_string(&Request::Update { exe: r"C:\Ferret\ferret-indexer.exe".into() }).unwrap();
        assert_eq!(up, r#"{"type":"update","exe":"C:\\Ferret\\ferret-indexer.exe"}"#);
        assert_eq!(serde_json::to_string(&Response::Updating).unwrap(), r#"{"type":"updating"}"#);
        // Sizes: one answer for each folder asked, none where the service has nothing to say.
        let ask = serde_json::to_string(&Request::Sizes { paths: vec![r"C:\a".into()] }).unwrap();
        assert_eq!(ask, r#"{"type":"sizes","paths":["C:\\a"]}"#);
        let answer = Response::Sizes { sizes: vec![Some(FolderTotal { bytes: 5, files: 1, partial: true }), None] };
        let back: Response = serde_json::from_str(&serde_json::to_string(&answer).unwrap()).unwrap();
        assert!(matches!(back, Response::Sizes { sizes } if sizes[0] == Some(FolderTotal { bytes: 5, files: 1, partial: true }) && sizes[1].is_none()));
        let resp = Response::Error { message: "x".into() };
        let s = serde_json::to_string(&resp).unwrap();
        assert_eq!(s, r#"{"type":"error","message":"x"}"#);
    }

    /// End-to-end over a real pipe (no service needed): server + client in-process.
    #[cfg(windows)]
    #[test]
    fn search_ipc_pipe_roundtrip() {
        use crate::search::engine::{Engine, Volume};
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;
        use std::time::Duration;
        // Skip if a real service already owns the pipe.
        if PipeClient::connect(Duration::from_millis(10)).is_ok() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("ferret-ipc-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join(r"sub\pipe-test-file.txt"), "x").unwrap();
        let root = dir.to_string_lossy().to_string();
        let mut ix = crate::search::index::Index::new(&root, false);
        let sub = ix.insert(0, "sub", crate::search::index::flags::DIR, 0, 0);
        ix.insert(sub, "pipe-test-file.txt", 0, 0, 0);
        // An entry under a folder that doesn't exist: the access check drops it.
        let ghost = ix.insert(0, "ghost-dir", crate::search::index::flags::DIR, 0, 0);
        ix.insert(ghost, "pipe-test-ghost.txt", 0, 0, 0);
        let eng = Engine::new();
        eng.add_volume(Volume::new(ix, true));
        let stop = Arc::new(AtomicBool::new(false));
        let errors = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let (e2, s2) = (eng.clone(), stop.clone());
        let server = std::thread::spawn(move || serve(e2, s2, errors, Arc::new(|_| {})));
        let mut client = None;
        for _ in 0..100 {
            if let Ok(c) = PipeClient::connect(Duration::from_millis(50)) {
                client = Some(c);
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut c = client.expect("connect");
        let r = c.request(&Request::Ping, Duration::from_secs(2)).unwrap();
        assert!(matches!(r, Response::Pong));
        let q = SearchRequest { query: "pipe-test".into(), limit: 10, ..Default::default() };
        match c.request(&Request::Query(q), Duration::from_secs(2)).unwrap() {
            Response::Results(r) => {
                assert_eq!(r.source, "service");
                let names: Vec<_> = r.results.iter().map(|h| h.name.as_str()).collect();
                assert_eq!(names, vec!["pipe-test-file.txt"]);
            }
            other => panic!("{other:?}"),
        }
        match c.request(&Request::Status, Duration::from_secs(2)).unwrap() {
            Response::Status(s) => assert_eq!(s.volumes.len(), 1),
            other => panic!("{other:?}"),
        }
        drop(c);
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        wake_server();
        server.join().unwrap().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }
}
