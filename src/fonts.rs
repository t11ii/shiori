//! egui ships without CJK glyphs, so we borrow a Japanese font from the OS.

use eframe::egui::{self, FontData, FontDefinitions, FontFamily};
use skrifa::instance::{LocationRef, Size};
use skrifa::{FontRef, MetadataProvider};
use std::sync::Arc;

const CANDIDATES: &[&str] = &[
    // macOS
    "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    "/Library/Fonts/Arial Unicode.ttf",
    // Windows
    "C:\\Windows\\Fonts\\YuGothM.ttc",
    "C:\\Windows\\Fonts\\meiryo.ttc",
    "C:\\Windows\\Fonts\\msgothic.ttc",
    // Linux
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/fonts-japanese-gothic.ttf",
    "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
    "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
];

pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    if let Some(bytes) = CANDIDATES.iter().find_map(|p| std::fs::read(p).ok()) {
        add_japanese_font(&mut fonts, FontData::from_owned(bytes), cfg!(windows));
    }
    ctx.set_fonts(fonts);
}

fn add_japanese_font(fonts: &mut FontDefinitions, mut font: FontData, align_baseline: bool) {
    if align_baseline {
        let latin_baseline = fonts
            .families
            .get(&FontFamily::Proportional)
            .and_then(|family| family.first())
            .and_then(|name| fonts.font_data.get(name))
            .and_then(|primary| baseline_from_line_center(primary));
        if let (Some(latin), Some(japanese)) = (latin_baseline, baseline_from_line_center(&font)) {
            // egui centers each fallback's full line height, including its line gap.
            // Align the drawn baseline in proportion to the requested font size.
            font.tweak.y_offset_factor += (latin - japanese) / font.tweak.scale;
        }
    }

    fonts.font_data.insert("jp".to_owned(), Arc::new(font));
    for fam in [FontFamily::Proportional, FontFamily::Monospace] {
        // after the default Latin font, before the emoji fallbacks
        fonts
            .families
            .entry(fam)
            .or_default()
            .insert(1, "jp".to_owned());
    }
}

fn baseline_from_line_center(font: &FontData) -> Option<f32> {
    let face = FontRef::from_index(font.font.as_ref().as_ref(), font.index).ok()?;
    let metrics = face.metrics(Size::new(font.tweak.scale), LocationRef::default());
    let line_height = metrics.ascent - metrics.descent + metrics.leading;
    Some(metrics.ascent - 0.5 * line_height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Color32, FontId, RawInput};

    #[test]
    fn mixed_filename_glyphs_share_a_baseline() {
        let mut fonts = FontDefinitions::default();
        // Use Hack's box-drawing glyph as a fallback. Scaling it creates a
        // font-metric mismatch on every test host.
        let mut fallback = fonts.font_data["Hack"].as_ref().clone();
        fallback.tweak.scale = 0.5;

        let reference_family = FontFamily::Name("unadjusted-fallback".into());
        fonts
            .font_data
            .insert("unadjusted-fallback".to_owned(), Arc::new(fallback.clone()));
        fonts.families.insert(
            reference_family.clone(),
            vec!["unadjusted-fallback".to_owned()],
        );
        add_japanese_font(&mut fonts, fallback, true);

        let ctx = egui::Context::default();
        ctx.set_fonts(fonts);
        ctx.run_ui(RawInput::default(), |ui| {
            let size = 13.5;
            let reference = ui.painter().layout_no_wrap(
                "│".to_owned(),
                FontId::new(size, reference_family.clone()),
                Color32::WHITE,
            );
            let mixed = ui.painter().layout_no_wrap(
                "R│.txt".to_owned(),
                FontId::proportional(size),
                Color32::WHITE,
            );
            let glyphs = &mixed.rows[0].glyphs;
            let latin_baseline = glyphs[0].pos.y;
            // egui applies FontTweak to the raster offset, leaving pos.y unchanged.
            let fallback_baseline = glyphs[1].pos.y + glyphs[1].uv_rect.offset.y
                - reference.rows[0].glyphs[0].uv_rect.offset.y;
            assert!(
                (fallback_baseline - latin_baseline).abs() <= 1.0,
                "Latin baseline: {latin_baseline}, fallback baseline: {fallback_baseline}"
            );
        })
        .drop_without_applying_deltas();
    }
}
