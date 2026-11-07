//! Network folders (SMB), such as a NAS: signing in to one, and the computers Windows finds on the
//! network. Ferret asks for the user name and password itself and hands them to Windows. They're
//! kept only when Remember is ticked, and then by Windows, in Credential Manager, where Explorer
//! keeps its own. Windows' policies stay as they are: where it blocks guest access, the answer is
//! an account on the server, never a weaker setting.

use crate::paths;
use serde::Serialize;

/// The start of an error that means "this needs a user name and password" (the UI asks). What
/// follows, if anything, says why.
pub const LOGIN_PREFIX: &str = "netlogin:";
/// The error `network_connect` gives for a user name or password that wasn't accepted.
pub const WRONG_LOGIN: &str = "wrong-login";

const INVALID_PASSWORD: i32 = 86;
const LOGON_FAILURE: i32 = 1326;
const BAD_USERNAME: i32 = 2202;
const GUEST_BLOCKED: i32 = 1272;
const ALREADY_CONNECTED_OTHERWISE: i32 = 1219;
const ACCESS_DENIED: i32 = 5;
const NO_SUCH_SHARE: i32 = 67;

/// Errors that a user name and password could get past.
const LOGIN: &[i32] = &[
    INVALID_PASSWORD, LOGON_FAILURE, BAD_USERNAME, GUEST_BLOCKED,
    1244, // not authenticated
    1311, // no logon servers
    1312, // no such logon session
    1327, // account restriction
    1328, // outside the account's hours
    1329, // not allowed on this computer
    1330, // password expired
    1331, // account disabled
    1385, // that kind of sign-in isn't granted
    1907, // password must change
    1909, // account locked
];

/// Errors that mean the server (or the way to it) isn't there.
const UNREACHABLE: &[i32] = &[
    51,   // remote computer not available
    53,   // network path not found
    59,   // unexpected network error
    64,   // network name no longer available
    121,  // semaphore timeout
    1203, // no network provider accepted the path
    1222, // no network
    1231, // network location can't be reached
    1232, // host unreachable
    1236, // connection aborted locally
    2114, // the server's service isn't running
];

/// What a path's server doesn't answer when it won't list its shares.
const NO_SHARE_LIST: &[i32] = &[50, 1722, 2138];

/// `\\nas\media\photos` → `("nas", Some("media"))`, `\\nas` → `("nas", None)`. Only Windows
/// network paths: not WSL, Ferret's own server paths or `\\?\` device paths.
pub fn parts(path: &str) -> Option<(&str, Option<&str>)> {
    if paths::is_wsl(path) || paths::is_remote(path) {
        return None;
    }
    let rest = path.strip_prefix(r"\\")?;
    if rest.starts_with(['?', '.']) {
        return None;
    }
    let mut it = rest.split('\\').filter(|s| !s.is_empty());
    let server = it.next()?;
    Some((server, it.next()))
}

/// What to connect to for a path: its share, or for a server on its own, the server's IPC$
/// (what listing its shares goes through).
fn target_of(path: &str) -> Option<String> {
    let (server, share) = parts(path)?;
    Some(format!(r"\\{server}\{}", share.unwrap_or("IPC$")))
}

/// The Windows error code in a message from `std::io::Error` or `listing::shares`.
fn os_error(text: &str) -> Option<i32> {
    let (_, rest) = text.rsplit_once("(os error ")?;
    rest.strip_suffix(')')?.parse().ok()
}

fn cant_reach(server: &str) -> String {
    format!("Can't reach “{server}”. Check that it's on and on this network, or try its IP address")
}

const GUEST_REASON: &str = "Windows doesn't allow guest access to shared folders, so this needs an account on it";

fn wont_share(server: &str) -> String {
    format!("“{server}” is on the network, but Windows can't open its shared folders. Its file sharing may be off, or set up in a way Windows won't accept")
}

