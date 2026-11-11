//! `ferret-indexer`: the FerretIndexer Windows service (LocalSystem) that runs
//! the MFT provider for every fixed NTFS volume and answers queries on
//! `\\.\pipe\ferret-index`, plus its CLI.
//!
//! ```text
//! ferret-indexer --install      register (auto-start) + start the service   [admin]
//! ferret-indexer --uninstall    stop + remove the service                   [admin]
//! ferret-indexer --console      run the indexer in the foreground           [admin]
//! ferret-indexer --query TEXT   query the running service, print results + timing
//! ferret-indexer --status       service + index status
//! ferret-indexer --probe        what MFT/USN access works for this user
//! ferret-indexer --sizes DIR... folder sizes from the service (as this user), next to a walk
//! ferret-indexer --sizes-bench  times and checks folder sizes from the file table      [admin]
//! ```

pub const SERVICE_NAME: &str = "FerretIndexer";
pub const DISPLAY_NAME: &str = "Ferret Indexer";

/// When the service ends without being stopped (a crash: "terminated unexpectedly", event 7034),
/// Windows starts it again after this many seconds: the first time, the second time, and every
/// time after that. Without them a crashed service stays stopped (users may not start it) and
/// Ferret falls back to walking the disk. Set at install and by the service at every start.
pub const RESTART_AFTER_SECS: [u64; 3] = [5, 30, 120];
/// After a day without a crash, the next one counts as the first again.
pub const RESTART_RESET_SECS: u64 = 24 * 60 * 60;

const HELP: &str = "\
ferret-indexer - whole-disk file index service for Ferret

USAGE:
  ferret-indexer --install       Register the FerretIndexer service (auto-start, LocalSystem) and start it [admin]
  ferret-indexer --uninstall     Stop and remove the service [admin]
  ferret-indexer --console       Run the MFT indexer + pipe server in the foreground (debugging) [admin]
  ferret-indexer --query <text>  Send a query to the running service and print results + timing
  ferret-indexer --status        Show service and index status
  ferret-indexer --probe         Report which MFT/USN operations work for the current user
  ferret-indexer --sizes <dir>.. Folder sizes from the service (as this user), next to a walk of them
  ferret-indexer --sizes-diff <dir>  Every folder inside, service against walk: where they differ
  ferret-indexer --sizes-bench OUT [--cold] [DIR | each:DIR | change:DIR | diff:DIR | names:FILE]...  Times and checks folder sizes from the file table [admin]
  ferret-indexer --help          This help

Pipe: \\\\.\\pipe\\ferret-index    State: %PROGRAMDATA%\\Ferret
";

/// Installed / running state of the service (works without admin).
#[cfg(windows)]
pub fn service_state() -> (bool, bool) {
    use windows_service::service::{ServiceAccess, ServiceState};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    let Ok(m) = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT) else { return (false, false) };
    let Ok(s) = m.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS) else { return (false, false) };
    let running = s.query_status().map(|st| st.current_state == ServiceState::Running).unwrap_or(false);
    (true, running)
}

#[cfg(not(windows))]
pub fn service_state() -> (bool, bool) {
    (false, false)
}

#[cfg(not(windows))]
pub fn main_entry() -> i32 {
    eprintln!("ferret-indexer only runs on Windows");
    1
}

#[cfg(windows)]
pub use win::{ensure_restart_on_failure, ensure_users_may_start, main_entry, restart_on_failure, start_if_stopped, update_from};

/// What `start_if_stopped` found or did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Started {
    NotInstalled,
    Running,
    Started,
    /// Windows said no (an older service that only administrators may start), and why.
    Refused(String),
}

#[cfg(not(windows))]
pub fn start_if_stopped() -> Started {
    Started::NotInstalled
}

