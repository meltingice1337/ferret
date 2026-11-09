fn main() {
    wsl_helper();
    release_key();
}

/// src-tauri, the app's folder: the helper's sources and build folder (target/wsl-size) and the
/// release key live there.
fn app_dir() -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    dir.parent().expect("ferret-core sits in src-tauri").to_path_buf()
}

/// The release key (the updater's, in the app's tauri.conf.json) for search/signed.rs.
fn release_key() {
    let dir = app_dir();
    let conf = dir.join("tauri.conf.json");
    println!("cargo:rerun-if-changed={}", conf.display());
    let conf: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&conf).unwrap()).unwrap();
    let key = conf["plugins"]["updater"]["pubkey"].as_str().expect("tauri.conf.json: plugins.updater.pubkey");
    println!("cargo:rustc-env=FERRET_RELEASE_KEY={key}");
}

/// The Linux program that counts folder sizes in WSL and on SSH servers (src-tauri/wsl-size/), built for
/// x86_64 and ARM64 Linux and embedded in the app (linux.rs puts it there and runs it). Without
/// Rust's musl targets a debug build goes without it (folders are counted with find then); a
/// release needs both.
fn wsl_helper() {
    use std::path::PathBuf;
    use std::process::Command;
    let dir = app_dir();
    let src = dir.join("wsl-size");
    for p in ["src", "Cargo.toml", "Cargo.lock", ".cargo/config.toml"] {
        println!("cargo:rerun-if-changed={}", src.join(p).display());
    }
    let target_dir = dir.join("target").join("wsl-size");
    for (arch, target) in [("X64", "x86_64-unknown-linux-musl"), ("ARM64", "aarch64-unknown-linux-musl")] {
        let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join(format!("ferret-wsl-size-{target}"));
        let mut cargo = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
        // Its own settings (linker, flags: wsl-size/.cargo/config.toml), not the app's.
        // (Its output to stderr: what a build script prints on stdout is read as instructions.)
        cargo.current_dir(&src).args(["build", "--release", "--locked", "--target", target, "--target-dir"]).arg(&target_dir);
        cargo.stdout(std::io::stderr());
        for v in ["CARGO_TARGET_DIR", "CARGO_BUILD_TARGET", "CARGO_ENCODED_RUSTFLAGS", "RUSTFLAGS", "RUSTC_WORKSPACE_WRAPPER"] {
            cargo.env_remove(v);
        }
        let built = target_dir.join(target).join("release").join("ferret-wsl-size");
        let ok = cargo.status().is_ok_and(|s| s.success()) && std::fs::copy(&built, &out).is_ok();
        if !ok {
            let fix = format!("the Linux folder-size helper didn't build for {target} (`rustup target add {target}`)");
            if std::env::var("PROFILE").as_deref() == Ok("release") {
                panic!("{fix}");
            }
            println!("cargo:warning={fix}: those folders are counted with find instead");
            std::fs::write(&out, []).unwrap();
        }
        // Its fingerprint: Linux checks a copy against it before running it.
        use sha2::Digest;
        let hash = sha2::Sha256::digest(std::fs::read(&out).unwrap());
        let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
        println!("cargo:rustc-env=FERRET_HELPER_{arch}_SHA256={hex}");
    }
}