/// Whether anything answers on the file sharing port: tells a server that's off from one whose
/// sharing Windows won't use. Only asked once opening it has already failed.
fn answers_on_smb_port(server: &str) -> bool {
    use std::net::{TcpStream, ToSocketAddrs};
    let Ok(mut addrs) = (server, 445).to_socket_addrs() else { return false };
    addrs.next().is_some_and(|a| TcpStream::connect_timeout(&a, std::time::Duration::from_millis(800)).is_ok())
}

/// Says what a failure to open a network folder means, in place of Windows' message. Other
/// paths' errors pass through. Asked only when it matters: `share_is_closed`, whether the
/// folder's share itself is closed to this user, and `port_open`, whether the server answers.
fn explain_with(path: &str, error: String, share_is_closed: impl Fn(&str) -> bool, port_open: impl Fn(&str) -> bool) -> String {
    let Some((server, share)) = parts(path) else { return error };
    if let Some(denied) = error.strip_prefix("denied:") {
        let root = target_of(path).unwrap_or_default();
        if paths::key(denied) == paths::key(&root) || share_is_closed(&root) {
            return LOGIN_PREFIX.to_string();
        }
        let folder = denied.rsplit('\\').next().unwrap_or(denied);
        return format!("Your account on “{server}” can't open “{folder}”");
    }
    let Some(code) = os_error(&error) else { return error };
    match code {
        GUEST_BLOCKED => format!("{LOGIN_PREFIX}{GUEST_REASON}"),
        c if LOGIN.contains(&c) => LOGIN_PREFIX.to_string(),
        // A server on its own that won't say what it shares to someone who hasn't signed in.
        ACCESS_DENIED if share.is_none() => LOGIN_PREFIX.to_string(),
        ALREADY_CONNECTED_OTHERWISE => already_connected(server),
        NO_SUCH_SHARE => format!("“{server}” has no shared folder named “{}”", share.unwrap_or_default()),
        // A server that answers but won't say what it shares: often it only offers that to
        // guests, which Windows blocks, and it comes back as "path not found". An account helps.
        c if UNREACHABLE.contains(&c) && port_open(server) => match share {
            None => format!("{LOGIN_PREFIX}“{server}” is on the network, but it only lists its shared folders to someone signed in"),
            Some(_) => wont_share(server),
        },
        c if UNREACHABLE.contains(&c) => cant_reach(server),
        c if share.is_none() && NO_SHARE_LIST.contains(&c) => {
            format!(r"“{server}” doesn't list its shared folders. Type one's address, like \\{server}\media")
        }
        _ => error,
    }
}

pub fn explain(path: &str, error: String) -> String {
    explain_with(path, error, |root| {
        matches!(std::fs::read_dir(paths::for_io(root)), Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied)
    }, answers_on_smb_port)
}

/// A network path Windows wouldn't say anything about until someone signs in, or that isn't
/// there right now: worth opening anyway, so the UI can ask, or say what's wrong.
pub fn worth_opening(path: &str, error: &std::io::Error) -> bool {
    parts(path).is_some() && error.raw_os_error().is_some_and(|c| {
        LOGIN.contains(&c) || UNREACHABLE.contains(&c) || c == ACCESS_DENIED || c == ALREADY_CONNECTED_OTHERWISE
    })
}

fn already_connected(server: &str) -> String {
    format!("Windows is already connected to “{server}” with another user name. Close what's open there and try again")
}

/// What to tell someone whose sign-in failed. A wrong user name or password is `WRONG_LOGIN`.
fn connect_error(code: i32, server: &str, share: Option<&str>) -> String {
    match code {
        INVALID_PASSWORD | LOGON_FAILURE | BAD_USERNAME => WRONG_LOGIN.to_string(),
        ACCESS_DENIED => format!("That account can't open this on “{server}”"),
        // A server set to treat unknown users as guests: Windows then blocks the guest.
        GUEST_BLOCKED => format!("“{server}” doesn't know that account, and Windows doesn't allow guest access"),
        1330 | 1907 => format!("That account's password has to be changed on “{server}” first"),
        1331 => format!("That account is turned off on “{server}”"),
        1909 => format!("That account is locked on “{server}”. Try again later"),
        1385 => format!("“{server}” doesn't let that account sign in over the network"),
        ALREADY_CONNECTED_OTHERWISE => already_connected(server),
        NO_SUCH_SHARE => format!("“{server}” has no shared folder named “{}”", share.unwrap_or_default()),
        c if UNREACHABLE.contains(&c) => cant_reach(server),
        c => std::io::Error::from_raw_os_error(c).to_string(),
    }
}