#[cfg(windows)]
mod win {
    use super::*;
    use crate::search::engine::{Engine, SearchRequest};
    use crate::search::ipc::{self, PipeClient, Request, Response};
    use crate::search::mft::{self, MftError};
    use parking_lot::Mutex;
    use std::ffi::OsString;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use windows_service::service::{
        Service, ServiceAccess, ServiceAction, ServiceActionType, ServiceControl, ServiceControlAccept, ServiceErrorControl,
        ServiceExitCode, ServiceFailureActions, ServiceFailureResetPeriod, ServiceInfo, ServiceStartType, ServiceState,
        ServiceStatus, ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    use windows_service::{define_windows_service, service_dispatcher};

    type Log = Arc<dyn Fn(String) + Send + Sync>;

    pub fn main_entry() -> i32 {
        let args: Vec<String> = std::env::args().skip(1).collect();
        match args.first().map(|s| s.as_str()) {
            Some("--install") => install(),
            Some("--finish-update") => finish_update(),
            Some("--uninstall") => uninstall(),
            Some("--console") => console(),
            Some("--service") => run_service_dispatcher(),
            Some("--query") => query_cli(&args[1..].join(" ")),
            Some("--status") => status_cli(),
            Some("--probe") => probe_cli(),
            Some("--sizes-bench") => sizes_bench(&args[1..]),
            Some("--sizes") => sizes_cli(&args[1..]),
            Some("--sizes-diff") => sizes_diff_cli(args.get(1).map_or("C:\\", |s| s.as_str())),
            None | Some("--help") | Some("-h") | Some("/?") => {
                print!("{HELP}");
                0
            }
            Some(other) => {
                eprintln!("unknown argument: {other}\n");
                eprint!("{HELP}");
                2
            }
        }
    }

    /// `%PROGRAMDATA%\Ferret` — snapshots + log; SYSTEM/Administrators only,
    /// since the snapshots contain every file name on the machine.
    pub fn state_dir() -> PathBuf {
        let base = std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
        base.join("Ferret")
    }

    fn wide_path(dir: &Path) -> Vec<u16> {
        dir.as_os_str().to_string_lossy().encode_utf16().chain(Some(0)).collect()
    }

    /// Is `dir` a real folder owned by SYSTEM or Administrators? A folder
    /// pre-created by a regular user (ProgramData lets users create folders)
    /// or a junction must never hold the service binary or the snapshots.
    fn trusted_dir(dir: &Path) -> Result<bool, String> {
        use std::os::windows::fs::MetadataExt;
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{LocalFree, ERROR_SUCCESS, HLOCAL};
        use windows::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
        use windows::Win32::Security::{
            IsWellKnownSid, WinBuiltinAdministratorsSid, WinLocalSystemSid, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
            PSID,
        };
        let m = std::fs::symlink_metadata(dir).map_err(|e| e.to_string())?;
        if !m.is_dir() || m.file_attributes() & 0x400 != 0 {
            return Ok(false);
        }
        let w = wide_path(dir);
        let mut owner = PSID::default();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        let r = unsafe {
            GetNamedSecurityInfoW(PCWSTR(w.as_ptr()), SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION, Some(&mut owner), None, None, None, &mut sd)
        };
        if r != ERROR_SUCCESS {
            return Err(format!("reading owner of {}: error {}", dir.display(), r.0));
        }
        let ok = unsafe { IsWellKnownSid(owner, WinBuiltinAdministratorsSid).as_bool() || IsWellKnownSid(owner, WinLocalSystemSid).as_bool() };
        unsafe {
            let _ = LocalFree(Some(HLOCAL(sd.0)));
        }
        Ok(ok)
    }

    /// Creates `dir` (SYSTEM + Administrators only). Must run elevated: an
    /// existing folder that isn't admin-owned is moved aside first.
    fn secure_dir(dir: &Path) -> Result<(), String> {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::ERROR_SUCCESS;
        use windows::Win32::Security::Authorization::{SetNamedSecurityInfoW, SE_FILE_OBJECT};
        use windows::Win32::Security::{
            GetSecurityDescriptorDacl, ACL, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
        };
        if std::fs::symlink_metadata(dir).is_ok() && !trusted_dir(dir)? {
            let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let aside = dir.with_file_name(format!("Ferret.untrusted-{stamp}"));
            std::fs::rename(dir, &aside)
                .map_err(|e| format!("{} is not owned by SYSTEM/Administrators and could not be moved aside: {e}", dir.display()))?;
        }
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        let sd = ipc::Sddl::new("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)").map_err(|e| e.to_string())?;
        let mut present = windows::core::BOOL(0);
        let mut defaulted = windows::core::BOOL(0);
        let mut dacl: *mut ACL = std::ptr::null_mut();
        unsafe { GetSecurityDescriptorDacl(sd.ptr(), &mut present, &mut dacl, &mut defaulted) }.map_err(|e| e.to_string())?;
        let w = wide_path(dir);
        let r = unsafe {
            SetNamedSecurityInfoW(
                PCWSTR(w.as_ptr()),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(dacl),
                None,
            )
        };
        if r != ERROR_SUCCESS {
            return Err(format!("securing {}: error {}", dir.display(), r.0));
        }
        // Re-check: someone could have raced us between move-aside and create.
        if !trusted_dir(dir)? {
            return Err(format!("{} is not owned by SYSTEM/Administrators", dir.display()));
        }
        Ok(())
    }

    fn file_logger(dir: &Path) -> Log {
        let path = dir.join("indexer.log");
        if std::fs::metadata(&path).map(|m| m.len() > (2 << 20)).unwrap_or(false) {
            let _ = std::fs::rename(&path, dir.join("indexer.old.log"));
        }
        let file = Mutex::new(std::fs::OpenOptions::new().create(true).append(true).open(&path).ok());
        Arc::new(move |msg: String| {
            if let Some(f) = file.lock().as_mut() {
                let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
                let _ = writeln!(f, "{ts} {msg}");
            }
        })
    }

    /// Runs MFT indexing for every fixed NTFS volume + the pipe server until
    /// `stop`. Fails fast (before serving) when the volumes can't be opened.
    pub fn run_indexer(stop: Arc<AtomicBool>, log: Log) -> Result<(), String> {
        let roots = mft::ntfs_fixed_volumes();
        if roots.is_empty() {
            return Err("no fixed NTFS volumes found".into());
        }
        // Not elevated? Say so clearly (and touch nothing on disk) instead of
        // serving an empty index.
        let first = roots[0].chars().next().unwrap_or('C');
        if let Err(e) = mft::open_volume(first) {
            return Err(e.to_string());
        }
        // Snapshots list every file name: refuse an untrusted state folder.
        let dir = state_dir();
        secure_dir(&dir)?;
        log(format!("indexing {}", roots.join(", ")));
        let engine = Engine::new();
        let errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut threads = Vec::new();
        for root in roots {
            let (eng, stop, log, errors, dir) = (engine.clone(), stop.clone(), log.clone(), errors.clone(), dir.clone());
            threads.push(std::thread::Builder::new().name(format!("mft-{root}")).spawn(move || {
                while !stop.load(Relaxed) {
                    let l = |m: String| log(m);
                    match mft::run_volume(&eng, &root, &dir, &stop, &l) {
                        Ok(()) => break,
                        Err(e @ MftError::AccessDenied(_)) | Err(e @ MftError::NotNtfs(_)) => {
                            log(format!("{root}: {e}"));
                            errors.lock().push(format!("{root}: {e}"));
                            break;
                        }
                        Err(e) => {
                            log(format!("{root}: {e}; retrying in 30s"));
                            errors.lock().push(format!("{root}: {e}"));
                            for _ in 0..300 {
                                if stop.load(Relaxed) {
                                    break;
                                }
                                std::thread::sleep(Duration::from_millis(100));
                            }
                        }
                    }
                }
            }));
        }
        let server = {
            let (eng, stop, errors, log) = (engine.clone(), stop.clone(), errors.clone(), log.clone());
            std::thread::Builder::new().name("ferret-pipe".into()).spawn(move || ipc::serve(eng, stop, errors, log))
        }
        .map_err(|e| e.to_string())?;
        while !stop.load(Relaxed) {
            if server.is_finished() {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        stop.store(true, Relaxed);
        ipc::wake_server();
        let served = server.join().map_err(|_| "pipe server panicked".to_string())?;
        for t in threads.into_iter().flatten() {
            let _ = t.join();
        }
        served.map_err(|e| e.to_string())
    }

    // ------------------------------------------------------------ service

    define_windows_service!(ffi_service_main, service_main);

    fn run_service_dispatcher() -> i32 {
        match service_dispatcher::start(SERVICE_NAME, ffi_service_main) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("not started by the service control manager ({e}); use --console to run interactively");
                1
            }
        }
    }

    fn service_main(_args: Vec<OsString>) {
        let dir = state_dir();
        let log: Log = match secure_dir(&dir) {
            Ok(()) => file_logger(&dir),
            Err(_) => Arc::new(|_| {}),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let handler = move |c| match c {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                stop2.store(true, Relaxed);
                ipc::wake_server();
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        };
        let Ok(status) = service_control_handler::register(SERVICE_NAME, handler) else { return };
        let set = |state: ServiceState, code: u32| {
            let _ = status.set_service_status(ServiceStatus {
                service_type: ServiceType::OWN_PROCESS,
                current_state: state,
                controls_accepted: if state == ServiceState::Running {
                    ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
                } else {
                    ServiceControlAccept::empty()
                },
                exit_code: ServiceExitCode::Win32(code),
                checkpoint: 0,
                wait_hint: Duration::from_secs(10),
                process_id: None,
            });
        };
        set(ServiceState::Running, 0);
        log("service started".into());
        // Restarted by Windows after a crash: set at install too, and here so that a service
        // installed before that gets it now.
        match ensure_restart_on_failure(SERVICE_NAME) {
            Ok(true) => log(format!("after a crash Windows restarts the service now ({})", restart_summary())),
            Ok(false) => {}
            Err(e) => log(format!("the service won't be restarted after a crash: {e}")),
        }
        // Ferret (the signed-in user) may start it again when it isn't running.
        match ensure_users_may_start(SERVICE_NAME) {
            Ok(true) => log("signed-in users may start the service now (Ferret starts it when it's stopped)".into()),
            Ok(false) => {}
            Err(e) => log(format!("users can't start the service: {e}")),
        }
        let r = run_indexer(stop, log.clone());
        if let Err(e) = &r {
            log(format!("indexer stopped: {e}"));
        }
        log("service stopped".into());
        set(ServiceState::Stopped, if r.is_ok() { 0 } else { 1 });
    }

    fn console() -> i32 {
        println!("ferret-indexer console - indexing fixed NTFS volumes (Ctrl+C to quit)");
        let log: Log = Arc::new(|m: String| println!("[{}] {m}", chrono::Local::now().format("%H:%M:%S")));
        let stop = Arc::new(AtomicBool::new(false));
        // Progress ticker.
        let (s2, l2) = (stop.clone(), log.clone());
        std::thread::spawn(move || {
            let mut last = String::new();
            while !s2.load(Relaxed) {
                std::thread::sleep(Duration::from_secs(2));
                if let Ok(mut c) = PipeClient::connect(Duration::from_millis(100)) {
                    if let Ok(Response::Status(st)) = c.request(&Request::Status, Duration::from_secs(1)) {
                        let line = st
                            .volumes
                            .iter()
                            .map(|v| format!("{} {} entries{}", v.root, v.entries, if v.ready { "" } else { " (building)" }))
                            .collect::<Vec<_>>()
                            .join(", ");
                        if line != last {
                            l2(line.clone());
                            last = line;
                        }
                    }
                }
            }
        });
        match run_indexer(stop, log) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("error: {e}");
                5
            }
        }
    }

    // ------------------------------------------------------------ install

    fn is_access_denied(e: &windows_service::Error) -> bool {
        matches!(e, windows_service::Error::Winapi(io) if io.raw_os_error() == Some(5))
    }

