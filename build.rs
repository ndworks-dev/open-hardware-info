// Embeds app.manifest: layered child windows need the Windows 8+ compatibility
// entry, and per-monitor DPI awareness must match the taskbar we attach to.
fn main() {
    println!("cargo:rerun-if-changed=app.manifest");
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{dir}\\app.manifest");

    println!("cargo:rerun-if-changed=assets/icon.ico");
    embed_resource::compile("assets/app.rc", embed_resource::NONE).manifest_optional().unwrap();

    let style = slint_build::CompilerConfiguration::new().with_style("fluent".into());
    slint_build::compile_with_config("ui/settings.slint", style).unwrap();
}