#[cfg(windows)]
mod win {
    use windows::Win32::NetworkManagement::WNet::{
        WNetAddConnection2W, WNetCancelConnection2W, CONNECT_TEMPORARY, NETRESOURCEW, NET_CONNECT_FLAGS, RESOURCETYPE_ANY,
    };
    use windows::Win32::Security::Credentials::{CredDeleteW, CredWriteW, CREDENTIALW, CRED_FLAGS, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_DOMAIN_PASSWORD};
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{
        BHID_EnumItems, FOLDERID_NetworkFolder, IEnumShellItems, IShellItem, SHGetKnownFolderItem, KF_FLAG_DEFAULT,
        SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_NORMALDISPLAY,
    };
    use windows::core::{PCWSTR, PWSTR};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    /// Connects to `remote`; no user and password means as the Windows user. Asks nothing on
    /// screen itself: Ferret does the asking. Returns Windows' error code (0 when connected).
    pub fn connect(remote: &str, user: Option<&str>, password: Option<&str>) -> i32 {
        let mut remote = wide(remote);
        let res = NETRESOURCEW { dwType: RESOURCETYPE_ANY, lpRemoteName: PWSTR(remote.as_mut_ptr()), ..Default::default() };
        let user = user.map(wide);
        let mut password = password.map(wide);
        let code = unsafe {
            WNetAddConnection2W(
                &res,
                password.as_ref().map_or(PCWSTR::null(), |p| PCWSTR(p.as_ptr())),
                user.as_ref().map_or(PCWSTR::null(), |u| PCWSTR(u.as_ptr())),
                CONNECT_TEMPORARY,
            )
        };
        if let Some(p) = password.as_mut() {
            p.fill(0);
        }
        code.0 as i32
    }

    /// Closes a connection, unless something's still open on it.
    pub fn close(remote: &str) {
        let remote = wide(remote);
        unsafe {
            let _ = WNetCancelConnection2W(PCWSTR(remote.as_ptr()), NET_CONNECT_FLAGS(0), false);
        }
    }

    /// Keeps the password as `cmdkey /add` would, and Explorer's "Remember my credentials" does:
    /// a credential for that server, which Windows then uses for it by itself.
    pub fn remember(server: &str, user: &str, password: &str) -> Result<(), String> {
        let mut target = wide(server);
        let mut user = wide(user);
        let mut blob: Vec<u8> = password.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let cred = CREDENTIALW {
            Flags: CRED_FLAGS(0),
            Type: CRED_TYPE_DOMAIN_PASSWORD,
            TargetName: PWSTR(target.as_mut_ptr()),
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            UserName: PWSTR(user.as_mut_ptr()),
            ..Default::default()
        };
        let res = unsafe { CredWriteW(&cred, 0) }.map_err(|e| e.to_string());
        blob.fill(0);
        res
    }

    pub fn forget(server: &str) {
        let target = wide(server);
        unsafe {
            let _ = CredDeleteW(PCWSTR(target.as_ptr()), CRED_TYPE_DOMAIN_PASSWORD, None);
        }
    }

    fn display_name(item: &IShellItem, kind: windows::Win32::UI::Shell::SIGDN) -> Option<String> {
        unsafe {
            let p = item.GetDisplayName(kind).ok()?;
            let s = p.to_string().ok();
            CoTaskMemFree(Some(p.0 as *const _));
            s
        }
    }

