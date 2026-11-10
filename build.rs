//! Embeds the application icon into the Windows executable.
//! macOS gets its icon from the .app bundle (`[package.metadata.bundle]` in Cargo.toml).

fn main() {
    println!("cargo:rerun-if-changed=assets/icon/shiori.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon/shiori.ico");
        if let Err(e) = res.compile() {
            // Don't fail the build over a missing resource compiler; the exe just has no icon.
            println!("cargo:warning=could not embed the app icon: {e}");
        }
    }
}
