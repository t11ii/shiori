//! Shiori's look: one palette per light/dark mode, the egui style built from
//! it, and a few small widgets (sidebar rows, tag chips, section headers)
//! that keep the screens consistent.

use eframe::egui::{
    self, Color32, CornerRadius, FontId, Margin, Response, RichText, Sense, Shadow, Stroke, Style,
    TextStyle, Theme, Ui, Vec2, Visuals, WidgetText,
};

#[derive(Clone, Copy)]
pub struct Palette {
    /// Main content background.
    pub bg: Color32,
    /// Sidebars, toolbar and status bar.
    pub panel: Color32,
    /// Cards, popups, dialogs.
    pub raised: Color32,
    /// Text fields.
    pub input: Color32,
    /// Buttons at rest / hovered / pressed.
    pub control: Color32,
    pub control_hover: Color32,
    pub control_active: Color32,
    /// Row hover in lists and the sidebar.
    pub hover: Color32,
    pub stripe: Color32,
    pub border: Color32,
    pub border_strong: Color32,
    pub text: Color32,
    pub text_strong: Color32,
    pub text_weak: Color32,
    pub accent: Color32,
    /// Accent-coloured text on `accent_soft`.
    pub accent_text: Color32,
    /// Selection background (opaque, so it can be painted under anything).
    pub accent_soft: Color32,
    pub on_accent: Color32,
    pub star: Color32,
    pub danger: Color32,
    pub dark: bool,
}

pub const DARK: Palette = Palette {
    bg: Color32::from_rgb(0x18, 0x19, 0x1d),
    panel: Color32::from_rgb(0x1f, 0x20, 0x25),
    raised: Color32::from_rgb(0x27, 0x28, 0x2f),
    input: Color32::from_rgb(0x2a, 0x2b, 0x32),
    control: Color32::from_rgb(0x2c, 0x2d, 0x35),
    control_hover: Color32::from_rgb(0x36, 0x38, 0x41),
    control_active: Color32::from_rgb(0x40, 0x42, 0x4c),
    hover: Color32::from_rgb(0x2a, 0x2b, 0x32),
    stripe: Color32::from_rgb(0x1f, 0x20, 0x25),
    border: Color32::from_rgb(0x30, 0x32, 0x3a),
    border_strong: Color32::from_rgb(0x46, 0x48, 0x53),
    text: Color32::from_rgb(0xd9, 0xdb, 0xe1),
    text_strong: Color32::from_rgb(0xf3, 0xf4, 0xf7),
    text_weak: Color32::from_rgb(0x8d, 0x90, 0x9b),
    accent: Color32::from_rgb(0x7c, 0x8c, 0xff),
    accent_text: Color32::from_rgb(0xb4, 0xbe, 0xff),
    accent_soft: Color32::from_rgb(0x2b, 0x31, 0x5c),
    on_accent: Color32::from_rgb(0x10, 0x12, 0x2a),
    star: Color32::from_rgb(0xf5, 0xb8, 0x3d),
    danger: Color32::from_rgb(0xf0, 0x6a, 0x6a),
    dark: true,
};

pub const LIGHT: Palette = Palette {
    bg: Color32::from_rgb(0xff, 0xff, 0xff),
    panel: Color32::from_rgb(0xf5, 0xf6, 0xf8),
    raised: Color32::from_rgb(0xff, 0xff, 0xff),
    input: Color32::from_rgb(0xee, 0xef, 0xf2),
    control: Color32::from_rgb(0xea, 0xeb, 0xef),
    control_hover: Color32::from_rgb(0xdf, 0xe1, 0xe6),
    control_active: Color32::from_rgb(0xd3, 0xd6, 0xdc),
    hover: Color32::from_rgb(0xec, 0xed, 0xf1),
    stripe: Color32::from_rgb(0xf4, 0xf5, 0xf7),
    border: Color32::from_rgb(0xe3, 0xe5, 0xea),
    border_strong: Color32::from_rgb(0xc9, 0xcc, 0xd4),
    text: Color32::from_rgb(0x2a, 0x2c, 0x33),
    text_strong: Color32::from_rgb(0x12, 0x13, 0x18),
    text_weak: Color32::from_rgb(0x6e, 0x72, 0x7e),
    accent: Color32::from_rgb(0x4f, 0x5f, 0xe8),
    accent_text: Color32::from_rgb(0x3a, 0x48, 0xc4),
    accent_soft: Color32::from_rgb(0xe3, 0xe7, 0xfd),
    on_accent: Color32::WHITE,
    star: Color32::from_rgb(0xe8, 0x9e, 0x12),
    danger: Color32::from_rgb(0xd6, 0x45, 0x45),
    dark: false,
};

