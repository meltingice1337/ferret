//! A Ferret kept ready. `ferret.exe --standby` does everything a start does before its window
//! (the graphics device alone takes over half of a start) and then waits, without a window, for
//! a launch to be handed to it: the folder to open arrives over a pipe and its window is there
//! some 40 ms later instead of 200.
//!
//! - The Explorer redirect keeps one waiting while Ferret is the default file manager (`ensure`
//!   on its timer), unless the setting is off (`wanted`); it hands its own launches over, and
//!   so does every `ferret.exe` started any other way (the taskbar, a folder opened by another
//!   program) before it does anything else.
//! - The one that took a launch is an ordinary window from then on; the next is started after.
//! - Nothing is kept from before the launch: settings, session and arguments are read when the
//!   window is made, as in any start.
//!
//! The pipe's name carries the exe's path, so an installed Ferret and a build run from
//! somewhere else never answer each other. Only this user's programs can write to it (the
//! default protection of a pipe), a launch is handed only to a pipe served by that very exe
//! (any program can make a pipe with a name it guesses), and what's handed over is what
//! starting `ferret.exe` with arguments does anyway.

use serde::{Deserialize, Serialize};

pub const ARG: &str = "--standby";

/// A start handed over: the arguments after the exe, and the folder they're relative to.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Launch {
    pub args: Vec<String>,
    pub cwd: String,
}

/// What comes over the pipe, as one line of JSON.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Ask {
    Open(Launch),
    /// The setting was turned off: the one waiting exits.
    Quit,
}

/// The pipe a standby of `exe` waits on, in this sign-in session.
fn pipe_name(exe: &str, session: u32) -> String {
    // FNV-1a: the same name from every copy of the program, whatever built it.
    let tag = exe.to_lowercase().replace('/', "\\").bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3));
    format!(r"\\.\pipe\ferret-ready-{session}-{tag:016x}")
}