    /// What Explorer's Network lists, as (name, path), without what isn't a computer with shared
    /// folders (TVs, printers). The first look takes a while: Windows waits for answers.
    pub fn computers() -> Result<Vec<(String, String)>, String> {
        let root: IShellItem = unsafe { SHGetKnownFolderItem(&FOLDERID_NetworkFolder, KF_FLAG_DEFAULT, None) }.map_err(|e| e.to_string())?;
        let items: IEnumShellItems = unsafe { root.BindToHandler(None, &BHID_EnumItems) }.map_err(|e: windows::core::Error| e.to_string())?;
        let mut out = Vec::new();
        loop {
            let mut one: [Option<IShellItem>; 1] = [None];
            let mut got = 0;
            if unsafe { items.Next(&mut one, Some(&mut got)) }.is_err() || got == 0 {
                break;
            }
            let Some(item) = one[0].take() else { break };
            if let (Some(path), Some(name)) = (display_name(&item, SIGDN_DESKTOPABSOLUTEPARSING), display_name(&item, SIGDN_NORMALDISPLAY)) {
                out.push((name, path));
            }
        }
        Ok(out)
    }
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Computer {
    pub name: String,
    pub path: String,
}

/// Signs in to the server of a network path. Returns whether the password was kept, when asked
/// to keep it (being connected matters more, so that failing isn't an error).
#[cfg(windows)]
#[tauri::command(async)]
pub fn network_connect(path: String, user: Option<String>, password: Option<String>, remember: bool) -> Result<bool, String> {
    let Some((server, share)) = parts(&path) else { return Err("That isn't a network folder".into()) };
    let target = target_of(&path).ok_or("That isn't a network folder")?;
    let user = user.as_deref().map(str::trim).filter(|u| !u.is_empty());
    // With a user name, an empty password means no password rather than "the Windows user's".
    let password = user.map(|_| password.as_deref().unwrap_or(""));
    let mut code = win::connect(&target, user, password);
    if code == ALREADY_CONNECTED_OTHERWISE {
        // Windows allows one user name per server: what this window opened before goes first.
        win::close(&target);
        win::close(&format!(r"\\{server}\IPC$"));
        code = win::connect(&target, user, password);
    }
    if code != 0 {
        log::info!("network: sign-in to {target} failed with error {code}");
        return Err(connect_error(code, server, share));
    }
    log::info!("network: signed in to {target}");
    let (Some(user), true) = (user, remember) else { return Ok(false) };
    match win::remember(server, user, password.unwrap_or("")) {
        Ok(()) => Ok(true),
        Err(e) => {
            log::warn!("network: couldn't keep the password for {server}: {e}");
            Ok(false)
        }
    }
}

/// Signs out of a server, and forgets a password kept for it.
#[cfg(windows)]
#[tauri::command(async)]
pub fn network_forget(path: String) -> Result<(), String> {
    let (server, _) = parts(&path).ok_or("That isn't a network folder")?;
    if let Some(target) = target_of(&path) {
        win::close(&target);
    }
    win::close(&format!(r"\\{server}\IPC$"));
    win::forget(server);
    log::info!("network: signed out of {server}");
    Ok(())
}

/// The computers Windows finds on the network, without this one.
fn only_computers(found: Vec<(String, String)>, this_one: &str) -> Vec<Computer> {
    let mut out: Vec<Computer> = found
        .into_iter()
        .filter(|(_, path)| crate::listing::server_root(path).is_some() && parts(path).is_some())
        .filter(|(_, path)| !path[2..].trim_end_matches('\\').eq_ignore_ascii_case(this_one))
        .map(|(name, path)| Computer { name, path: path.trim_end_matches('\\').to_string() })
        .collect();
    out.sort_by_key(|c| c.name.to_lowercase());
    out.dedup_by(|a, b| a.path.eq_ignore_ascii_case(&b.path));
    out
}

#[cfg(windows)]
#[tauri::command]
pub async fn network_computers() -> Result<Vec<Computer>, String> {
    let found = crate::shell::blocking_com(win::computers).await?;
    log::info!("network: Windows lists {} items on the network", found.len());
    Ok(only_computers(found, &std::env::var("COMPUTERNAME").unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn explained(path: &str, error: &str) -> String {
        explain_with(path, error.to_string(), |_| false, |_| false)
    }

    #[test]
    fn parts_are_only_for_windows_network_paths() {
        assert_eq!(parts(r"\\nas"), Some(("nas", None)));
        assert_eq!(parts(r"\\nas\"), Some(("nas", None)));
        assert_eq!(parts(r"\\nas\media"), Some(("nas", Some("media"))));
        assert_eq!(parts(r"\\nas\media\photos\2026"), Some(("nas", Some("media"))));
        assert_eq!(parts(r"C:\Users"), None);
        assert_eq!(parts(r"\\wsl.localhost\Ubuntu\home"), None);
        assert_eq!(parts(r"\\ssh:\home.local\srv"), None);
        assert_eq!(parts(r"\\?\C:\Users"), None);
    }

    #[test]
    fn connects_to_the_share_or_the_servers_ipc() {
        assert_eq!(target_of(r"\\nas\media\photos").as_deref(), Some(r"\\nas\media"));
        assert_eq!(target_of(r"\\nas").as_deref(), Some(r"\\nas\IPC$"));
        assert_eq!(target_of(r"C:\Users"), None);
    }

    #[test]
    fn reads_the_code_in_an_error() {
        assert_eq!(os_error("The user name or password is incorrect. (os error 1326)"), Some(1326));
        assert_eq!(os_error("Can't list the shared folders of “nas” (os error 5)"), Some(5));
        assert_eq!(os_error("password-required"), None);
    }

    #[test]
    fn asks_for_a_login_when_one_could_help() {
        assert_eq!(explained(r"\\nas\media", "The user name or password is incorrect. (os error 1326)"), LOGIN_PREFIX);
        assert_eq!(explained(r"\\nas", "Can't list the shared folders of “nas” (os error 5)"), LOGIN_PREFIX);
        // The share itself turned down this user.
        assert_eq!(explained(r"\\nas\media", r"denied:\\nas\media"), LOGIN_PREFIX);
        assert!(explained(r"\\nas\media", "blocked (os error 1272)").starts_with(LOGIN_PREFIX));
        assert!(explained(r"\\nas\media", "blocked (os error 1272)").contains("guest"));
    }

    #[test]
    fn a_closed_folder_inside_an_open_share_doesnt_ask() {
        let said = explained(r"\\nas\media\private", r"denied:\\nas\media\private");
        assert_eq!(said, "Your account on “nas” can't open “private”");
        // When the share is closed too, signing in is what could help.
        let closed = explain_with(r"\\nas\media\private", r"denied:\\nas\media\private".to_string(), |root| root == r"\\nas\media", |_| false);
        assert_eq!(closed, LOGIN_PREFIX);
    }

    #[test]
    fn tells_a_server_thats_off_from_one_whose_sharing_windows_wont_use() {
        let not_found = "The network path was not found. (os error 53)";
        assert!(explain_with(r"\\nas", not_found.to_string(), |_| false, |_| false).starts_with("Can't reach “nas”"));
        // A server on its own that answers, as a Samba that only offers its list to guests does.
        let listing = explain_with(r"\\nas", not_found.to_string(), |_| false, |server| server == "nas");
        assert!(listing.starts_with(LOGIN_PREFIX), "{listing}");
        // A share on one: signing in wouldn't tell Windows where to find it.
        let share = explain_with(r"\\nas\media", not_found.to_string(), |_| false, |server| server == "nas");
        assert!(share.starts_with("“nas” is on the network"), "{share}");
    }

    #[test]
    fn says_what_went_wrong_otherwise() {
        assert!(explained(r"\\nas\media", "The network path was not found. (os error 53)").starts_with("Can't reach “nas”"));
        assert_eq!(explained(r"\\nas\nope", "The network name cannot be found. (os error 67)"), "“nas” has no shared folder named “nope”");
        assert!(explained(r"\\nas", "Can't list the shared folders of “nas” (os error 1722)").contains(r"\\nas\media"));
        assert!(explained(r"\\nas\media", "other (os error 1219)").contains("another user name"));
    }

    #[test]
    fn leaves_everything_else_alone() {
        assert_eq!(explained(r"C:\Windows", "The user name or password is incorrect. (os error 1326)"), "The user name or password is incorrect. (os error 1326)");
        assert_eq!(explained(r"\\wsl.localhost\Ubuntu\root", r"denied:\\wsl.localhost\Ubuntu\root"), r"denied:\\wsl.localhost\Ubuntu\root");
        assert_eq!(explained(r"\\nas\media\a.zip", "password-required"), "password-required");
        // A folder gone from a share that's there is just gone.
        assert_eq!(explained(r"\\nas\media\gone", "not found (os error 3)"), "not found (os error 3)");
    }

    #[test]
    fn a_wrong_password_is_told_apart() {
        for code in [INVALID_PASSWORD, LOGON_FAILURE, BAD_USERNAME] {
            assert_eq!(connect_error(code, "nas", None), WRONG_LOGIN);
        }
        assert!(connect_error(1331, "nas", None).contains("turned off"));
        assert!(connect_error(53, "nas", None).starts_with("Can't reach"));
    }

    #[test]
    fn worth_opening_only_for_network_paths_that_might_open() {
        let err = |c| std::io::Error::from_raw_os_error(c);
        assert!(worth_opening(r"\\nas\media", &err(1326)));
        assert!(worth_opening(r"\\nas\media", &err(53)));
        assert!(worth_opening(r"\\nas\media", &err(5)));
        assert!(!worth_opening(r"\\nas\media\gone", &err(3)));
        assert!(!worth_opening(r"C:\gone", &err(1326)));
        assert!(!worth_opening(r"\\wsl.localhost\Ubuntu\gone", &err(53)));
    }

    #[test]
    fn keeps_only_computers_that_arent_this_one() {
        let found = vec![
            ("Dining room TV".to_string(), r"::{F02C1A0D-BE21-4350-88B0-7367FC96EF3C}\Provider\Microsoft.Networking.SSDP//uuid:14cc".to_string()),
            ("Printer".to_string(), r"::{F02C1A0D-BE21-4350-88B0-7367FC96EF3C}\Provider\Microsoft.Networking.WSD//urn:uuid:e324".to_string()),
            ("NAS".to_string(), r"\\NAS".to_string()),
            ("nas".to_string(), r"\\nas".to_string()),
            ("Attic".to_string(), r"\\ATTIC".to_string()),
            ("THIS-PC".to_string(), r"\\THIS-PC".to_string()),
        ];
        let got = only_computers(found, "this-pc");
        assert_eq!(got, vec![
            Computer { name: "Attic".into(), path: r"\\ATTIC".into() },
            Computer { name: "NAS".into(), path: r"\\NAS".into() },
        ]);
    }

    /// Against this computer's own SMB server, without a window or a real account: a sign-in as
    /// the Windows user works, and an account that doesn't exist is told apart. Run with
    /// `cargo test --lib network_ -- --ignored`.
    #[test]
    #[ignore]
    fn network_connect_for_real_on_this_computer() {
        let remote = r"\\127.0.0.1\IPC$";
        win::close(remote);
        assert_eq!(win::connect(remote, None, None), 0, "as the Windows user");
        win::close(remote);
        let code = win::connect(remote, Some("ferret-test-no-such-user"), Some("not-a-password"));
        win::close(remote);
        assert!([LOGON_FAILURE, ALREADY_CONNECTED_OTHERWISE, BAD_USERNAME].contains(&code), "got {code}");
    }
}
