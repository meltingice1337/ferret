//! Release signatures for the indexer service.
//!
//! The service runs as SYSTEM from %PROGRAMDATA%\Ferret\bin, which only administrators can change.
//! Ferret's own folder can be changed by the user's account (it's a per-user install), so a
//! `ferret-indexer.exe` found there is only installed over the service when it's signed with
//! Ferret's release key (the updater's: tauri.conf.json), and that's checked by the installed copy
//! (`ferret-indexer --update`). A copy swapped into Ferret's folder can't become the service that
//! way. The first install has nothing to check it against: it trusts the copy it's given.
//!
//! Each release signs `ferret-indexer.exe` (scripts/sign-indexer.mjs, as `tauri signer sign`
//! does the installer); the signature ships next to it as `ferret-indexer.exe.sig`.

/// Ferret's release key, as tauri.conf.json has it (base64 of a minisign public key; build.rs).
pub const RELEASE_KEY: &str = env!("FERRET_RELEASE_KEY");

/// Checks `sig` (a `.sig` from `tauri signer sign`: base64 of a minisign signature) of `data`
/// against `key`; returns when it was signed (Unix seconds, from the signed comment).
pub fn verify(data: &[u8], sig: &str, key: &str) -> Result<u64, String> {
    use base64::Engine;
    let text = |s: &str| {
        base64::engine::general_purpose::STANDARD.decode(s.trim()).ok().and_then(|b| String::from_utf8(b).ok())
    };
    let key = text(key).and_then(|k| minisign_verify::PublicKey::decode(&k).ok()).ok_or("the release key can't be read")?;
    let sig = text(sig).and_then(|s| minisign_verify::Signature::decode(&s).ok()).ok_or("it isn't signed")?;
    key.verify(data, &sig, true).map_err(|_| "it isn't signed with Ferret's release key".to_string())?;
    sig.trusted_comment()
        .split('\t')
        .find_map(|p| p.strip_prefix("timestamp:"))
        .and_then(|t| t.trim().parse().ok())
        .ok_or_else(|| "its signature has no time".to_string())
}

/// May `new` (its bytes and signature) replace the installed copy (`old`, with its signature if
/// it has one)? Only when signed with `key`, and not before the installed one was: an older signed
/// release would bring its old bugs back.
pub fn may_replace(new: (&[u8], &str), old: Option<(&[u8], &str)>, key: &str) -> Result<(), String> {
    let signed = verify(new.0, new.1, key)?;
    // An installed copy without a (valid) signature: a first install, or a development build.
    if let Some(was) = old.and_then(|(data, sig)| verify(data, sig, key).ok()) {
        if signed < was {
            return Err(format!("it's older than the installed one (signed {signed}, installed {was})"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // A throwaway key (not Ferret's), and two files signed with it, `newer` 11 s after `data`.
    const KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDBBN0E5QzBDOUVBODJBMTQKUldRVUtxaWVESng2Q3BiRStuS2MzUkVzTG0ycWNuQ1BMcWxZT2JFWnNMOXhkSm5MYmIzNklYREEK";
    const OTHER_KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEY5NEFDRDMwQ0YwNTlCMjMKUldRam13WFBNTTFLK2ZraTNIeWhmNHFIWWxBVFJNLzBWbHdEc2JGYXlTTy9BTlpvaGdPdHRJQ28K";
    const DATA: &[u8] = b"ferret signed test\n";
    const DATA_SIG: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVRVUtxaWVESng2Q3BaOVFyRFpXaUVkYTBiZHFvWVNmdVYxbnFubUUzV3NuQjRlbWdTQ1NGRFJXbjhjS2g5Z2ZxNUhRYnVOc3ZDMEVhaXUzY0JVNVJNRkMxcVVtTFNKbUE0PQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzkzMDIwNjYyCWZpbGU6ZGF0YS5iaW4KREJ0RDBkalkxczNCTXplbUd5TU11ZlViVDQxR1hrTTF4cVNaVFVXNnJjbzZkeTJRdm5LODIwR1hmOGVSR1Y0VlV5Zm85NVdhQllsdTNrRUhVRlZPREE9PQo=";
    const NEWER: &[u8] = b"ferret signed test, newer\n";
    const NEWER_SIG: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVRVUtxaWVESng2Q28xTDMwUkR1Tzh6WGZFNnAwNnRPTVpVN1ArQlExRWhhaVAxWXNtRWFQemxiL2dXbWhWRGxGUFRyaVhLc2JGSUFHRWtJTzlXSE1UWjBaeDRrTW84ZGcwPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzkzMDIwNjczCWZpbGU6bmV3ZXIuYmluCjRMQzN0Q2xFQW9LNk1uRnlqOVRkbSswc0duc0NDbmZkWEEyZGYwS2F3QmZxZk5RSkRTUVl0aHJpS1lmVDFUMmZ0M3ZUekRrdmpNVHYvYTFxUG9jZER3PT0K";

    #[test]
    fn only_files_signed_with_the_key_pass() {
        assert_eq!(verify(DATA, DATA_SIG, KEY), Ok(1_793_020_662));
        assert!(verify(b"ferret signed test!\n", DATA_SIG, KEY).is_err(), "changed");
        assert!(verify(NEWER, DATA_SIG, KEY).is_err(), "another file's signature");
        assert!(verify(DATA, DATA_SIG, OTHER_KEY).is_err(), "another key");
        assert!(verify(DATA, "", KEY).is_err(), "unsigned (a development build's empty .sig)");
        assert!(verify(DATA, "not base64!", KEY).is_err());
        assert!(verify(DATA, DATA_SIG, "").is_err(), "no key");
    }

    #[test]
    fn a_signed_copy_replaces_an_older_one_only() {
        assert_eq!(may_replace((NEWER, NEWER_SIG), Some((DATA, DATA_SIG)), KEY), Ok(()));
        assert_eq!(may_replace((DATA, DATA_SIG), Some((DATA, DATA_SIG)), KEY), Ok(()), "the same release again");
        assert!(may_replace((DATA, DATA_SIG), Some((NEWER, NEWER_SIG)), KEY).is_err(), "older");
        // Nothing signed installed (first install, development build): any signed copy.
        assert_eq!(may_replace((DATA, DATA_SIG), None, KEY), Ok(()));
        assert_eq!(may_replace((DATA, DATA_SIG), Some((b"dev build", "")), KEY), Ok(()));
        assert!(may_replace((b"swapped in", ""), None, KEY).is_err(), "unsigned");
        assert!(may_replace((b"swapped in", DATA_SIG), Some((DATA, DATA_SIG)), KEY).is_err(), "someone else's signature");
    }

    #[test]
    fn the_release_key_is_built_in() {
        assert!(RELEASE_KEY.len() > 40);
        use base64::Engine;
        let text = String::from_utf8(base64::engine::general_purpose::STANDARD.decode(RELEASE_KEY).unwrap()).unwrap();
        assert!(minisign_verify::PublicKey::decode(&text).is_ok(), "{text}");
    }
}