#[cfg(windows)]
mod win {
    use super::*;
    use std::time::Duration;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_FILE_NOT_FOUND, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FlushFileBuffers, ReadFile, WriteFile, FILE_FLAGS_AND_ATTRIBUTES, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_SHARE_MODE, OPEN_EXISTING,
        PIPE_ACCESS_DUPLEX,
    };
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeServerProcessId, WaitNamedPipeW, PIPE_READMODE_BYTE,
        PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
    };

    const KEY: &str = r"Software\Ferret";
    const VALUE: &str = "KeepReady";
    /// A launch is a line of JSON: arguments and a folder. Anything longer isn't one.
    const LONGEST: usize = 256 * 1024;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    fn name(exe: &str) -> String {
        use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
        let mut session = 0u32;
        // SAFETY: an out-parameter for this process's own id.
        let _ = unsafe { ProcessIdToSessionId(std::process::id(), &mut session) };
        pipe_name(&true_path(exe), session)
    }

    /// The file's path as Windows resolves it, so the helper (which reads the exe's path from
    /// the registry) and the standby (which asks what it runs as) name the same pipe however
    /// each came by the path: a short 8.3 name, a link, another case.
    fn true_path(path: &str) -> String {
        std::fs::canonicalize(path).map(|p| p.to_string_lossy().trim_start_matches(r"\\?\").to_string()).unwrap_or_else(|_| path.to_string())
    }

    /// A handle closed when dropped.
    struct Owned(HANDLE);
    impl Drop for Owned {
        fn drop(&mut self) {
            // SAFETY: the handle is this value's own, closed once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
    // SAFETY: a pipe handle can be used from any thread.
    unsafe impl Send for Owned {}

    /// Reads up to the end of a line.
    fn read_line(h: HANDLE) -> Option<String> {
        let mut all = Vec::new();
        let mut buf = [0u8; 4096];
        while !all.contains(&b'\n') && all.len() < LONGEST {
            let mut n = 0u32;
            // SAFETY: `buf` and `n` outlive the call; the read is synchronous.
            if unsafe { ReadFile(h, Some(&mut buf), Some(&mut n), None) }.is_err() || n == 0 {
                break;
            }
            all.extend_from_slice(&buf[..n as usize]);
        }
        let end = all.iter().position(|b| *b == b'\n')?;
        String::from_utf8(all[..end].to_vec()).ok()
    }

    fn write_all(h: HANDLE, mut bytes: &[u8]) -> bool {
        while !bytes.is_empty() {
            let mut n = 0u32;
            // SAFETY: `bytes` and `n` outlive the call; the write is synchronous.
            if unsafe { WriteFile(h, Some(bytes), Some(&mut n), None) }.is_err() || n == 0 {
                return false;
            }
            bytes = &bytes[n as usize..];
        }
        true
    }

    /// Whether Ferret keeps a copy ready (Settings; on unless turned off).
    pub fn wanted() -> bool {
        use winreg::{enums::HKEY_CURRENT_USER, RegKey};
        RegKey::predef(HKEY_CURRENT_USER).open_subkey(KEY).and_then(|k| k.get_value::<u32, _>(VALUE)).map(|v| v != 0).unwrap_or(true)
    }

    pub fn set_wanted(on: bool) -> Result<(), String> {
        use winreg::{enums::HKEY_CURRENT_USER, RegKey};
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(KEY).map_err(|e| e.to_string())?;
        key.set_value(VALUE, &(on as u32)).map_err(|e| e.to_string())
    }

    /// Whether a standby of `exe` waits (or is taking a launch this moment).
    pub fn waiting(exe: &str) -> bool {
        let name = wide(&name(exe));
        // SAFETY: a NUL-terminated name; the call only looks.
        unsafe { WaitNamedPipeW(PCWSTR(name.as_ptr()), 1).as_bool() || GetLastError() != ERROR_FILE_NOT_FOUND }
    }

    /// Starts a standby of `exe` unless one waits.
    pub fn ensure(exe: &str) {
        if waiting(exe) {
            return;
        }
        if let Err(e) = std::process::Command::new(exe).arg(ARG).spawn() {
            log::warn!("can't start a Ferret to keep ready: {e}");
        }
    }

    /// Connects to the standby of `exe` and sends `ask`; its process id once it answered "ok".
    fn send(exe: &str, ask: &Ask, allow_front: bool) -> Option<u32> {
        let name = wide(&name(exe));
        // SAFETY: a NUL-terminated name; the handle returned is owned below.
        let h = unsafe { CreateFileW(PCWSTR(name.as_ptr()), GENERIC_READ.0 | GENERIC_WRITE.0, FILE_SHARE_MODE(0), None, OPEN_EXISTING, FILE_FLAGS_AND_ATTRIBUTES(0), None) }.ok()?;
        let pipe = Owned(h);
        let mut pid = 0u32;
        // SAFETY: an out-parameter for a pipe handle this function owns.
        if unsafe { GetNamedPipeServerProcessId(pipe.0, &mut pid) }.is_err() || pid == 0 {
            return None;
        }
        // Only a pipe that very exe serves: any program could have made one with this name.
        if !exe_of(pid).is_some_and(|served_by| same_file(&served_by, exe)) {
            log::warn!("the pipe of the Ferret kept ready isn't served by {exe}: left alone");
            return None;
        }
        if allow_front {
            // SAFETY: plain call. It may fail (this process isn't in front either), which the
            // caller covers by raising the window itself.
            let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(pid) };
        }
        let mut line = serde_json::to_string(ask).ok()?;
        line.push('\n');
        // On a thread of its own: a standby that hangs must not hang this start with it.
        let (tell, heard) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            // The whole value moves here (a field alone is a raw handle, which a thread won't take).
            let pipe = pipe;
            let ok = write_all(pipe.0, line.as_bytes()) && read_line(pipe.0).is_some_and(|l| l.trim() == "ok");
            let _ = tell.send(ok);
        });
        matches!(heard.recv_timeout(Duration::from_millis(1500)), Ok(true)).then_some(pid)
    }

    /// Hands a start to the standby of `exe`, if one waits: the id of the process whose window
    /// it becomes. None: start Ferret the usual way.
    pub fn hand_over(exe: &str, launch: &Launch) -> Option<u32> {
        send(exe, &Ask::Open(launch.clone()), true)
    }

    /// Tells the standby of `exe`, if one waits, to exit (the setting was turned off).
    pub fn dismiss(exe: &str) {
        if waiting(exe) {
            let _ = send(exe, &Ask::Quit, false);
        }
    }

    /// The exe a process runs.
    fn exe_of(pid: u32) -> Option<String> {
        use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION};
        // SAFETY: the handle is closed by `Owned`; the buffer and its length outlive the call.
        unsafe {
            let process = Owned(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?);
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            QueryFullProcessImageNameW(process.0, PROCESS_NAME_FORMAT(0), windows::core::PWSTR(buf.as_mut_ptr()), &mut len).ok()?;
            Some(String::from_utf16_lossy(&buf[..len as usize]))
        }
    }

    fn same_file(a: &str, b: &str) -> bool {
        let plain = |p: &str| true_path(p).replace('/', "\\").to_lowercase();
        plain(a) == plain(b)
    }

    /// Whether the process is still running.
    pub fn alive(pid: u32) -> bool {
        use windows::Win32::Foundation::WAIT_TIMEOUT;
        use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
        // SAFETY: the handle is closed by `Owned`.
        unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid).map(Owned).is_ok_and(|p| WaitForSingleObject(p.0, 0) == WAIT_TIMEOUT) }
    }

    /// This process becomes the standby of `exe` (its own): `woken` is called, on another
    /// thread, with the launch handed to it, once. While it waits, `keep` is asked every half
    /// minute whether it should (the redirect still runs), and it exits when not, or when told
    /// to. False when another standby already waits: this one has nothing to do.
    pub fn wait(exe: &str, keep: fn() -> bool, woken: impl FnOnce(Launch) + Send + 'static) -> bool {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let name = wide(&name(exe));
        // One instance, the first of its name: a second standby (or anything else that made a
        // pipe with this name first) is refused.
        // SAFETY: a NUL-terminated name; the handle returned is owned below.
        let h = unsafe {
            CreateNamedPipeW(
                PCWSTR(name.as_ptr()),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                4096,
                4096,
                0,
                None,
            )
        };
        if h.is_invalid() {
            return false;
        }
        let pipe = Owned(h);
        let waiting = Arc::new(AtomicBool::new(true));
        let still = waiting.clone();
        std::thread::spawn(move || {
            while still.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_secs(30));
                if still.load(Ordering::Acquire) && !keep() {
                    std::process::exit(0);
                }
            }
        });
        std::thread::spawn(move || {
            let mut woken = Some(woken);
            // As in `send`: the whole value, not its raw handle.
            let pipe = pipe;
            loop {
                // SAFETY: the pipe is this thread's; the calls are synchronous.
                let connected = match unsafe { ConnectNamedPipe(pipe.0, None) } {
                    Ok(()) => true,
                    Err(e) => e.code() == ERROR_PIPE_CONNECTED.to_hresult(),
                };
                if !connected {
                    std::thread::sleep(Duration::from_millis(200));
                    continue;
                }
                let ask = read_line(pipe.0).and_then(|l| serde_json::from_str::<Ask>(&l).ok());
                match ask {
                    Some(Ask::Open(launch)) => {
                        waiting.store(false, Ordering::Release);
                        write_all(pipe.0, b"ok\n");
                        // SAFETY: as above. The pipe closes on return: the next standby makes its own.
                        unsafe {
                            let _ = FlushFileBuffers(pipe.0);
                            let _ = DisconnectNamedPipe(pipe.0);
                        }
                        if let Some(woken) = woken.take() {
                            woken(launch);
                        }
                        return;
                    }
                    Some(Ask::Quit) => {
                        write_all(pipe.0, b"ok\n");
                        // SAFETY: as above.
                        unsafe {
                            let _ = FlushFileBuffers(pipe.0);
                        }
                        std::process::exit(0);
                    }
                    // Not a launch: whoever it was is let go, and the wait goes on.
                    None => unsafe {
                        let _ = DisconnectNamedPipe(pipe.0);
                    },
                }
            }
        });
        true
    }
}

