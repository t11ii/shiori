#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod db;
mod fonts;
mod htmlview;
mod osthumb;
mod query;
mod scan;
#[cfg(test)]
mod tests;
mod theme;
mod thumbs;

use anyhow::Context as _;

fn db_path() -> anyhow::Result<std::path::PathBuf> {
    if let Ok(p) = std::env::var("SHIORI_DB") {
        return Ok(p.into());
    }
    let dirs = directories::ProjectDirs::from("dev", "shiori", "Shiori")
        .context("home directory not found")?;
    Ok(dirs.data_dir().join("shiori.db"))
}

/// Window icon (title bar / taskbar on Windows, Dock when run outside the .app on macOS).
/// The bundled icons (`shiori.ico` in the exe, `shiori.icns` in Shiori.app) come from
/// `assets/icon/`; see `assets/icon/build_icons.py`.
fn window_icon() -> Option<eframe::egui::IconData> {
    #[cfg(windows)]
    let png: &[u8] = include_bytes!("../assets/icon/shiori-win-256.png");
    #[cfg(not(windows))]
    let png: &[u8] = include_bytes!("../assets/icon/shiori-512.png");
    eframe::icon_data::from_png_bytes(png).ok()
}

fn main() -> anyhow::Result<()> {
    let path = db_path()?;
    let db = db::Db::open(&path)?;
    // WebView2 (HTML preview on Windows) keeps its profile next to the database
    let web_data = path.with_file_name("webview");
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title("Shiori")
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([720.0, 420.0])
        .with_drag_and_drop(true);
    if let Some(icon) = window_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "Shiori",
        options,
        Box::new(move |cc| Ok(Box::new(app::ShioriApp::new(cc, db, web_data)))),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}
