#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod db;
mod fonts;
mod osthumb;
mod query;
mod scan;
#[cfg(test)]
mod tests;
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

fn main() -> anyhow::Result<()> {
    let path = db_path()?;
    let db = db::Db::open(&path)?;
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Shiori")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([720.0, 420.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "Shiori",
        options,
        Box::new(move |cc| Ok(Box::new(app::ShioriApp::new(cc, db)))),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}