#[cfg(windows)]
pub use win::{alive, dismiss, ensure, hand_over, set_wanted, wait, waiting, wanted};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_names_tell_copies_and_sessions_apart() {
        let installed = pipe_name(r"C:\Users\me\AppData\Local\Ferret\ferret.exe", 1);
        assert!(installed.starts_with(r"\\.\pipe\ferret-ready-1-"));
        // The same exe, however its path is written.
        assert_eq!(installed, pipe_name(r"c:/users/me/appdata/local/ferret/FERRET.EXE", 1));
        assert_ne!(installed, pipe_name(r"C:\dev\target\release\ferret-gpui.exe", 1));
        assert_ne!(installed, pipe_name(r"C:\Users\me\AppData\Local\Ferret\ferret.exe", 2));
    }

    #[test]
    fn asks_are_one_line_of_json() {
        let launch = Launch { args: vec!["/select,D:\\a b\\c.txt".into()], cwd: "C:\\x".into() };
        let line = serde_json::to_string(&Ask::Open(launch.clone())).unwrap();
        assert!(!line.contains('\n'));
        assert_eq!(serde_json::from_str::<Ask>(&line).unwrap(), Ask::Open(launch));
        assert_eq!(serde_json::from_str::<Ask>(&serde_json::to_string(&Ask::Quit).unwrap()).unwrap(), Ask::Quit);
        assert!(serde_json::from_str::<Ask>("{\"open\":5}").is_err());
    }

    /// A standby in this very process: a launch handed over reaches it, with the folder asked
    /// for; a second one finds nobody waiting.
    #[cfg(windows)]
    #[test]
    fn a_launch_reaches_the_one_waiting() {
        let exe = std::env::current_exe().unwrap().to_string_lossy().into_owned();
        assert!(!waiting(&exe));
        let (tell, heard) = std::sync::mpsc::channel();
        assert!(wait(&exe, || true, move |launch| tell.send(launch).unwrap()));
        // A second standby of the same exe is refused.
        assert!(!wait(&exe, || true, |_| panic!("a second standby took a launch")));
        assert!(waiting(&exe));
        let launch = Launch { args: vec!["D:\\Downloads".into()], cwd: "C:\\".into() };
        assert_eq!(hand_over(&exe, &launch), Some(std::process::id()));
        assert_eq!(heard.recv_timeout(std::time::Duration::from_secs(5)).unwrap(), launch);
        // It's a window now: nobody waits, and the next launch starts Ferret the usual way.
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(!waiting(&exe));
        assert_eq!(hand_over(&exe, &launch), None);
        assert!(alive(std::process::id()));
    }
}
