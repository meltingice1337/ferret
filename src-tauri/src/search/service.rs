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
//! ```

pub const SERVICE_NAME: &str = "FerretIndexer";
pub const DISPLAY_NAME: &str = "Ferret Indexer";

const HELP: &str = "\
ferret-indexer - whole-disk file index service for Ferret

USAGE:
  ferret-indexer --install       Register the FerretIndexer service (auto-start, LocalSystem) and start it [admin]
  ferret-indexer --uninstall     Stop and remove the service [admin]
  ferret-indexer --console       Run the MFT indexer + pipe server in the foreground (debugging) [admin]
  ferret-indexer --query <text>  Send a query to the running service and print results + timing
  ferret-indexer --status        Show service and index status
  ferret-indexer --probe         Report which MFT/USN operations work for the current user
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
pub use win::main_entry;

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
        ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode, ServiceInfo,
        ServiceStartType, ServiceState, ServiceStatus, ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    use windows_service::{define_windows_service, service_dispatcher};

    type Log = Arc<dyn Fn(String) + Send + Sync>;

    pub fn main_entry() -> i32 {
        let args: Vec<String> = std::env::args().skip(1).collect();
        match args.first().map(|s| s.as_str()) {
            Some("--install") => install(),
            Some("--uninstall") => uninstall(),
            Some("--console") => console(),
            Some("--service") => run_service_dispatcher(),
            Some("--query") => query_cli(&args[1..].join(" ")),
            Some("--status") => status_cli(),
            Some("--probe") => probe_cli(),
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

    fn install() -> i32 {
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
        let src = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        };
        let bin_dir = dir.join("bin");
        let exe = bin_dir.join("ferret-indexer.exe");
        if let Err(e) = std::fs::create_dir_all(&bin_dir).and_then(|_| std::fs::copy(&src, &exe)) {
            eprintln!("error: copying {} to {}: {e}", src.display(), exe.display());
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
        let svc = match manager.create_service(&info, ServiceAccess::CHANGE_CONFIG | ServiceAccess::START | ServiceAccess::QUERY_STATUS) {
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
        if let Err(e) = svc.start(&[] as &[&std::ffi::OsStr]) {
            eprintln!("error: service installed but failed to start: {e}");
            return 1;
        }
        println!("{SERVICE_NAME} installed ({}) and started.", exe.display());
        0
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
}
