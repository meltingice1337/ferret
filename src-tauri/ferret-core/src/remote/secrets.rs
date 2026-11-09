//! Passwords and key passphrases the user chose to remember, in Windows Credential Manager
//! (encrypted for their Windows account, as other apps keep theirs). Nothing is stored unless the
//! prompt's Remember box was ticked; a wrong one is forgotten when the server turns it down.

/// Credential Manager's name for a server password: `Ferret SSH: user@host:port`.
pub fn password_target(user: &str, host: &str, port: u16) -> String {
    format!("Ferret SSH: {user}@{host}:{port}")
}

/// … and for a key file's passphrase.
pub fn passphrase_target(key_file: &std::path::Path) -> String {
    format!("Ferret SSH key: {}", key_file.display())
}

#[cfg(windows)]
mod imp {
    use windows::Win32::Security::Credentials::{
        CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_FLAGS, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
    };
    use windows::core::{PCWSTR, PWSTR};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    pub fn get(target: &str) -> Option<String> {
        let name = wide(target);
        let mut cred: *mut CREDENTIALW = std::ptr::null_mut();
        unsafe {
            CredReadW(PCWSTR(name.as_ptr()), CRED_TYPE_GENERIC, None, &mut cred).ok()?;
            let c = &*cred;
            let bytes = std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize);
            let units: Vec<u16> = bytes.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
            let secret = String::from_utf16(&units).ok();
            CredFree(cred as *const _);
            secret
        }
    }

    pub fn set(target: &str, secret: &str) -> Result<(), String> {
        let mut name = wide(target);
        let mut user = wide("Ferret");
        let mut blob: Vec<u8> = secret.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let cred = CREDENTIALW {
            Flags: CRED_FLAGS(0),
            Type: CRED_TYPE_GENERIC,
            TargetName: PWSTR(name.as_mut_ptr()),
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            UserName: PWSTR(user.as_mut_ptr()),
            ..Default::default()
        };
        let res = unsafe { CredWriteW(&cred, 0) }.map_err(|e| e.to_string());
        blob.iter_mut().for_each(|b| *b = 0);
        res
    }

    pub fn forget(target: &str) {
        let name = wide(target);
        let _ = unsafe { CredDeleteW(PCWSTR(name.as_ptr()), CRED_TYPE_GENERIC, None) };
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn get(_target: &str) -> Option<String> {
        None
    }
    pub fn set(_target: &str, _secret: &str) -> Result<(), String> {
        Err("Remembering passwords needs Windows".into())
    }
    pub fn forget(_target: &str) {}
}

pub use imp::{forget, get, set};

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn round_trip() {
        let target = format!("Ferret SSH test {}", std::process::id());
        super::set(&target, "pässwörd 🔑").unwrap();
        assert_eq!(super::get(&target).as_deref(), Some("pässwörd 🔑"));
        super::forget(&target);
        assert_eq!(super::get(&target), None);
    }
}
