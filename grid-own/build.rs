// Windows only: puts the Zest icon into the program file, so Explorer, the desktop and a pinned taskbar button show the logo (the running window's own icons are set in main.rs).
fn main() {
    println!("cargo:rerun-if-changed=assets/zest.rc");
    println!("cargo:rerun-if-changed=assets/zest.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("assets/zest.rc", embed_resource::NONE).manifest_optional().ok();
    }
}