    /// The copy the service runs, in a folder only SYSTEM and administrators can change.
    fn installed_exe() -> PathBuf {
        state_dir().join("bin").join("ferret-indexer.exe")
    }

    /// A copy waiting to replace the installed one (`update_from`), in the same folder.
    fn next_exe() -> PathBuf {
        state_dir().join("bin").join("ferret-indexer.next.exe")
    }

    /// The release signature that comes with an exe (`ferret-indexer.exe.sig`; see `signed`).
    fn signature_of(exe: &Path) -> PathBuf {
        let mut name = exe.file_name().unwrap_or_default().to_os_string();
        name.push(".sig");
        exe.with_file_name(name)
    }

    fn same_file(a: &Path, b: &Path) -> bool {
        matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
    }

    /// Reads at most `most` bytes of a file (what a client names is read by SYSTEM).
    fn read_at_most(path: &Path, most: u64) -> Result<Vec<u8>, String> {
        use std::io::Read;
        let mut bytes = Vec::new();
        std::fs::File::open(path).and_then(|f| f.take(most + 1).read_to_end(&mut bytes)).map_err(|e| format!("{} can't be read ({e})", path.display()))?;
        if bytes.len() as u64 > most {
            return Err(format!("{} is too big", path.display()));
        }
        Ok(bytes)
    }

