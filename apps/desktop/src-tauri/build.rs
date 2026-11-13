fn main() {
    // The frontend is embedded from `../dist`, which cargo does not track on
    // its own: without this a rebuilt UI silently keeps the old assets.
    println!("cargo:rerun-if-changed=../dist");
    println!("cargo:rerun-if-changed=tauri.conf.json");
    // The Windows resource includes this manifest instead of tauri-build's
    // default one; see the file for why.
    let windows = tauri_build::WindowsAttributes::new().app_manifest(include_str!("windows-app-manifest.xml"));
    tauri_build::try_build(tauri_build::Attributes::new().windows_attributes(windows))
        .expect("failed to run tauri-build");
}