/// The palette for whatever theme `ui` is currently drawn in.
pub fn pal(ui: &Ui) -> &'static Palette {
    if ui.visuals().dark_mode {
        &DARK
    } else {
        &LIGHT
    }
}

pub fn install(ctx: &egui::Context) {
    ctx.set_style_of(Theme::Dark, style_for(&DARK));
    ctx.set_style_of(Theme::Light, style_for(&LIGHT));
}

fn style_for(p: &Palette) -> Style {
    let mut s = Style {
        visuals: if p.dark {
            Visuals::dark()
        } else {
            Visuals::light()
        },
        ..Style::default()
    };

    use egui::FontFamily::{Monospace, Proportional};
    s.text_styles = [
        (TextStyle::Heading, FontId::new(18.0, Proportional)),
        (TextStyle::Body, FontId::new(13.5, Proportional)),
        (TextStyle::Button, FontId::new(13.5, Proportional)),
        (TextStyle::Small, FontId::new(11.0, Proportional)),
        (TextStyle::Monospace, FontId::new(12.5, Monospace)),
    ]
    .into();

    let sp = &mut s.spacing;
    sp.item_spacing = Vec2::new(8.0, 6.0);
    sp.button_padding = Vec2::new(10.0, 4.0);
    sp.interact_size = Vec2::new(24.0, 26.0);
    sp.menu_margin = Margin::same(6);
    sp.window_margin = Margin::same(16);
    sp.indent = 14.0;
    sp.combo_width = 90.0;
    sp.icon_width = 15.0;
    sp.icon_width_inner = 9.0;

    let v = &mut s.visuals;
    let r6 = CornerRadius::same(6);
    v.panel_fill = p.bg;
    v.window_fill = p.raised;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(8);
    let shadow_alpha = if p.dark { 110 } else { 40 };
    v.window_shadow = Shadow {
        offset: [0, 8],
        blur: 28,
        spread: 0,
        color: Color32::from_black_alpha(shadow_alpha),
    };
    v.popup_shadow = Shadow {
        offset: [0, 4],
        blur: 14,
        spread: 0,
        color: Color32::from_black_alpha(shadow_alpha),
    };
    v.extreme_bg_color = p.input;
    v.text_edit_bg_color = Some(p.input);
    v.faint_bg_color = p.stripe;
    v.code_bg_color = p.input;
    v.hyperlink_color = p.accent_text;
    v.selection.bg_fill = p.accent_soft;
    v.selection.stroke = Stroke::new(1.0, p.accent_text);
    v.warn_fg_color = p.star;
    v.error_fg_color = p.danger;
    v.striped = false;
    v.indent_has_left_vline = false;
    v.collapsing_header_frame = false;
    v.slider_trailing_fill = true;

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.bg;
    w.noninteractive.weak_bg_fill = p.bg;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = r6;

    w.inactive.bg_fill = p.control;
    w.inactive.weak_bg_fill = p.control;
    w.inactive.bg_stroke = Stroke::NONE;
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = r6;

    w.hovered.bg_fill = p.control_hover;
    w.hovered.weak_bg_fill = p.control_hover;
    w.hovered.bg_stroke = Stroke::new(1.0, p.border_strong);
    w.hovered.fg_stroke = Stroke::new(1.5, p.text_strong);
    w.hovered.corner_radius = r6;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = p.control_active;
    w.active.weak_bg_fill = p.control_active;
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.active.fg_stroke = Stroke::new(1.5, p.text_strong);
    w.active.corner_radius = r6;
    w.active.expansion = 0.0;

    w.open.bg_fill = p.control_hover;
    // also the title bar of the active dialog: keep it flush with the dialog body
    w.open.weak_bg_fill = p.raised;
    w.open.bg_stroke = Stroke::new(1.0, p.border_strong);
    w.open.fg_stroke = Stroke::new(1.0, p.text_strong);
    w.open.corner_radius = r6;
    s
}