    /// `Request::Update`, in the service: installs a newer release of the indexer in place of
    /// this one. `exe` is Ferret's copy, in Ferret's own folder, which the user's account can
    /// change: it's installed only if it's signed with Ferret's release key and no older than
    /// this one (`signed::may_replace`). The check is made here, by the installed copy, and the
    /// bytes checked are the ones installed: they go to `next_exe`, which finishes the update
    /// (`--finish-update`). Anyone may ask: all they can get is a newer release.
    pub fn update_from(exe: &str) -> Result<(), String> {
        static BUSY: AtomicBool = AtomicBool::new(false);
        let installed = installed_exe();
        if !std::env::current_exe().is_ok_and(|me| same_file(&me, &installed)) {
            return Err("only the installed service updates itself".into());
        }
        let new = Path::new(exe);
        let bytes = read_at_most(new, 64 << 20)?;
        let sig = read_at_most(&signature_of(new), 4096).ok().and_then(|s| String::from_utf8(s).ok()).unwrap_or_default();
        let old = (std::fs::read(&installed).ok(), std::fs::read_to_string(signature_of(&installed)).ok());
        let old = old.0.as_deref().zip(old.1.as_deref());
        crate::search::signed::may_replace((&bytes, &sig), old, crate::search::signed::RELEASE_KEY)
            .map_err(|why| format!("{} wasn't installed: {why}", new.display()))?;
        if BUSY.swap(true, Relaxed) {
            return Err("an update is under way already".into());
        }
        let next = next_exe();
        let written = std::fs::write(&next, &bytes).and_then(|_| std::fs::write(signature_of(&next), &sig));
        let started = written.and_then(|_| {
            use std::os::windows::process::CommandExt;
            const DETACHED_PROCESS: u32 = 0x8;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            std::process::Command::new(&next).arg("--finish-update").creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW).spawn()
        });
        if let Err(e) = started {
            BUSY.store(false, Relaxed);
            return Err(format!("the update couldn't start ({e})"));
        }
        Ok(())
    }

    /// `--finish-update`: run from `next_exe` (see `update_from`), stops the service and puts this
    /// copy in its place.
    fn finish_update() -> i32 {
        let next = next_exe();
        if !std::env::current_exe().is_ok_and(|me| same_file(&me, &next)) {
            eprintln!("error: --finish-update is for the service's own update ({})", next.display());
            return 2;
        }
        let (Ok(bytes), Ok(sig)) = (std::fs::read(&next), std::fs::read_to_string(signature_of(&next))) else {
            eprintln!("error: {} can't be read", next.display());
            return 1;
        };
        put_in_place(&bytes, Some(&sig))
    }

    /// `--install`: this copy, as it is (a first install has nothing to check it against).
    fn install() -> i32 {
        let src = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        };
        let bytes = match std::fs::read(&src) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("error: reading {}: {e}", src.display());
                return 1;
            }
        };
        let sig = std::fs::read_to_string(signature_of(&src)).ok().filter(|s| !s.trim().is_empty());
        put_in_place(&bytes, sig.as_deref())
    }

    /// Registers (again) and starts the service, running these bytes from `installed_exe`.
    fn put_in_place(bytes: &[u8], sig: Option<&str>) -> i32 {
        let manager = match ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE) {
            Ok(m) => m,
            Err(e) if is_access_denied(&e) => {
                eprintln!("error: installing the service requires administrator rights (run from an elevated prompt)");
                return 5;
            }
            Err(e) => {
                eprintln!("error: cannot open the service manager: {e}");
                return 1;
            }
        };
        // Replace an existing registration.
        if let Ok(old) = manager.open_service(SERVICE_NAME, ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS) {
            let _ = old.stop();
            wait_state(&old, ServiceState::Stopped, Duration::from_secs(15));
            let _ = old.delete();
            drop(old);
            std::thread::sleep(Duration::from_millis(500));
        }
        // Run from a protected copy so the service binary can't be swapped by
        // non-admins and rebuilding/updating the app doesn't hit a locked exe.
        let dir = state_dir();
        if let Err(e) = secure_dir(&dir) {
            eprintln!("error: {e}");
            return 1;
        }
        let exe = installed_exe();
        // With its signature, for the next update to compare with (none: a development build).
        let sig_file = signature_of(&exe);
        // The old service's process may hold its exe for a moment after it stopped.
        let write_exe = || {
            let mut tries = 0;
            loop {
                match std::fs::write(&exe, bytes) {
                    Err(e) if tries < 20 && matches!(e.raw_os_error(), Some(5 | 32)) => {
                        tries += 1;
                        std::thread::sleep(Duration::from_millis(250));
                    }
                    r => return r,
                }
            }
        };
        let written = std::fs::create_dir_all(exe.parent().unwrap_or(&dir))
            .and_then(|_| write_exe())
            .and_then(|_| match sig {
                Some(s) => std::fs::write(&sig_file, s),
                None => std::fs::remove_file(&sig_file).or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) }),
            });
        if let Err(e) = written {
            eprintln!("error: writing {}: {e}", exe.display());
            return 1;
        }
        let info = ServiceInfo {
            name: SERVICE_NAME.into(),
            display_name: DISPLAY_NAME.into(),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: exe.clone(),
            launch_arguments: vec!["--service".into()],
            dependencies: vec![],
            account_name: None, // LocalSystem
            account_password: None,
        };
        let access = ServiceAccess::CHANGE_CONFIG | ServiceAccess::START | ServiceAccess::QUERY_STATUS | ServiceAccess::QUERY_CONFIG;
        let svc = match manager.create_service(&info, access) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: creating the service failed: {e}");
                return 1;
            }
        };
        let _ = svc.set_description(
            "Indexes file names on NTFS volumes (MFT + USN journal) for instant search in Ferret. \
             Results are filtered to folders the requesting user can list.",
        );
        if let Err(e) = restart_on_failure(&svc) {
            eprintln!("warning: the service won't be restarted after a crash ({})", winapi_error(&e));
        }
        if let Err(e) = ensure_users_may_start(SERVICE_NAME) {
            eprintln!("warning: Ferret won't be able to start the service when it's stopped ({e})");
        }
        if let Err(e) = svc.start(&[] as &[&std::ffi::OsStr]) {
            eprintln!("error: service installed but failed to start: {e}");
            return 1;
        }
        println!("{SERVICE_NAME} installed ({}) and started.", exe.display());
        0
    }

    // ------------------------------------------------------------ crash recovery

    /// `RESTART_AFTER_SECS` and `RESTART_RESET_SECS`, as the service control manager takes them.
    pub(super) fn restart_actions() -> ServiceFailureActions {
        ServiceFailureActions {
            reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(RESTART_RESET_SECS)),
            // Left as they are (no reboot message or command: neither action is used).
            reboot_msg: None,
            command: None,
            actions: Some(
                RESTART_AFTER_SECS
                    .iter()
                    .map(|&s| ServiceAction { action_type: ServiceActionType::Restart, delay: Duration::from_secs(s) })
                    .collect(),
            ),
        }
    }

    /// Does a service's recovery already restart it as `restart_actions` does?
    pub(super) fn restarts_already(current: &ServiceFailureActions) -> bool {
        let want = restart_actions();
        current.reset_period == want.reset_period && current.actions == want.actions
    }

    /// "5 s, 30 s, then every 2 min; counted afresh after a day"
    pub(super) fn restart_summary() -> String {
        let say = |s: u64| if s.is_multiple_of(60) { format!("{} min", s / 60) } else { format!("{s} s") };
        let (last, first) = RESTART_AFTER_SECS.split_last().expect("restart delays");
        let first: Vec<String> = first.iter().map(|&s| say(s)).collect();
        format!("after {}, then every {}; counted afresh after a day", first.join(", "), say(*last))
    }

    /// The Windows error behind a windows-service error (its own message leaves the code out).
    fn winapi_error(e: &windows_service::Error) -> String {
        match e {
            windows_service::Error::Winapi(io) => io.to_string(),
            other => other.to_string(),
        }
    }

    /// Makes Windows restart this service when it ends without being stopped (`RESTART_AFTER_SECS`),
    /// unless it does already; true when it was set. The handle needs QUERY_CONFIG, CHANGE_CONFIG
    /// and START (Windows asks for START to accept a restart action).
    pub fn restart_on_failure(svc: &Service) -> windows_service::Result<bool> {
        if svc.get_failure_actions().is_ok_and(|a| restarts_already(&a)) {
            return Ok(false);
        }
        svc.update_failure_actions(restart_actions())?;
        Ok(true)
    }

    /// `restart_on_failure` for the service called `name`. The search service calls it at every
    /// start, for itself: one installed before it was set at install gets it then. It runs as
    /// SYSTEM, which may change a service's configuration (as a member of Administrators).
    pub fn ensure_restart_on_failure(name: &str) -> Result<bool, String> {
        let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
            .map_err(|e| format!("the service manager: {}", winapi_error(&e)))?;
        let svc = manager
            .open_service(name, ServiceAccess::QUERY_CONFIG | ServiceAccess::CHANGE_CONFIG | ServiceAccess::START)
            .map_err(|e| format!("opening {name}: {}", winapi_error(&e)))?;
        restart_on_failure(&svc).map_err(|e| format!("{name}'s recovery: {}", winapi_error(&e)))
    }

    /// Starts the service if it's installed and not running, as the signed-in user (no elevation:
    /// a service from this release on lets users start it, `ensure_users_may_start`).
    pub fn start_if_stopped() -> super::Started {
        use windows_service::service::ServiceState;
        let Ok(manager) = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT) else {
            return super::Started::Refused("the service manager can't be reached".into());
        };
        let Ok(svc) = manager.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS) else { return super::Started::NotInstalled };
        if svc.query_status().is_ok_and(|s| matches!(s.current_state, ServiceState::Running | ServiceState::StartPending)) {
            return super::Started::Running;
        }
        match manager.open_service(SERVICE_NAME, ServiceAccess::START).and_then(|s| s.start(&[] as &[&std::ffi::OsStr])) {
            Ok(()) => super::Started::Started,
            Err(e) => super::Started::Refused(winapi_error(&e)),
        }
    }

    /// Lets signed-in users start the service, and nothing more (not stop it, change it or
    /// remove it), so Ferret can start it again when it isn't running: stopped in Services, or
    /// out of restarts after crashes. Windows' default lets only administrators start a service.
    /// Adds `(A;;RP;;;IU)` (RP: start) to its DACL unless interactive users may start it already;
    /// true when it was added. Run as SYSTEM (at every start) and by the elevated installer.
    pub fn ensure_users_may_start(name: &str) -> Result<bool, String> {
        use windows::Win32::Foundation::{HLOCAL, LocalFree};
        use windows::Win32::Security::Authorization::{
            ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
        };
        use windows::Win32::Security::{DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR};
        use windows::Win32::System::Services::{
            CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceObjectSecurity, SC_MANAGER_CONNECT, SetServiceObjectSecurity,
        };
        use windows::core::{HSTRING, PWSTR};
        const READ_CONTROL: u32 = 0x0002_0000;
        const WRITE_DAC: u32 = 0x0004_0000;
        let err = |what: &str, e: windows::core::Error| format!("{what}: {}", e.message());
        unsafe {
            let scm = OpenSCManagerW(None, None, SC_MANAGER_CONNECT).map_err(|e| err("the service manager", e))?;
            let svc = match OpenServiceW(scm, &HSTRING::from(name), READ_CONTROL | WRITE_DAC) {
                Ok(s) => s,
                Err(e) => {
                    let _ = CloseServiceHandle(scm);
                    return Err(err(&format!("opening {name}"), e));
                }
            };
            let result = (|| -> Result<bool, String> {
                // Its DACL as text.
                let mut needed = 0u32;
                let _ = QueryServiceObjectSecurity(svc, DACL_SECURITY_INFORMATION.0, None, 0, &mut needed);
                let mut buf = vec![0u8; needed.max(64) as usize];
                QueryServiceObjectSecurity(svc, DACL_SECURITY_INFORMATION.0, Some(PSECURITY_DESCRIPTOR(buf.as_mut_ptr() as _)), buf.len() as u32, &mut needed)
                    .map_err(|e| err("reading its permissions", e))?;
                let mut text = PWSTR::null();
                ConvertSecurityDescriptorToStringSecurityDescriptorW(PSECURITY_DESCRIPTOR(buf.as_mut_ptr() as _), SDDL_REVISION_1, DACL_SECURITY_INFORMATION, &mut text, None)
                    .map_err(|e| err("reading its permissions", e))?;
                let sddl = text.to_string().unwrap_or_default();
                let _ = LocalFree(Some(HLOCAL(text.0 as _)));
                let Some(with) = with_users_start(&sddl) else { return Ok(false) };
                let mut sd = PSECURITY_DESCRIPTOR::default();
                ConvertStringSecurityDescriptorToSecurityDescriptorW(&HSTRING::from(with.as_str()), SDDL_REVISION_1, &mut sd, None)
                    .map_err(|e| err("building its permissions", e))?;
                let set = SetServiceObjectSecurity(svc, DACL_SECURITY_INFORMATION, sd);
                let _ = LocalFree(Some(HLOCAL(sd.0 as _)));
                set.map_err(|e| err("setting its permissions", e))?;
                Ok(true)
            })();
            let _ = CloseServiceHandle(svc);
            let _ = CloseServiceHandle(scm);
            result
        }
    }

    /// The DACL (SDDL) with interactive users allowed to start the service, or None when they may
    /// already (an ACE for IU, or for everyone signed in, whose rights include RP).
    pub(super) fn with_users_start(sddl: &str) -> Option<String> {
        let dacl_at = sddl.find("D:")?;
        let aces = &sddl[dacl_at..];
        let allows_start = aces.split('(').skip(1).any(|ace| {
            let parts: Vec<&str> = ace.trim_end_matches(')').split(';').collect();
            parts.len() >= 6 && parts[0] == "A" && matches!(parts[5], "IU" | "AU" | "WD" | "BU") && parts[2].as_bytes().chunks(2).any(|r| r == b"RP")
        });
        if allows_start {
            return None;
        }
        // After "D:" and its flags (P, AI, AR…), before the first ACE.
        let first_ace = dacl_at + aces.find('(').unwrap_or(aces.len());
        Some(format!("{}(A;;RP;;;IU){}", &sddl[..first_ace], &sddl[first_ace..]))
    }

    /// What Windows does after the service crashes, for `--status` (any user may read it).
    fn recovery_status() -> String {
        let read = || -> windows_service::Result<ServiceFailureActions> {
            let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
            manager.open_service(SERVICE_NAME, ServiceAccess::QUERY_CONFIG)?.get_failure_actions()
        };
        match read() {
            Ok(a) if restarts_already(&a) => format!("restarted {}", restart_summary()),
            Ok(a) if a.actions.as_ref().is_some_and(|v| v.iter().any(|x| x.action_type == ServiceActionType::Restart)) => {
                format!("restarted, though not as Ferret sets it ({a:?})")
            }
            Ok(_) => "stays stopped (no recovery actions: a service from this release on sets them at install and start)".into(),
            Err(e) => format!("unknown ({})", winapi_error(&e)),
        }
    }

    fn wait_state(svc: &windows_service::service::Service, want: ServiceState, timeout: Duration) {
        let t = Instant::now();
        while t.elapsed() < timeout {
            match svc.query_status() {
                Ok(s) if s.current_state == want => return,
                Err(_) => return,
                _ => std::thread::sleep(Duration::from_millis(200)),
            }
        }
    }

    fn uninstall() -> i32 {
        let manager = match ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        };
        let svc = match manager.open_service(SERVICE_NAME, ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS) {
            Ok(s) => s,
            Err(e) if is_access_denied(&e) => {
                eprintln!("error: removing the service requires administrator rights");
                return 5;
            }
            Err(_) => {
                println!("{SERVICE_NAME} is not installed.");
                return 0;
            }
        };
        let _ = svc.stop();
        wait_state(&svc, ServiceState::Stopped, Duration::from_secs(15));
        if let Err(e) = svc.delete() {
            eprintln!("error: {e}");
            return 1;
        }
        drop(svc);
        // Remove snapshots (they list every file name); the running copy of
        // the binary may stay locked for a moment.
        let dir = state_dir();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "bin" || x == "tmp") {
                    let _ = std::fs::remove_file(p);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(500));
        let _ = std::fs::remove_dir_all(dir.join("bin"));
        println!("{SERVICE_NAME} removed.");
        0
    }

    // ------------------------------------------------------------ client CLI

    fn query_cli(text: &str) -> i32 {
        let t0 = Instant::now();
        let mut c = match PipeClient::connect(Duration::from_millis(500)) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("error: cannot connect to {} ({e}). Is the {SERVICE_NAME} service running?", ipc::PIPE_NAME);
                return 3;
            }
        };
        let connect_us = t0.elapsed().as_micros();
        let req = Request::Query(SearchRequest { query: text.to_string(), limit: 20, ..Default::default() });
        let t1 = Instant::now();
        match c.request(&req, Duration::from_secs(10)) {
            Ok(Response::Results(r)) => {
                let rt = t1.elapsed().as_micros();
                for h in &r.results {
                    println!("{:>8.0}  {}{}", h.score, h.path, if h.is_dir { "\\" } else { "" });
                }
                println!(
                    "\n{} shown / {} matches; service {:.2} ms, round trip {:.2} ms (connect {:.2} ms){}",
                    r.results.len(),
                    r.total,
                    r.took_us as f64 / 1e3,
                    rt as f64 / 1e3,
                    connect_us as f64 / 1e3,
                    if r.partial { " [index still building]" } else { "" }
                );
                0
            }
            Ok(Response::Error { message }) => {
                eprintln!("error: {message}");
                1
            }
            Ok(other) => {
                eprintln!("unexpected reply: {other:?}");
                1
            }
            Err(e) => {
                eprintln!("error: {e}");
                1
            }
        }
    }

    fn status_cli() -> i32 {
        let (installed, running) = service_state();
        println!("service {SERVICE_NAME}: {}", if !installed { "not installed" } else if running { "running" } else { "stopped" });
        if installed {
            println!("  after a crash: {}", recovery_status());
        }
        match PipeClient::connect(Duration::from_millis(300)) {
            Ok(mut c) => match c.request(&Request::Status, Duration::from_secs(3)) {
                Ok(Response::Status(s)) => {
                    println!("pipe {}: connected (service v{})", ipc::PIPE_NAME, s.version);
                    for v in s.volumes {
                        println!(
                            "  {}  {} entries  {}",
                            v.root,
                            v.entries,
                            if v.ready { "ready".to_string() } else { format!("building {:.0}%", v.progress * 100.0) }
                        );
                    }
                    for e in s.errors {
                        println!("  error: {e}");
                    }
                }
                other => println!("pipe: unexpected reply {other:?}"),
            },
            Err(e) => println!("pipe {}: not available ({e})", ipc::PIPE_NAME),
        }
        0
    }

    fn probe_cli() -> i32 {
        for root in crate::search::walk::fixed_drives() {
            match mft::volume_info(&root) {
                Some((serial, fs)) => println!("{root} {fs} serial {serial:08x}"),
                None => println!("{root} (no volume info)"),
            }
            if mft::volume_info(&root).is_some_and(|(_, fs)| fs.eq_ignore_ascii_case("NTFS")) {
                for line in mft::probe(&root) {
                    println!("  {line}");
                }
            }
        }
        0
    }

    /// `--sizes FOLDER...`: asks the running service for these folders' sizes, as Ferret does (as
    /// this user), and walks them too, to compare.
    fn sizes_cli(folders: &[String]) -> i32 {
        let mut c = match PipeClient::connect_to_service(Duration::from_secs(2)) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("error: cannot connect to {} ({e}). Is the {SERVICE_NAME} service running?", ipc::PIPE_NAME);
                return 3;
            }
        };
        let t = Instant::now();
        let sizes = match c.request(&Request::Sizes { paths: folders.to_vec() }, Duration::from_secs(30)) {
            Ok(Response::Sizes { sizes }) => sizes,
            Ok(other) => {
                eprintln!("error: {other:?}");
                return 1;
            }
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        };
        println!("{} folders in {:.2?}", folders.len(), t.elapsed());
        let gb = |b: u64| format!("{:.3} GB", b as f64 / 1e9);
        for (f, s) in folders.iter().zip(sizes) {
            let t = Instant::now();
            let w = crate::foldersize::count_folder(f);
            let took = t.elapsed();
            let service = s.map_or("—".to_string(), |s| format!("{} in {} files{}", gb(s.bytes), s.files, if s.partial { ", partial" } else { "" }));
            let walk = w.as_ref().map_or("—".to_string(), |w| format!("{} in {} files{}", gb(w.bytes), w.files.unwrap_or(0), if w.partial { ", partial" } else { "" }));
            let same = match (s, &w) {
                (Some(s), Some(w)) => s.bytes == w.bytes && Some(s.files) == w.files && s.partial == w.partial,
                (None, None) => true,
                _ => false,
            };
            println!("{f}\n  service: {service}\n  walk ({took:.2?}): {walk}  {}", if same { "same" } else { "DIFFERENT" });
        }
        0
    }

    /// `--sizes-diff FOLDER`: every folder in FOLDER walked once, and asked of the service all at
    /// once (as this user): where do they start to differ?
    fn sizes_diff_cli(folder: &str) -> i32 {
        let t = Instant::now();
        let (top, mut all) = crate::foldersize::count_folder_all(folder);
        all.extend(top);
        let walked = t.elapsed();
        let paths: Vec<String> = all.iter().map(|s| s.path.clone()).collect();
        let mut c = match PipeClient::connect_to_service(Duration::from_secs(2)) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("error: cannot connect ({e})");
                return 3;
            }
        };
        let t = Instant::now();
        let Ok(Response::Sizes { sizes }) = c.request(&Request::Sizes { paths: paths.clone() }, Duration::from_secs(120)) else {
            eprintln!("error: no sizes from the service");
            return 1;
        };
        let asked = t.elapsed();
        type Differ<'a> = (&'a str, (u64, u64, bool), Option<ipc::FolderTotal>);
        let differ: Vec<Differ> = all
            .iter()
            .zip(&sizes)
            .filter_map(|(w, s)| {
                let walk = (w.bytes, w.files.unwrap_or(0), w.partial);
                let same = s.is_some_and(|s| (s.bytes, s.files, s.partial) == walk);
                (!same).then_some((w.path.as_str(), walk, *s))
            })
            .collect();
        let lower: Vec<String> = differ.iter().map(|(p, _, _)| p.to_lowercase() + "\\").collect();
        let starts: Vec<_> = differ.iter().filter(|(p, _, _)| { let me = p.to_lowercase() + "\\"; !lower.iter().any(|o| o != &me && o.starts_with(&me)) }).collect();
        println!("{folder}: {} folders walked in {walked:.2?}, asked in {asked:.2?}; {} differ, {} where it starts", all.len(), differ.len(), starts.len());
        for (p, w, s) in starts.iter().take(25) {
            let s = s.map_or("none".to_string(), |s| format!("{} bytes, {} files{}", s.bytes, s.files, if s.partial { ", partial" } else { "" }));
            println!("  {p}\n    walk {} bytes, {} files{}; service {s}", w.0, w.1, if w.2 { ", partial" } else { "" });
        }
        0
    }

    // ------------------------------------------------------------ folder sizes benchmark

    /// `--sizes-bench OUT [--cold] [FOLDER | each:FOLDER | change:FOLDER]...` [admin]: times
    /// folder sizes from the file table (`sizes::scan`, and the raw table read alone, as WizTree
    /// reads it) and Ferret's own walk (`foldersize`), and compares their sizes:
    /// - each fixed NTFS drive: the table read and scanned;
    /// - FOLDER: walked, and its size from the table, side by side;
    /// - each:FOLDER: every folder inside it too, which must all agree;
    /// - change:FOLDER: files and folders changed in it, then followed through the change
    ///   journal (`sizes::look_up`, `Tree::update_all`), as the service follows them: the tree
    ///   must then be what a fresh scan gives, and what a walk gives.
    ///
    /// With --cold, Windows' file cache is emptied before each first timing. What it finds goes
    /// to OUT too.
    fn sizes_bench(args: &[String]) -> i32 {
        use crate::search::sizes::{self, Tree};
        let Some(out) = args.first() else {
            eprintln!("usage: ferret-indexer --sizes-bench OUT [--cold] [FOLDER | each:FOLDER | change:FOLDER]...");
            return 2;
        };
        let cold = args.iter().any(|a| a == "--cold");
        let mut lines: Vec<String> = Vec::new();
        let mut say = |s: String| {
            println!("{s}");
            lines.push(s);
        };
        let purge = |say: &mut dyn FnMut(String)| {
            if let Err(e) = if cold { purge_file_cache() } else { Ok(()) } {
                say(format!("  (the file cache wasn't emptied: {e})"));
            }
        };
        let gb = |b: u64| format!("{:.2} GB", b as f64 / 1e9);
        let never = AtomicBool::new(false);
        let mut drives: Vec<(String, mft::VolHandle, Tree)> = Vec::new();
        for root in mft::ntfs_fixed_volumes() {
            let letter = root.chars().next().unwrap_or('C');
            let vol = match mft::open_volume(letter) {
                Ok(v) => v,
                Err(e) => {
                    say(format!("{root} {e}"));
                    continue;
                }
            };
            purge(&mut say);
            match sizes::raw_read(&vol) {
                Ok((bytes, used, took)) => say(format!(
                    "{root} raw table read: {:.0} MB, {used} records in use, {took:.2?} ({:.0} MB/s)",
                    bytes as f64 / 1e6,
                    bytes as f64 / 1e6 / took.as_secs_f64()
                )),
                Err(e) => say(format!("{root} raw table read: {e}")),
            }
            purge(&mut say);
            match sizes::scan(&vol, &never) {
                Ok((tree, st)) => {
                    let all = tree.dir(sizes::ROOT).copied().unwrap_or_default();
                    say(format!(
                        "{root} table scan: {} records, {} folders, {} calls, {:.0} MB of answers, {:.2?}; the drive: {} in {} files; tree {:.0} MB",
                        st.records,
                        tree.dir_count(),
                        st.calls,
                        st.output_bytes as f64 / 1e6,
                        st.took,
                        gb(all.bytes),
                        all.files,
                        tree.heap_bytes() as f64 / 1e6
                    ));
                    if let Ok((_, again)) = sizes::scan(&vol, &never) {
                        say(format!("{root} table scan again (cached): {:.2?}", again.took));
                    }
                    drives.push((root.clone(), vol, tree));
                }
                Err(e) => say(format!("{root} table scan: {e}")),
            }
        }
        let recno = |p: &str| mft::root_frn(p).ok().map(|f| (f & 0x0000_FFFF_FFFF_FFFF) as u32);
        let walk = |p: &str| crate::foldersize::count_folder(p).map(|s| (s.bytes, s.files.unwrap_or(0), s.partial));
        let table = |tree: &Tree, p: &str| recno(p).and_then(|r| tree.dir(r)).map(|d| (d.bytes, d.files));
        // Every folder inside one (no symlinks or junctions, as the walk).
        fn folders_in(dir: &str, out: &mut Vec<String>) {
            for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                if e.file_type().is_ok_and(|t| t.is_dir()) {
                    let p = e.path().to_string_lossy().into_owned();
                    out.push(p.clone());
                    folders_in(&p, out);
                }
            }
        }
        for arg in args[1..].iter().filter(|a| *a != "--cold") {
            let (how, folder) = match arg.split_once(':').filter(|(h, _)| matches!(*h, "each" | "change" | "diff" | "names")) {
                Some((h, f)) => (h, f),
                None => ("", arg.as_str()),
            };
            let Some(d) = drives.iter().position(|(r, _, _)| folder.to_lowercase().starts_with(&r.to_lowercase())) else {
                say(format!("{folder}: not on a scanned drive"));
                continue;
            };
            if how == "change" {
                let (_, vol, tree) = &mut drives[d];
                say(change_check(vol, tree, folder));
                continue;
            }
            // File names only for my own test folders.
            let names_ok = |p: &str| p.to_lowercase().contains(r"\ferretqa\");
            if how == "names" {
                let (_, vol, _) = &drives[d];
                for l in recno(folder).map(|r| sizes::describe(vol, &[r], names_ok(folder)).unwrap_or_default()).unwrap_or_default() {
                    say(format!("{folder}: {l}"));
                }
                continue;
            }
            if how == "diff" {
                // Every folder walked once, against the table: where do they start to differ?
                let (_, vol, tree) = &drives[d];
                let (top, mut all) = crate::foldersize::count_folder_all(folder);
                all.extend(top);
                type Differ = (String, (u64, u64), Option<(u64, u64)>);
                let differ: Vec<Differ> = all
                    .iter()
                    .filter_map(|s| {
                        let w = (s.bytes, s.files.unwrap_or(0));
                        let t = table(tree, &s.path);
                        (Some(w) != t).then(|| (s.path.clone(), w, t))
                    })
                    .collect();
                let lower: Vec<String> = differ.iter().map(|(p, _, _)| p.to_lowercase() + "\\").collect();
                let mut starts: Vec<&Differ> =
                    differ.iter().filter(|(p, _, _)| { let me = p.to_lowercase() + "\\"; !lower.iter().any(|o| o != &me && o.starts_with(&me)) }).collect();
                let off = |(_, w, t): &&Differ| (w.1 as i64 - t.map_or(0, |t| t.1) as i64).abs();
                starts.sort_by_key(|e| std::cmp::Reverse(off(e)));
                // A program started from a packaged (Store) app, like this one from the Claude
                // app, sees that app's own copy of AppData merged in: the walk counts that too.
                let appdata = |p: &str| p.to_lowercase().contains(r"\appdata\");
                say(format!(
                    "{folder}: {} folders compared, {} differ, {} where it starts; outside AppData: {} differ, {} where it starts",
                    all.len(),
                    differ.len(),
                    starts.len(),
                    differ.iter().filter(|(p, _, _)| !appdata(p)).count(),
                    starts.iter().filter(|(p, _, _)| !appdata(p)).count()
                ));
                starts.sort_by_key(|(p, _, _)| appdata(p));
                for (p, w, t) in starts.iter().take(15) {
                    say(format!("    {p}: walk {w:?}, table {t:?}"));
                }
                for (p, _, _) in starts.iter().take(6) {
                    // The folder and some of what's in it: in the table, and in the tree.
                    let Some(r) = recno(p) else { continue };
                    say(format!("  {p}: record {r}; the tree: {}", tree.debug(r)));
                    for l in sizes::describe(vol, &[r], names_ok(p)).unwrap_or_default() {
                        say(format!("    its record: {l}"));
                    }
                    for e in std::fs::read_dir(p).into_iter().flatten().flatten().take(6) {
                        let child = e.path().to_string_lossy().into_owned();
                        let Ok(f) = mft::root_frn(&child) else { continue };
                        let c = (f & 0x0000_FFFF_FFFF_FFFF) as u32;
                        say(format!("    inside: record {c} (seq {}); the tree: {}", f >> 48, tree.debug(c)));
                        for l in sizes::describe(vol, &[c], names_ok(p)).unwrap_or_default() {
                            say(format!("      {l}"));
                        }
                    }
                }
                for (p, _, _) in starts.iter().take(0) {
                    let recs = recno(p).map(|r| tree.in_folder(r)).unwrap_or_default();
                    say(format!("  the table, in {p}: {} records", recs.len()));
                    for l in sizes::describe(vol, &recs[..recs.len().min(40)], names_ok(p)).unwrap_or_default() {
                        say(format!("    {l}"));
                    }
                    say(format!("  the walk, in {p}:"));
                    for e in std::fs::read_dir(p).into_iter().flatten().flatten().take(40) {
                        use std::os::windows::fs::MetadataExt;
                        let (Ok(kind), Ok(m)) = (e.file_type(), e.metadata()) else { continue };
                        let name = e.file_name().to_string_lossy().into_owned();
                        let shown = if names_ok(p) { name } else { name.rsplit_once('.').map(|(_, x)| format!("*.{x}")).unwrap_or("*".into()) };
                        say(format!("    {shown:?} attributes {:#x} {} bytes{}", m.file_attributes(), m.len(), if kind.is_symlink() { " (symlink)" } else { "" }));
                    }
                }
                continue;
            }
            let tree = &drives[d].2;
            purge(&mut say);
            let t = Instant::now();
            let walked = walk(folder);
            let first = t.elapsed();
            let t = Instant::now();
            let _ = walk(folder);
            let again = t.elapsed();
            let from_table = table(tree, folder);
            let t = Instant::now();
            for _ in 0..1000 {
                let _ = table(tree, folder);
            }
            let ask = t.elapsed() / 1000;
            let same = walked.map(|(b, f, _)| (b, f)) == from_table;
            say(format!(
                "{folder}: walk {first:.2?} (again {again:.2?}): {}; table ({ask:.1?} to ask): {}; {}",
                walked.map_or("—".into(), |(b, f, p)| format!("{} in {f} files{}", gb(b), if p { ", partial" } else { "" })),
                from_table.map_or("—".into(), |(b, f)| format!("{} in {f} files", gb(b))),
                if same { "the same" } else { "DIFFERENT" }
            ));
            if how == "each" {
                let mut all = Vec::new();
                folders_in(folder, &mut all);
                let differ: Vec<String> = all
                    .iter()
                    .filter_map(|p| {
                        let (w, t) = (walk(p).map(|(b, f, _)| (b, f)), table(tree, p));
                        (w != t).then(|| format!("    {p}: walk {w:?}, table {t:?}"))
                    })
                    .collect();
                say(format!("  {} folders inside compared: {} differ", all.len(), differ.len()));
                for line in differ.into_iter().take(30) {
                    say(line);
                }
            }
        }
        if let Err(e) = std::fs::write(out, lines.join("\n") + "\n") {
            eprintln!("error: writing {out}: {e}");
            return 1;
        }
        0
    }

    /// `change:FOLDER` (see `sizes_bench`): changes made in FOLDER\changes, followed through the
    /// journal, must leave the tree as a fresh scan and a walk have it.
    fn change_check(vol: &mft::VolHandle, tree: &mut crate::search::sizes::Tree, folder: &str) -> String {
        use crate::search::sizes;
        let base = Path::new(folder).join("changes");
        let _ = std::fs::remove_dir_all(&base);
        let Ok(j) = vol.query_journal() else { return format!("{folder}: no change journal") };
        let start = j.next_usn;
        // Files made, grown, linked, moved and deleted; folders made, moved and deleted.
        let made = (|| -> std::io::Result<()> {
            let (a, b) = (base.join("a"), base.join("b"));
            std::fs::create_dir_all(a.join("deep").join("er"))?;
            std::fs::create_dir_all(&b)?;
            std::fs::write(a.join("one"), vec![1u8; 1000])?;
            std::fs::write(a.join("deep").join("er").join("two"), vec![2u8; 20_000])?;
            std::fs::write(b.join("three"), vec![3u8; 300])?;
            std::fs::hard_link(a.join("one"), b.join("one-again"))?;
            let mut f = std::fs::OpenOptions::new().append(true).open(b.join("three"))?;
            std::io::Write::write_all(&mut f, &[4u8; 4000])?;
            drop(f);
            std::fs::rename(a.join("deep"), b.join("deep"))?;
            std::fs::write(b.join("gone"), vec![5u8; 50])?;
            std::fs::remove_file(b.join("gone"))?;
            std::fs::create_dir_all(base.join("c").join("d"))?;
            std::fs::remove_dir_all(base.join("c"))?;
            std::os::windows::fs::symlink_file(a.join("one"), a.join("link"))?;
            Ok(())
        })();
        if let Err(e) = made {
            return format!("{folder}: couldn't make the changes ({e})");
        }
        std::thread::sleep(Duration::from_millis(500));
        // What changed, from the journal, looked up again and put in the tree.
        let t = Instant::now();
        let (mut at, mut changed, mut buf) = (start, Vec::new(), vec![0u64; 64 * 1024]);
        while let Ok((next, recs)) = vol.read_journal(j.id, at, &mut buf) {
            changed.extend(recs.iter().filter_map(|r| u32::try_from(mft::recno(r.frn)).ok()));
            if next == at {
                break;
            }
            at = next;
        }
        changed.sort_unstable();
        changed.dedup();
        let now = match sizes::look_up(vol, &changed) {
            Ok(n) => n,
            Err(e) => return format!("{folder}: look-up failed ({e})"),
        };
        let found = now.iter().filter(|(_, r)| r.is_some()).count();
        tree.update_all(now);
        let took = t.elapsed();
        let never = AtomicBool::new(false);
        let Ok((fresh, _)) = sizes::scan(vol, &never) else { return format!("{folder}: rescan failed") };
        let recno = |p: &Path| mft::root_frn(&p.to_string_lossy()).ok().map(|f| (f & 0x0000_FFFF_FFFF_FFFF) as u32);
        let mut report = format!("{folder}: {} records changed ({found} there now), followed in {took:.2?}", changed.len());
        let mut ok = true;
        for p in [base.clone(), base.join("a"), base.join("b"), base.join("b").join("deep"), Path::new(folder).to_path_buf()] {
            let r = recno(&p);
            let (t, f) = (r.and_then(|r| tree.dir(r)).map(|d| (d.bytes, d.files)), r.and_then(|r| fresh.dir(r)).map(|d| (d.bytes, d.files)));
            let w = crate::foldersize::count_folder(&p.to_string_lossy()).map(|s| (s.bytes, s.files.unwrap_or(0)));
            let good = t == f && t == w;
            ok &= good;
            report.push_str(&format!("\n    {}: followed {t:?}, fresh scan {f:?}, walk {w:?}{}", p.display(), if good { "" } else { "  DIFFERENT" }));
        }
        let _ = std::fs::remove_dir_all(&base);
        report.push_str(if ok { "\n  all the same" } else { "\n  NOT the same" });
        report
    }

    /// Empties Windows' file cache (the system cache's working set, then the standby list), so
    /// what's timed next reads from the disk.
    fn purge_file_cache() -> Result<(), String> {
        use windows::Win32::System::Memory::SetSystemFileCacheSize;
        enable_privilege("SeIncreaseQuotaPrivilege")?;
        enable_privilege("SeProfileSingleProcessPrivilege")?;
        unsafe { SetSystemFileCacheSize(usize::MAX, usize::MAX, Default::default()) }.map_err(|e| format!("emptying the file cache: {e}"))?;
        #[link(name = "ntdll")]
        unsafe extern "system" {
            fn NtSetSystemInformation(class: i32, info: *mut std::ffi::c_void, len: u32) -> i32;
        }
        const SYSTEM_MEMORY_LIST_INFORMATION: i32 = 80;
        let mut purge_standby_list = 4i32;
        let st = unsafe { NtSetSystemInformation(SYSTEM_MEMORY_LIST_INFORMATION, &mut purge_standby_list as *mut i32 as *mut _, 4) };
        if st != 0 {
            return Err(format!("purging the standby list: status {st:#x}"));
        }
        Ok(())
    }

    fn enable_privilege(name: &str) -> Result<(), String> {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_NOT_ALL_ASSIGNED, HANDLE, LUID};
        use windows::Win32::Security::{
            AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES,
            TOKEN_PRIVILEGES, TOKEN_QUERY,
        };
        use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
        let mut token = HANDLE::default();
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut token) }.map_err(|e| e.to_string())?;
        let w: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let mut luid = LUID::default();
        let r = unsafe { LookupPrivilegeValueW(PCWSTR::null(), PCWSTR(w.as_ptr()), &mut luid) }.and_then(|_| {
            let tp = TOKEN_PRIVILEGES { PrivilegeCount: 1, Privileges: [LUID_AND_ATTRIBUTES { Luid: luid, Attributes: SE_PRIVILEGE_ENABLED }] };
            unsafe { AdjustTokenPrivileges(token, false, Some(&tp), 0, None, None) }
        });
        // It succeeds without the privilege too, saying so only in the last error.
        let missing = r.is_ok() && unsafe { GetLastError() } == ERROR_NOT_ALL_ASSIGNED;
        unsafe {
            let _ = CloseHandle(token);
        }
        if missing {
            return Err(format!("{name} isn't held"));
        }
        r.map_err(|e| format!("{name}: {e}"))
    }
}

