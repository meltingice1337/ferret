//! Embeds every icon in assets/icons (Lucide's, as Ferret uses them): a new icon is a new file
//! there. On Windows it also gives both programs their icon and version details (what Explorer,
//! Task Manager and the installer show).

use std::fmt::Write;

fn main() {
    icons();
    resources();
}

fn icons() {
    let dir = std::path::Path::new("assets/icons");
    println!("cargo:rerun-if-changed=assets/icons");
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("assets/icons")
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_str()?.strip_suffix(".svg").map(str::to_string))
        .collect();
    names.sort();
    let mut out = String::from("static ICONS: &[(&str, &[u8])] = &[\n");
    for n in &names {
        let path = std::fs::canonicalize(dir.join(format!("{n}.svg"))).expect("icon path");
        writeln!(out, "    ({n:?}, include_bytes!({:?})),", path.display().to_string()).unwrap();
    }
    out.push_str("];\n");
    let dest = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("icons.rs");
    std::fs::write(dest, out).expect("writing icons.rs");
}

/// ferret.exe (this crate's `ferret-gpui` binary, renamed by the installer) and
/// ferret-indexer.exe: their icon (resource 1, which GPUI loads for its windows) and version
/// details, from the app's icons in src-tauri/icons and this crate's version. Only a release
/// (scripts/release.sh sets FERRET_RELEASE=1) gets Ferret's icon; every other build shows the
/// development one, as the Tauri app's dev builds did, so it's never mistaken for the installed app.
fn resources() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    println!("cargo:rerun-if-env-changed=FERRET_RELEASE");
    let release = std::env::var("FERRET_RELEASE").is_ok_and(|v| v == "1");
    let app_icon = if release { "icon.ico" } else { "dev.ico" };
    let dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let icons = dir.join("..").join("src-tauri").join("icons");
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let mut parts = version.split(['.', '-']).map(|p| p.parse::<u16>().unwrap_or(0));
    let (a, b, c) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    for (bin, ico, description, file) in [
        ("ferret-gpui", app_icon, "Ferret", "ferret.exe"),
        ("ferret-indexer", "indexer.ico", "Ferret search service", "ferret-indexer.exe"),
    ] {
        let ico = icons.join(ico);
        println!("cargo:rerun-if-changed={}", ico.display());
        let ico = ico.canonicalize().expect("src-tauri/icons").display().to_string().replace('\\', "/");
        let ico = ico.trim_start_matches("//?/");
        let rc = format!(
            r#"#include <winver.h>
1 ICON "{ico}"
VS_VERSION_INFO VERSIONINFO
FILEVERSION {a},{b},{c},0
PRODUCTVERSION {a},{b},{c},0
FILEOS VOS_NT_WINDOWS32
FILETYPE VFT_APP
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "meltingice1337"
      VALUE "FileDescription", "{description}"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "{bin}"
      VALUE "LegalCopyright", "Copyright (C) 2026 meltingice1337"
      VALUE "OriginalFilename", "{file}"
      VALUE "ProductName", "Ferret"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
        );
        let path = out.join(format!("{bin}.rc"));
        std::fs::write(&path, rc).expect("writing the .rc");
        embed_resource::compile_for(&path, [bin], embed_resource::NONE).manifest_optional().expect("compiling the resources (rc.exe, from the Windows SDK)");
    }
}
