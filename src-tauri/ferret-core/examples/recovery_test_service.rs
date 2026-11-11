//! A throwaway Windows service, to try the search service's crash recovery (`search::service`'s
//! `ensure_restart_on_failure` and `restart_on_failure`) for real without touching FerretIndexer.
//! It never indexes anything; it only writes to the log it's given. A script, run as
//! administrator, creates it, lets it crash and deletes it (see `--help`).
//!
//! ```text
//! recovery_test_service --create NAME LOG   creates the service NAME running this exe, setting its
//!                                           recovery as `ferret-indexer --install` does [admin]
//! recovery_test_service --service NAME LOG  how Windows runs it: it sets its own recovery as the
//!                                           search service does at every start, notes the start
//!                                           in LOG, and its first two runs end as a crash does
//!                                           (the process exits without saying it stopped)
//! ```

#[cfg(windows)]
fn main() {
    std::process::exit(win::main());
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Windows only");
}

#[cfg(windows)]
mod win {
    use ferret_core::search::service::{ensure_restart_on_failure, restart_on_failure};
    use std::ffi::OsString;
    use std::io::Write;
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
    use std::sync::{Arc, OnceLock};
    use std::time::Duration;
    use windows_service::service::{
        ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode, ServiceInfo,
        ServiceStartType, ServiceState, ServiceStatus, ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    use windows_service::{define_windows_service, service_dispatcher};

    /// The service's name and its log, from its command line.
    static ARGS: OnceLock<(String, String)> = OnceLock::new();

    /// Runs that end as a crash, before one keeps running.
    const CRASHES: usize = 2;

    define_windows_service!(ffi_service_main, service_main);

    pub fn main() -> i32 {
        let args: Vec<String> = std::env::args().skip(1).collect();
        match (args.first().map(String::as_str), args.get(1), args.get(2)) {
            (Some("--create"), Some(name), Some(log)) => create(name, log),
            (Some("--service"), Some(name), Some(log)) => {
                let _ = ARGS.set((name.clone(), log.clone()));
                match service_dispatcher::start(name, ffi_service_main) {
                    Ok(()) => 0,
                    Err(e) => {
                        eprintln!("not started by Windows' service control manager ({e})");
                        1
                    }
                }
            }
            _ => {
                eprintln!("usage: recovery_test_service --create NAME LOG | --service NAME LOG");
                2
            }
        }
    }

    fn note(log: &str, line: &str) {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
            let _ = writeln!(f, "{} {line}", chrono::Local::now().format("%H:%M:%S%.3f"));
        }
    }

    fn service_main(_args: Vec<OsString>) {
        let Some((name, log)) = ARGS.get().cloned() else { return };
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let handler = move |c| match c {
            ServiceControl::Stop => {
                stop2.store(true, Relaxed);
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        };
        let Ok(status) = service_control_handler::register(&name, handler) else { return };
        let set = |state: ServiceState| {
            let _ = status.set_service_status(ServiceStatus {
                service_type: ServiceType::OWN_PROCESS,
                current_state: state,
                controls_accepted: if state == ServiceState::Running { ServiceControlAccept::STOP } else { ServiceControlAccept::empty() },
                exit_code: ServiceExitCode::Win32(0),
                checkpoint: 0,
                wait_hint: Duration::from_secs(5),
                process_id: None,
            });
        };
        set(ServiceState::Running);
        let run = std::fs::read_to_string(&log).map(|s| s.lines().filter(|l| l.contains(" start ")).count()).unwrap_or(0) + 1;
        // As the search service does at every start (service.rs, service_main).
        let recovery = match ensure_restart_on_failure(&name) {
            Ok(true) => "recovery set".to_string(),
            Ok(false) => "recovery already set".to_string(),
            Err(e) => format!("recovery NOT set: {e}"),
        };
        note(&log, &format!("start {run} (process {}): {recovery}", std::process::id()));
        if run <= CRASHES {
            std::thread::sleep(Duration::from_secs(1));
            note(&log, &format!("crash {run}"));
            // Gone without saying it stopped: to Windows, a crash ("terminated unexpectedly").
            std::process::exit(3);
        }
        while !stop.load(Relaxed) {
            std::thread::sleep(Duration::from_millis(200));
        }
        note(&log, "stopped");
        set(ServiceState::Stopped);
    }

    /// As `ferret-indexer --install` creates FerretIndexer (service.rs, put_in_place), recovery
    /// included; it isn't started.
    fn create(name: &str, log: &str) -> i32 {
        let exe = match std::env::current_exe() {
            Ok(e) => e,
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        };
        let manager = match ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("error: the service manager ({e}): run this as administrator");
                return 5;
            }
        };
        let info = ServiceInfo {
            name: name.into(),
            display_name: format!("{name} (Ferret recovery test)").into(),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::OnDemand,
            error_control: ServiceErrorControl::Normal,
            executable_path: exe,
            launch_arguments: vec!["--service".into(), name.into(), log.into()],
            dependencies: vec![],
            account_name: None, // LocalSystem, as FerretIndexer
            account_password: None,
        };
        let access = ServiceAccess::CHANGE_CONFIG | ServiceAccess::START | ServiceAccess::QUERY_STATUS | ServiceAccess::QUERY_CONFIG;
        let svc = match manager.create_service(&info, access) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: creating {name}: {e}");
                return 1;
            }
        };
        match restart_on_failure(&svc) {
            Ok(set) => {
                println!("{name} created; recovery {}", if set { "set" } else { "already set" });
                0
            }
            Err(e) => {
                eprintln!("error: {name} created, recovery NOT set: {e:?}");
                1
            }
        }
    }
}