#[cfg(all(test, windows))]
mod tests {
    /// Only the installed service (in its admin-only folder) checks and installs an update: this
    /// test runs from elsewhere, so nothing is read or installed.
    #[test]
    fn only_the_installed_service_updates_itself() {
        let refused = super::update_from(r"C:\nowhere\ferret-indexer.exe").unwrap_err();
        assert!(refused.contains("only the installed service"), "{refused}");
    }

    use super::win::{restart_actions, restart_summary, restarts_already};
    use std::time::Duration;
    use windows_service::service::{ServiceAction, ServiceActionType, ServiceFailureActions, ServiceFailureResetPeriod};

    fn restart(secs: u64) -> ServiceAction {
        ServiceAction { action_type: ServiceActionType::Restart, delay: Duration::from_secs(secs) }
    }

    /// Restarted after 5 s, 30 s, then 2 minutes (every later crash), counted afresh after a day.
    #[test]
    fn a_crashed_service_is_restarted_after_5_s_30_s_then_2_min() {
        let a = restart_actions();
        assert_eq!(a.actions, Some(vec![restart(5), restart(30), restart(120)]));
        assert_eq!(a.reset_period, ServiceFailureResetPeriod::After(Duration::from_secs(86_400)));
        // Nothing else changed: no reboot message, no command.
        assert_eq!((a.reboot_msg, a.command), (None, None));
        // Every delay fits Windows' milliseconds (u32): converting them must not panic.
        for x in a.actions.unwrap() {
            assert_eq!(x.to_raw().Delay as u128, x.delay.as_millis());
        }
        assert_eq!(restart_summary(), "after 5 s, 30 s, then every 2 min; counted afresh after a day");
    }