// ---------------------------------------------------------------- frames

pub fn panel_frame(p: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(p.panel)
        .inner_margin(Margin::symmetric(12, 10))
}

pub fn card_frame(p: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(p.raised)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(10))
}

// ---------------------------------------------------------------- widgets

/// A full-width sidebar row: icon, label, optional count badge on the right.
pub fn nav_item(
    ui: &mut Ui,
    icon: &str,
    label: &str,
    count: Option<i64>,
    selected: bool,
) -> Response {
    let p = pal(ui);
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 26.0), Sense::click());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, 6.0, p.accent_soft);
    } else if resp.hovered() {
        painter.rect_filled(rect, 6.0, p.hover);
    }
    let fg = if selected { p.accent_text } else { p.text };
    let icon_fg = if selected { p.accent_text } else { p.text_weak };
    let cy = rect.center().y;
    let mut x = rect.left() + 8.0;
    if !icon.is_empty() {
        painter.text(
            egui::pos2(x + 8.0, cy),
            egui::Align2::CENTER_CENTER,
            icon,
            FontId::proportional(13.0),
            icon_fg,
        );
        x += 24.0;
    }
    let mut right = rect.right() - 8.0;
    if let Some(n) = count {
        let g = painter.layout_no_wrap(n.to_string(), FontId::proportional(11.0), p.text_weak);
        right -= g.size().x;
        painter.galley(egui::pos2(right, cy - g.size().y / 2.0), g, p.text_weak);
        right -= 8.0;
    }
    let galley = WidgetText::from(RichText::new(label).color(fg)).into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        (right - x).max(10.0),
        TextStyle::Body,
    );
    painter.galley(egui::pos2(x, cy - galley.size().y / 2.0), galley, fg);
    resp
}

/// Small, muted section title with an optional area for buttons on the right.
pub fn section(ui: &mut Ui, title: &str, right: impl FnOnce(&mut Ui)) {
    let p = pal(ui);
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.label(RichText::new(title).size(11.0).color(p.text_weak).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), right);
    });
    ui.add_space(2.0);
}

/// A borderless icon button (toolbar arrows, "+" in section headers, ...).
pub fn icon_button(ui: &mut Ui, icon: &str, hover: &str) -> Response {
    ui.add(egui::Button::new(RichText::new(icon).size(14.0)).frame_when_inactive(false))
        .on_hover_text(hover)
}

pub fn primary_button(ui: &mut Ui, text: &str) -> Response {
    let p = pal(ui);
    ui.add(
        egui::Button::new(RichText::new(text).color(p.on_accent).strong())
            .fill(p.accent)
            .corner_radius(6),
    )
}

pub fn danger_button(ui: &mut Ui, text: &str) -> Response {
    let p = pal(ui);
    ui.add(
        egui::Button::new(RichText::new(text).color(Color32::WHITE).strong())
            .fill(p.danger)
            .corner_radius(6),
    )
}

/// Field caption in the detail panel.
pub fn caption(ui: &mut Ui, text: &str) {
    let p = pal(ui);
    ui.add_space(8.0);
    ui.label(RichText::new(text).size(11.0).color(p.text_weak).strong());
}

/// Stable, pleasant colours for a tag name: (background, foreground).
fn tag_hue(name: &str) -> f32 {
    let mut h: u32 = 2166136261;
    for b in name.to_lowercase().bytes() {
        h = (h ^ b as u32).wrapping_mul(16777619);
    }
    (h % 360) as f32
}

/// The solid colour used for a tag's dot in the sidebar.
pub fn tag_dot(p: &Palette, name: &str) -> Color32 {
    let hue = tag_hue(name);
    if p.dark {
        hsl(hue, 0.70, 0.68)
    } else {
        hsl(hue, 0.62, 0.52)
    }
}

