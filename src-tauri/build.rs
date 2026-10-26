fn main() {
    indexer_icon();
    wsl_helper();
    release_key();
    tauri_build::build()
}

/// The release key (the updater's, tauri.conf.json) for search/signed.rs, and a place for the
/// indexer's signature: scripts/sign-indexer.mjs writes it before bundling, and the installer
/// carries it (bundle.resources). Until then, and in builds that aren't signed, it's empty.
fn release_key() {
    let dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let conf = dir.join("tauri.conf.json");
    println!("cargo:rerun-if-changed={}", conf.display());
    let conf: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&conf).unwrap()).unwrap();
    let key = conf["plugins"]["updater"]["pubkey"].as_str().expect("tauri.conf.json: plugins.updater.pubkey");
    println!("cargo:rustc-env=FERRET_RELEASE_KEY={key}");
    let sig = dir.join("target").join("signed").join("ferret-indexer.exe.sig");
    if !sig.exists() {
        std::fs::create_dir_all(sig.parent().unwrap()).unwrap();
        std::fs::write(&sig, "").unwrap();
    }
}

/// The Linux program that counts folder sizes in WSL (wsl-size/), built for Linux and embedded
/// in the app (foldersize.rs writes it out and runs it). Without Rust's musl target a debug
/// build goes without it (WSL counts with find then); a release needs it.
fn wsl_helper() {
    use std::path::PathBuf;
    use std::process::Command;
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let src = dir.join("wsl-size");
    for p in ["src", "Cargo.toml", "Cargo.lock", ".cargo/config.toml"] {
        println!("cargo:rerun-if-changed={}", src.join(p).display());
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("ferret-wsl-size");
    let target_dir = dir.join("target").join("wsl-size");
    let mut cargo = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    // Its own settings (target, linker, flags: wsl-size/.cargo/config.toml), not the app's.
    // (Its output to stderr: what a build script prints on stdout is read as instructions.)
    cargo.current_dir(&src).args(["build", "--release", "--locked", "--target-dir"]).arg(&target_dir);
    cargo.stdout(std::io::stderr());
    for v in ["CARGO_TARGET_DIR", "CARGO_BUILD_TARGET", "CARGO_ENCODED_RUSTFLAGS", "RUSTFLAGS", "RUSTC_WORKSPACE_WRAPPER"] {
        cargo.env_remove(v);
    }
    let built = target_dir.join("x86_64-unknown-linux-musl").join("release").join("ferret-wsl-size");
    let ok = cargo.status().is_ok_and(|s| s.success()) && std::fs::copy(&built, &out).is_ok();
    if !ok {
        let fix = "the WSL folder-size helper didn't build (`rustup target add x86_64-unknown-linux-musl`)";
        if std::env::var("PROFILE").as_deref() == Ok("release") {
            panic!("{fix}");
        }
        println!("cargo:warning={fix}: WSL folders are counted with find instead");
        std::fs::write(&out, []).unwrap();
    }
    // Its fingerprint: Linux checks a copy against it before running it.
    use sha2::Digest;
    let hash = sha2::Sha256::digest(std::fs::read(&out).unwrap());
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    println!("cargo:rustc-env=FERRET_WSL_HELPER_SHA256={hex}");
}

/// ferret-indexer.exe gets its own icon. Explorer and Task Manager show a program's
/// lowest-numbered icon group, so group 1 wins over the app icon (32512) both binaries
/// carry. The .res is written by hand: `rc` would number the icon images 1.., which
/// collide with the app icon's images.
fn indexer_icon() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let ico_path = dir.join("icons").join("indexer.ico");
    println!("cargo:rerun-if-changed={}", ico_path.display());
    let ico = std::fs::read(&ico_path).expect("icons/indexer.ico");
    let res = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("indexer.res");
    std::fs::write(&res, icon_res(&ico, 1, 101)).unwrap();
    println!("cargo:rustc-link-arg-bin=ferret-indexer={}", res.display());
}

/// A .res file holding one icon group (`group_id`) whose images get ids from `first_image_id`.
fn icon_res(ico: &[u8], group_id: u16, first_image_id: u16) -> Vec<u8> {
    const RT_ICON: u16 = 3;
    const RT_GROUP_ICON: u16 = 14;
    let u16_at = |i: usize| u16::from_le_bytes([ico[i], ico[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes([ico[i], ico[i + 1], ico[i + 2], ico[i + 3]]);

    fn entry(out: &mut Vec<u8>, kind: u16, name: u16, flags: u16, data: &[u8]) {
        out.extend((data.len() as u32).to_le_bytes()); // DataSize
        out.extend(32u32.to_le_bytes()); // HeaderSize
        out.extend([0xFF, 0xFF]);
        out.extend(kind.to_le_bytes());
        out.extend([0xFF, 0xFF]);
        out.extend(name.to_le_bytes());
        out.extend(0u32.to_le_bytes()); // DataVersion
        out.extend(flags.to_le_bytes()); // MemoryFlags
        out.extend(0x0409u16.to_le_bytes()); // LanguageId (en-US)
        out.extend(0u32.to_le_bytes()); // Version
        out.extend(0u32.to_le_bytes()); // Characteristics
        out.extend(data);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }

    let mut out = Vec::new();
    entry(&mut out, 0, 0, 0, &[]); // a .res starts with an empty entry
    let count = u16_at(4);
    let mut group = Vec::new();
    group.extend(0u16.to_le_bytes());
    group.extend(1u16.to_le_bytes());
    group.extend(count.to_le_bytes());
    for i in 0..count as usize {
        let e = 6 + i * 16;
        let (size, offset) = (u32_at(e + 8), u32_at(e + 12) as usize);
        let id = first_image_id + i as u16;
        entry(&mut out, RT_ICON, id, 0x1010, &ico[offset..offset + size as usize]);
        group.extend(&ico[e..e + 8]); // width, height, colors, reserved, planes, bit count
        group.extend(size.to_le_bytes());
        group.extend(id.to_le_bytes());
    }
    entry(&mut out, RT_GROUP_ICON, group_id, 0x1030, &group);
    out
}