    /// Set only when the service doesn't restart that way already (the service checks at every
    /// start, and shouldn't rewrite its configuration each time).
    #[test]
    fn recovery_is_set_only_when_it_differs() {
        assert!(restarts_already(&restart_actions()));
        // As Windows reports one that has them (with an empty reboot message and command).
        let mut reported = restart_actions();
        reported.reboot_msg = Some("".into());
        reported.command = Some("".into());
        assert!(restarts_already(&reported));
        // None at all: a service installed before this (sc qfailure shows no actions).
        let none = ServiceFailureActions {
            reset_period: ServiceFailureResetPeriod::After(Duration::ZERO),
            reboot_msg: None,
            command: None,
            actions: None,
        };
        assert!(!restarts_already(&none));
        assert!(!restarts_already(&ServiceFailureActions { actions: Some(vec![]), ..none.clone() }));
        // Other delays, another reset period, or another action.
        let mut other = restart_actions();
        other.actions = Some(vec![restart(60), restart(60), restart(60)]);
        assert!(!restarts_already(&other));
        let mut other = restart_actions();
        other.reset_period = ServiceFailureResetPeriod::Never;
        assert!(!restarts_already(&other));
        let mut other = restart_actions();
        other.actions = Some(vec![restart(5), restart(30), ServiceAction { action_type: ServiceActionType::None, delay: Duration::ZERO }]);
        assert!(!restarts_already(&other));
    }