pub fn tag_colors(p: &Palette, name: &str) -> (Color32, Color32) {
    let hue = tag_hue(name);
    if p.dark {
        (hsl(hue, 0.30, 0.24), hsl(hue, 0.75, 0.80))
    } else {
        (hsl(hue, 0.70, 0.93), hsl(hue, 0.55, 0.33))
    }
}

pub fn chip_size(ui: &Ui, text: &str, size: f32) -> Vec2 {
    let g =
        ui.painter()
            .layout_no_wrap(text.to_string(), FontId::proportional(size), Color32::WHITE);
    Vec2::new(g.size().x + size * 1.1, size + 7.0)
}

/// Paints a pill-shaped tag at `rect`.
pub fn paint_chip(ui: &Ui, rect: egui::Rect, text: &str, size: f32, colors: (Color32, Color32)) {
    let painter = ui.painter();
    painter.rect_filled(rect, rect.height() / 2.0, colors.0);
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        FontId::proportional(size),
        colors.1,
    );
}

/// A tag chip laid out as a widget.
pub fn tag_chip(ui: &mut Ui, tag: &str, suffix: &str, size: f32, sense: Sense) -> Response {
    let p = pal(ui);
    let text = format!("{tag}{suffix}");
    let (rect, resp) = ui.allocate_exact_size(chip_size(ui, &text, size), sense);
    let mut colors = tag_colors(p, tag);
    if resp.hovered() && sense != Sense::hover() {
        colors.0 = colors.0.gamma_multiply(if p.dark { 1.35 } else { 0.92 });
    }
    paint_chip(ui, rect, &text, size, colors);
    resp
}

/// Lays chips out left to right inside `rect`, ending with "+N" when they don't fit.
pub fn paint_chip_row(ui: &Ui, rect: egui::Rect, tags: &[String], size: f32, centered: bool) {
    let p = pal(ui);
    let gap = 4.0;
    let sizes: Vec<Vec2> = tags.iter().map(|t| chip_size(ui, t, size)).collect();
    let mut n = tags.len();
    let total = |n: usize, extra: Option<Vec2>| -> f32 {
        sizes[..n].iter().map(|s| s.x + gap).sum::<f32>() + extra.map(|e| e.x).unwrap_or(-gap)
    };
    let mut more = None;
    while n > 0 && total(n, more) > rect.width() {
        n -= 1;
        more = Some(chip_size(ui, &format!("+{}", tags.len() - n), size));
    }
    let w = total(n, more).max(0.0);
    let mut x = if centered {
        rect.center().x - w / 2.0
    } else {
        rect.left()
    };
    for (t, s) in tags[..n].iter().zip(&sizes) {
        let r = egui::Rect::from_min_size(egui::pos2(x, rect.center().y - s.y / 2.0), *s);
        paint_chip(ui, r, t, size, tag_colors(p, t));
        x += s.x + gap;
    }
    if let Some(s) = more {
        let r = egui::Rect::from_min_size(egui::pos2(x, rect.center().y - s.y / 2.0), s);
        paint_chip(
            ui,
            r,
            &format!("+{}", tags.len() - n),
            size,
            (p.control, p.text_weak),
        );
    }
}

/// The bookmark-ribbon mark next to the app name.
pub fn paint_logo(ui: &mut Ui, size: f32) {
    let p = pal(ui);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(size * 0.75, size), Sense::hover());
    let r = rect.shrink2(Vec2::new(size * 0.1, size * 0.05));
    let notch = egui::pos2(r.center().x, r.bottom() - r.height() * 0.28);
    ui.painter().add(egui::Shape::convex_polygon(
        vec![
            r.left_top(),
            r.right_top(),
            r.right_bottom(),
            notch,
            r.left_bottom(),
        ],
        p.accent,
        Stroke::NONE,
    ));
}

fn hsl(h: f32, s: f32, l: f32) -> Color32 {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let f = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(f(r), f(g), f(b))
}

/// A row of dialog buttons, right-aligned, only as tall as the buttons.
pub fn button_row<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let size = Vec2::new(ui.available_width(), ui.spacing().interact_size.y);
    ui.allocate_ui_with_layout(size, egui::Layout::right_to_left(egui::Align::Center), add)
        .inner
}
