// Embeds app.manifest: layered child windows need the Windows 8+ compatibility
// entry, and per-monitor DPI awareness must match the taskbar we attach to.
fn main() {
    println!("cargo:rerun-if-changed=app.manifest");
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{dir}\\app.manifest");
}