    /// A service that isn't there: an error to log, not a panic (the service goes on running).
    #[test]
    fn recovery_for_a_missing_service_is_an_error() {
        let e = super::ensure_restart_on_failure("FerretNoSuchServiceForTests").unwrap_err();
        assert!(e.contains("FerretNoSuchServiceForTests"), "{e}");
    }

    /// Signed-in users get the right to start it (RP), once, and nothing else changes.
    #[test]
    fn users_are_let_start_the_service_once() {
        use super::win::with_users_start;
        // The service's DACL as Windows made it (read on the user's PC, 2026-11-11).
        let made = "D:(A;;CCLCSWRPWPDTLOCRRC;;;SY)(A;;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;BA)(A;;CCLCSWLOCRRC;;;IU)(A;;CCLCSWLOCRRC;;;SU)";
        let with = with_users_start(made).expect("users can't start it yet");
        assert_eq!(with, format!("D:(A;;RP;;;IU){}", &made[2..]));
        // Done already: left alone.
        assert_eq!(with_users_start(&with), None);
        assert_eq!(with_users_start("D:(A;;CCLCSWRPLOCRRC;;;IU)"), None);
        // A protected DACL keeps its flags first.
        assert_eq!(with_users_start("D:P(A;;GA;;;SY)").as_deref(), Some("D:P(A;;RP;;;IU)(A;;GA;;;SY)"));
        // RP as part of another right's letters isn't RP ("CRP…" is CR then P…).
        assert!(with_users_start("D:(A;;CRPC;;;IU)").is_some());
    }
}
