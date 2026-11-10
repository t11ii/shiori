//! The egui user interface.

use crate::db::{Db, FileRow, Profile, Scope, SortKey, LABELS};
use crate::htmlview::{self, HtmlView};
use crate::query::Filter;
use crate::scan::{spawn_scan, ScanMsg};
use crate::theme;
use crate::thumbs::{has_thumbnail, is_image, Thumbs};
use eframe::egui::{self, Color32, Key, Margin, Modifiers, RichText, Sense, Stroke, Vec2};
use egui_extras::{Column, TableBuilder};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

const RESULT_LIMIT: usize = 50_000;

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum ViewMode {
    List,
    Grid,
}

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum ThemeChoice {
    System,
    Light,
    Dark,
}

impl ThemeChoice {
    fn preference(self) -> egui::ThemePreference {
        match self {
            ThemeChoice::System => egui::ThemePreference::System,
            ThemeChoice::Light => egui::ThemePreference::Light,
            ThemeChoice::Dark => egui::ThemePreference::Dark,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Settings {
    view: ViewMode,
    sort: SortKey,
    desc: bool,
    thumb_size: f32,
    recursive: bool,
    theme: ThemeChoice,
    /// Let previewed HTML pages run their scripts.
    html_scripts: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            view: ViewMode::List,
            sort: SortKey::Name,
            desc: false,
            thumb_size: 140.0,
            recursive: false,
            theme: ThemeChoice::System,
            html_scripts: true,
        }
    }
}

/// Things the user did this frame. Collected while drawing, applied afterwards,
/// so that drawing code only needs shared borrows.
enum Action {
    Click(usize, Modifiers),
    Open(usize),
    EnterDir(String),
    Reveal(String),
    StartRename(i64),
    AskTrash(Vec<i64>),
    Forget(Vec<i64>),
    AddToProfile(i64, Vec<i64>),
    RemoveFromProfile(i64, Vec<i64>),
    SetStar(Vec<i64>, u8),
    SetLabel(Vec<i64>, u8),
    AddTag(Vec<i64>, String),
    RemoveTag(Vec<i64>, String),
    SetScope(Scope),
    SetSearch(String),
    Rescan(Vec<String>),
    RemoveRoot(String),
    AddRoot,
    EditProfile(Option<i64>, bool),
    DeleteProfile(i64),
    RenameTag(String),
    DeleteTag(String),
    SelectAll,
}

struct ProfileEdit {
    id: Option<i64>,
    name: String,
    smart: bool,
    query: String,
}

struct TagEdit {
    old: String,
    new: String,
}

pub struct ShioriApp {
    db: Db,
    thumbs: Thumbs,
    settings: Settings,

    scan_tx: Sender<ScanMsg>,
    scan_rx: Receiver<ScanMsg>,
    scanning: HashSet<String>,
    scan_progress: HashMap<String, usize>,

    roots: Vec<String>,
    profiles: Vec<(Profile, i64)>,
    tags: Vec<(String, i64)>,

    scope: Scope,
    back: Vec<Scope>,
    search: String,
    rows: Vec<FileRow>,
    dirty: bool,
    sidebar_dirty: bool,
    query_ms: f64,

    selected: HashSet<i64>,
    anchor: Option<usize>,
    scroll_to: Option<usize>,

    detail_for: Option<i64>,
    comment_buf: String,
    tag_input: String,
    rename: Option<(i64, String)>,
    profile_edit: Option<ProfileEdit>,
    tag_edit: Option<TagEdit>,
    confirm_trash: Option<Vec<i64>>,
    toast: Option<(String, Instant)>,

    htmlview: HtmlView,
    /// The HTML file the detail panel wants shown this frame, and where (in points).
    html_want: Option<(PathBuf, egui::Rect)>,
}

impl ShioriApp {
    pub fn new(cc: &eframe::CreationContext<'_>, db: Db, web_data: PathBuf) -> Self {
        crate::fonts::install(&cc.egui_ctx);
        let settings: Settings = cc
            .storage
            .and_then(|s| eframe::get_value(s, "shiori-settings"))
            .unwrap_or_default();
        theme::install(&cc.egui_ctx);
        cc.egui_ctx.set_theme(settings.theme.preference());
        let (scan_tx, scan_rx) = channel();
        let mut app = Self {
            thumbs: Thumbs::new(&cc.egui_ctx),
            db,
            settings,
            scan_tx,
            scan_rx,
            scanning: HashSet::new(),
            scan_progress: HashMap::new(),
            roots: vec![],
            profiles: vec![],
            tags: vec![],
            scope: Scope::All,
            back: vec![],
            search: String::new(),
            rows: vec![],
            dirty: true,
            sidebar_dirty: true,
            query_ms: 0.0,
            selected: HashSet::new(),
            anchor: None,
            scroll_to: None,
            detail_for: None,
            comment_buf: String::new(),
            tag_input: String::new(),
            rename: None,
            profile_edit: None,
            tag_edit: None,
            confirm_trash: None,
            toast: None,
            htmlview: HtmlView::new(web_data),
            html_want: None,
        };
        app.refresh_sidebar();
        if let Some(first) = app.roots.first().cloned() {
            app.scope = Scope::Folder {
                path: first,
                recursive: app.settings.recursive,
            };
        }
        // refresh the index in the background on startup
        let roots = app.roots.clone();
        app.start_scan(&cc.egui_ctx, roots);
        app
    }

    fn notify(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    fn report<T>(&mut self, r: anyhow::Result<T>) -> Option<T> {
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                self.notify(format!("Error: {e:#}"));
                None
            }
        }
    }

    fn start_scan(&mut self, ctx: &egui::Context, roots: Vec<String>) {
        let roots: Vec<String> = roots
            .into_iter()
            .filter(|r| !self.scanning.contains(r))
            .collect();
        if roots.is_empty() {
            return;
        }
        self.scanning.extend(roots.iter().cloned());
        spawn_scan(
            self.db.path.clone(),
            roots,
            self.scan_tx.clone(),
            ctx.clone(),
        );
    }

    fn refresh_sidebar(&mut self) {
        self.roots = self.db.roots().unwrap_or_default();
        let profiles = self.db.profiles().unwrap_or_default();
        self.profiles = profiles
            .into_iter()
            .map(|p| {
                let n = if p.query.is_some() {
                    -1
                } else {
                    self.db.profile_count(p.id).unwrap_or(0)
                };
                (p, n)
            })
            .collect();
        self.tags = self.db.tag_counts().unwrap_or_default();
        self.sidebar_dirty = false;
    }

    fn requery(&mut self) {
        let t = Instant::now();
        let filter = Filter::parse(&self.search);
        // searching inside a folder looks through its subfolders too
        let scope = match &self.scope {
            Scope::Folder {
                path,
                recursive: false,
            } if !filter.is_empty() => Scope::Folder {
                path: path.clone(),
                recursive: true,
            },
            s => s.clone(),
        };
        let r = self.db.search(
            &scope,
            &filter,
            self.settings.sort,
            self.settings.desc,
            RESULT_LIMIT,
        );
        if let Some(rows) = self.report(r) {
            self.rows = rows;
        }
        let present: HashSet<i64> = self.rows.iter().map(|r| r.id).collect();
        self.selected.retain(|id| present.contains(id));
        self.anchor = self.anchor.filter(|a| *a < self.rows.len());
        self.query_ms = t.elapsed().as_secs_f64() * 1000.0;
        self.dirty = false;
    }

    fn set_scope(&mut self, scope: Scope) {
        if scope != self.scope {
            self.back.push(std::mem::replace(&mut self.scope, scope));
            if self.back.len() > 100 {
                self.back.remove(0);
            }
            self.selected.clear();
            self.anchor = None;
            self.dirty = true;
        }
    }

    fn selected_ids(&self) -> Vec<i64> {
        self.rows
            .iter()
            .filter(|r| self.selected.contains(&r.id))
            .map(|r| r.id)
            .collect()
    }

    fn selected_rows(&self) -> Vec<&FileRow> {
        self.rows
            .iter()
            .filter(|r| self.selected.contains(&r.id))
            .collect()
    }

    fn open_row(&mut self, idx: usize) {
        let Some(row) = self.rows.get(idx).cloned() else {
            return;
        };
        if row.is_dir {
            self.set_scope(Scope::Folder {
                path: row.path,
                recursive: false,
            });
        } else if let Err(e) = open::that_detached(&row.path) {
            self.notify(format!("Could not open: {e}"));
        }
    }

    fn apply(&mut self, ctx: &egui::Context, action: Action) {
        let mut changed = true;
        match action {
            Action::Click(idx, m) => {
                changed = false;
                let Some(id) = self.rows.get(idx).map(|r| r.id) else {
                    return;
                };
                if m.shift {
                    let a = self.anchor.unwrap_or(idx);
                    if !m.command {
                        self.selected.clear();
                    }
                    for r in &self.rows[a.min(idx)..=a.max(idx)] {
                        self.selected.insert(r.id);
                    }
                } else if m.command {
                    if !self.selected.remove(&id) {
                        self.selected.insert(id);
                    }
                    self.anchor = Some(idx);
                } else {
                    self.selected.clear();
                    self.selected.insert(id);
                    self.anchor = Some(idx);
                }
            }
            Action::Open(idx) => {
                changed = false;
                self.open_row(idx);
            }
            Action::EnterDir(path) => {
                changed = false;
                self.set_scope(Scope::Folder {
                    path,
                    recursive: false,
                });
            }
            Action::Reveal(path) => {
                changed = false;
                reveal(&path);
            }
            Action::StartRename(id) => {
                changed = false;
                if let Some(r) = self.rows.iter().find(|r| r.id == id) {
                    self.rename = Some((id, r.name.clone()));
                }
            }
            Action::AskTrash(ids) => {
                changed = false;
                if !ids.is_empty() {
                    self.confirm_trash = Some(ids);
                }
            }
            Action::Forget(ids) => {
                let r = self.db.forget_ids(&ids);
                self.report(r);
            }
            Action::AddToProfile(pid, ids) => {
                let r = self.db.add_to_profile(pid, &ids);
                if self.report(r).is_some() {
                    let name = self
                        .profiles
                        .iter()
                        .find(|p| p.0.id == pid)
                        .map(|p| p.0.name.clone())
                        .unwrap_or_default();
                    self.notify(format!("Added {} item(s) to \"{}\"", ids.len(), name));
                }
            }
            Action::RemoveFromProfile(pid, ids) => {
                let r = self.db.remove_from_profile(pid, &ids);
                self.report(r);
            }
            Action::SetStar(ids, n) => {
                let r = self.db.set_star(&ids, n);
                self.report(r);
            }
            Action::SetLabel(ids, l) => {
                let r = self.db.set_label(&ids, l);
                self.report(r);
            }
            Action::AddTag(ids, t) => {
                let r = self.db.add_tag(&ids, &t);
                self.report(r);
            }
            Action::RemoveTag(ids, t) => {
                let r = self.db.remove_tag(&ids, &t);
                self.report(r);
            }
            Action::SetScope(s) => {
                changed = false;
                self.set_scope(s);
            }
            Action::SetSearch(s) => {
                changed = false;
                self.search = s;
                self.dirty = true;
            }
            Action::Rescan(roots) => {
                changed = false;
                self.start_scan(ctx, roots);
            }
            Action::RemoveRoot(p) => {
                let r = self.db.remove_root(&p);
                if let Some((removed, kept)) = self.report(r) {
                    let mut m = format!(
                        "Removed folder from Shiori ({removed} items cleared from the list)"
                    );
                    if kept > 0 {
                        m = format!("{m}; {kept} items with tags/stars/comments kept");
                    }
                    self.notify(m);
                }
                if matches!(&self.scope, Scope::Folder { path, .. } if path.starts_with(&p)) {
                    self.set_scope(Scope::All);
                }
            }
            Action::AddRoot => {
                changed = false;
                if let Some(dir) = rfd::FileDialog::new()
                    .set_title("Choose a folder to add")
                    .pick_folder()
                {
                    self.add_root(ctx, &dir);
                }
            }
            Action::EditProfile(id, smart) => {
                changed = false;
                self.profile_edit = Some(
                    match id.and_then(|id| self.profiles.iter().find(|p| p.0.id == id)) {
                        Some((p, _)) => ProfileEdit {
                            id: Some(p.id),
                            name: p.name.clone(),
                            smart: p.query.is_some(),
                            query: p.query.clone().unwrap_or_default(),
                        },
                        None => ProfileEdit {
                            id: None,
                            name: String::new(),
                            smart,
                            query: if smart {
                                self.search.clone()
                            } else {
                                String::new()
                            },
                        },
                    },
                );
            }
            Action::DeleteProfile(id) => {
                let r = self.db.delete_profile(id);
                self.report(r);
                if self.scope == Scope::Profile(id) {
                    self.set_scope(Scope::All);
                }
            }
            Action::RenameTag(t) => {
                changed = false;
                self.tag_edit = Some(TagEdit {
                    new: t.clone(),
                    old: t,
                });
            }
            Action::DeleteTag(t) => {
                let r = self.db.delete_tag(&t);
                self.report(r);
            }
            Action::SelectAll => {
                changed = false;
                self.selected = self.rows.iter().map(|r| r.id).collect();
            }
        }
        if changed {
            self.dirty = true;
            self.sidebar_dirty = true;
            self.detail_for = None; // reload the detail editors
        }
    }

    fn add_root(&mut self, ctx: &egui::Context, dir: &Path) {
        let r = self.db.add_root(dir);
        if self.report(r).is_some() {
            let key = crate::db::path_key(dir);
            self.sidebar_dirty = true;
            self.start_scan(ctx, vec![key.clone()]);
            self.set_scope(Scope::Folder {
                path: key,
                recursive: self.settings.recursive,
            });
        }
    }

    // ------------------------------------------------------------------ input

    fn handle_keys(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if ctx.egui_wants_keyboard_input() || self.rename.is_some() || self.profile_edit.is_some() {
            return;
        }
        let ids = self.selected_ids();
        ctx.input_mut(|i| {
            if i.consume_key(Modifiers::COMMAND, Key::A) {
                actions.push(Action::SelectAll);
            }
            if i.consume_key(Modifiers::COMMAND, Key::F) {
                // focus handled below
            }
            if !ids.is_empty() {
                let stars = [
                    Key::Num0,
                    Key::Num1,
                    Key::Num2,
                    Key::Num3,
                    Key::Num4,
                    Key::Num5,
                ];
                for (n, k) in stars.iter().enumerate() {
                    if i.consume_key(Modifiers::NONE, *k) {
                        actions.push(Action::SetStar(ids.clone(), n as u8));
                    }
                }
                if i.consume_key(Modifiers::NONE, Key::Delete)
                    || i.consume_key(Modifiers::COMMAND, Key::Backspace)
                {
                    actions.push(Action::AskTrash(ids.clone()));
                }
                if i.consume_key(Modifiers::NONE, Key::F2) && ids.len() == 1 {
                    actions.push(Action::StartRename(ids[0]));
                }
                if i.consume_key(Modifiers::NONE, Key::Enter) {
                    if let Some(idx) = self.rows.iter().position(|r| r.id == ids[0]) {
                        actions.push(Action::Open(idx));
                    }
                }
            }
            let down = i.consume_key(Modifiers::NONE, Key::ArrowDown)
                || i.consume_key(Modifiers::NONE, Key::ArrowRight);
            let up = i.consume_key(Modifiers::NONE, Key::ArrowUp)
                || i.consume_key(Modifiers::NONE, Key::ArrowLeft);
            if (down || up) && !self.rows.is_empty() {
                let cur = self.anchor.unwrap_or(0);
                let next = if down {
                    (cur + 1).min(self.rows.len() - 1)
                } else {
                    cur.saturating_sub(1)
                };
                actions.push(Action::Click(next, Modifiers::NONE));
                self.scroll_to = Some(next);
            }
            if i.consume_key(Modifiers::NONE, Key::Backspace)
                || i.consume_key(Modifiers::ALT, Key::ArrowUp)
            {
                if let Scope::Folder { path, .. } = &self.scope {
                    if !self.roots.contains(path) {
                        if let Some(parent) = Path::new(path).parent() {
                            actions.push(Action::EnterDir(crate::db::path_key(parent)));
                        }
                    }
                }
            }
        });
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if dropped.is_empty() {
            return;
        }
        let mut ids = vec![];
        for p in &dropped {
            if p.is_dir() && !matches!(self.scope, Scope::Profile(_)) {
                self.add_root(ctx, p);
            } else if let Some(id) = self.report(self.db.register_path(p)) {
                ids.push(id);
            }
        }
        if let Scope::Profile(pid) = self.scope {
            let r = self.db.add_to_profile(pid, &ids);
            self.report(r);
            self.notify(format!("Added {} item(s) to the virtual folder", ids.len()));
        }
        self.dirty = true;
        self.sidebar_dirty = true;
    }

    // ------------------------------------------------------------------ panels

    fn sidebar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let p = theme::pal(ui);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            theme::paint_logo(ui, 20.0);
            ui.label(
                RichText::new("Shiori")
                    .size(17.0)
                    .strong()
                    .color(p.text_strong),
            );
        });
        ui.add_space(6.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                theme::section(ui, "LIBRARY", |_| {});
                let quick = [
                    ("📚", "All Files", ""),
                    ("⭐", "Starred", "star:1"),
                    ("🏷", "Untagged", "is:untagged is:file"),
                    ("💬", "With Comments", "is:comment"),
                    ("⚠", "Missing Files", "is:missing"),
                ];
                for (icon, label, q) in quick {
                    let active = self.scope == Scope::All && self.search == q;
                    if theme::nav_item(ui, icon, label, None, active).clicked() {
                        actions.push(Action::SetScope(Scope::All));
                        actions.push(Action::SetSearch(q.to_string()));
                    }
                }

                theme::section(ui, "FOLDERS", |ui| {
                    if theme::icon_button(ui, "+", "Add a folder").clicked() {
                        actions.push(Action::AddRoot);
                    }
                });
                if self.roots.is_empty() {
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new("No folders yet")
                                .size(12.0)
                                .color(p.text_weak),
                        );
                    });
                }
                for root in &self.roots {
                    let name = Path::new(root)
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| root.clone());
                    let active = matches!(&self.scope, Scope::Folder { path, .. } if path == root);
                    let icon = if self.scanning.contains(root) {
                        "⟳"
                    } else {
                        "🗀"
                    };
                    let count = self.scan_progress.get(root).map(|n| *n as i64);
                    let resp = theme::nav_item(ui, icon, &name, count, active).on_hover_text(root);
                    if resp.clicked() {
                        actions.push(Action::SetScope(Scope::Folder {
                            path: root.clone(),
                            recursive: self.settings.recursive,
                        }));
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Rescan").clicked() {
                            actions.push(Action::Rescan(vec![root.clone()]));
                            ui.close();
                        }
                        if ui.button(reveal_label()).clicked() {
                            actions.push(Action::Reveal(root.clone()));
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Remove from Shiori").clicked() {
                            actions.push(Action::RemoveRoot(root.clone()));
                            ui.close();
                        }
                    });
                }

                theme::section(ui, "VIRTUAL FOLDERS", |ui| {
                    if theme::icon_button(ui, "+", "New virtual folder").clicked() {
                        actions.push(Action::EditProfile(None, false));
                    }
                });
                if self.profiles.is_empty() {
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new("Collect files from anywhere")
                                .size(12.0)
                                .color(p.text_weak),
                        );
                    });
                }
                for (prof, n) in &self.profiles {
                    let active = self.scope == Scope::Profile(prof.id);
                    let (icon, count) = if prof.query.is_some() {
                        ("🔎", None)
                    } else {
                        ("🔖", Some(*n))
                    };
                    let resp = theme::nav_item(ui, icon, &prof.name, count, active);
                    if resp.clicked() {
                        actions.push(Action::SetScope(Scope::Profile(prof.id)));
                    }
                    if prof.query.is_none() {
                        if let Some(ids) = resp.dnd_release_payload::<Vec<i64>>() {
                            actions.push(Action::AddToProfile(prof.id, (*ids).clone()));
                        }
                        if resp.dnd_hover_payload::<Vec<i64>>().is_some() {
                            drop_highlight(ui, &resp);
                        }
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Edit…").clicked() {
                            actions.push(Action::EditProfile(Some(prof.id), false));
                            ui.close();
                        }
                        if ui.button("Delete").clicked() {
                            actions.push(Action::DeleteProfile(prof.id));
                            ui.close();
                        }
                    });
                }

                theme::section(ui, "TAGS", |_| {});
                if self.tags.is_empty() {
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new("Add tags from the panel on the right")
                                .size(12.0)
                                .color(p.text_weak),
                        );
                    });
                }
                let current = Filter::parse(&self.search);
                for (t, n) in &self.tags {
                    let active = current.tags.iter().any(|x| x.eq_ignore_ascii_case(t));
                    let resp = theme::nav_item(ui, "#", t, Some(*n), active);
                    // colour swatch matching the tag's chip colour
                    let dot = egui::pos2(resp.rect.left() + 16.0, resp.rect.center().y);
                    ui.painter().circle_filled(dot, 4.5, theme::tag_dot(p, t));
                    if resp.clicked() {
                        let token = tag_token(t);
                        let cmd = ui.input(|i| i.modifiers.command);
                        let q = if cmd {
                            if active {
                                self.search.replace(&token, "").trim().to_string()
                            } else {
                                format!("{} {}", self.search.trim(), token)
                                    .trim()
                                    .to_string()
                            }
                        } else if active {
                            String::new()
                        } else {
                            token
                        };
                        if !cmd {
                            actions.push(Action::SetScope(Scope::All));
                        }
                        actions.push(Action::SetSearch(q));
                    }
                    if let Some(ids) = resp.dnd_release_payload::<Vec<i64>>() {
                        actions.push(Action::AddTag((*ids).clone(), t.clone()));
                    }
                    if resp.dnd_hover_payload::<Vec<i64>>().is_some() {
                        drop_highlight(ui, &resp);
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Rename…").clicked() {
                            actions.push(Action::RenameTag(t.clone()));
                            ui.close();
                        }
                        if ui.button("Delete Tag").clicked() {
                            actions.push(Action::DeleteTag(t.clone()));
                            ui.close();
                        }
                    });
                }

                theme::section(ui, "LABELS", |_| {});
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(6.0);
                    ui.spacing_mut().item_spacing.x = 2.0;
                    for (i, (en, _, c)) in LABELS.iter().enumerate().skip(1) {
                        let (rect, resp) =
                            ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
                        let active = self.search == format!("label:{en}");
                        if active || resp.hovered() {
                            ui.painter().circle_filled(rect.center(), 10.0, p.hover);
                        }
                        ui.painter().circle_filled(rect.center(), 6.5, rgb(*c));
                        if active {
                            ui.painter().circle_stroke(
                                rect.center(),
                                9.5,
                                Stroke::new(1.5, p.text_strong),
                            );
                        }
                        let resp = resp.on_hover_text(label_name(i));
                        if resp.clicked() {
                            actions.push(Action::SetScope(Scope::All));
                            actions.push(Action::SetSearch(if active {
                                String::new()
                            } else {
                                format!("label:{en}")
                            }));
                        }
                        if let Some(ids) = resp.dnd_release_payload::<Vec<i64>>() {
                            actions.push(Action::SetLabel((*ids).clone(), i as u8));
                        }
                    }
                });
                ui.add_space(14.0);
                egui::Frame::new()
                    .inner_margin(Margin::symmetric(8, 0))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(
                                "Tip: drag files onto a tag, virtual folder or label to apply it",
                            )
                            .size(11.0)
                            .color(p.text_weak),
                        );
                    });
            });
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let p = theme::pal(ui);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let back = ui
                .add_enabled_ui(!self.back.is_empty(), |ui| {
                    theme::icon_button(ui, "⏴", "Back")
                })
                .inner;
            if back.clicked() {
                if let Some(s) = self.back.pop() {
                    self.scope = s;
                    self.dirty = true;
                }
            }
            let folder = if let Scope::Folder { path, recursive } = &self.scope {
                Some((path.clone(), *recursive))
            } else {
                None
            };
            let at_root = folder
                .as_ref()
                .is_none_or(|(path, _)| self.roots.contains(path));
            let up = ui
                .add_enabled_ui(!at_root, |ui| theme::icon_button(ui, "⏶", "Up one folder"))
                .inner;
            if up.clicked() {
                if let Some(parent) = folder
                    .as_ref()
                    .and_then(|(path, _)| Path::new(path).parent())
                {
                    actions.push(Action::EnterDir(crate::db::path_key(parent)));
                }
            }
            ui.add_space(8.0);
            ui.spacing_mut().item_spacing.x = 4.0;
            match &folder {
                Some((path, _)) => self.breadcrumb(ui, path, actions),
                None => {
                    let title = match &self.scope {
                        Scope::Profile(id) => self
                            .profiles
                            .iter()
                            .find(|p| p.0.id == *id)
                            .map(|p| p.0.name.clone())
                            .unwrap_or_default(),
                        _ => "All Files".to_string(),
                    };
                    ui.label(
                        RichText::new(title)
                            .size(16.0)
                            .strong()
                            .color(p.text_strong),
                    );
                }
            }
            if let Some((path, recursive)) = folder {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut rec = recursive;
                    if ui.checkbox(&mut rec, "Include subfolders").changed() {
                        self.settings.recursive = rec;
                        self.scope = Scope::Folder {
                            path,
                            recursive: rec,
                        };
                        self.dirty = true;
                    }
                });
            }
        });
        ui.add_space(6.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if theme::icon_button(ui, "⟳", "Rescan all folders").clicked() {
                actions.push(Action::Rescan(self.roots.clone()));
            }
            if self.settings.view == ViewMode::Grid {
                ui.spacing_mut().slider_width = 80.0;
                ui.add(
                    egui::Slider::new(&mut self.settings.thumb_size, 64.0..=256.0)
                        .show_value(false),
                )
                .on_hover_text("Thumbnail size");
            }
            // segmented view switch
            egui::Frame::new()
                .fill(p.control)
                .corner_radius(7)
                .inner_margin(Margin::same(2))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    ui.spacing_mut().button_padding = Vec2::new(8.0, 2.0);
                    for (mode, icon, tip) in [
                        (ViewMode::Grid, "⊞", "Thumbnail view"),
                        (ViewMode::List, "☰", "List view"),
                    ] {
                        let on = self.settings.view == mode;
                        let b = egui::Button::new(RichText::new(icon).size(14.0).color(if on {
                            p.text_strong
                        } else {
                            p.text_weak
                        }))
                        .fill(if on { p.raised } else { Color32::TRANSPARENT })
                        .corner_radius(5);
                        if ui.add(b).on_hover_text(tip).clicked() {
                            self.settings.view = mode;
                        }
                    }
                });
            ui.add_space(4.0);
            let old = (self.settings.sort, self.settings.desc);
            if theme::icon_button(
                ui,
                if self.settings.desc { "↓" } else { "↑" },
                "Ascending / descending",
            )
            .clicked()
            {
                self.settings.desc = !self.settings.desc;
            }
            egui::ComboBox::from_id_salt("sort")
                .width(96.0)
                .selected_text(sort_name(self.settings.sort))
                .show_ui(ui, |ui| {
                    for k in [
                        SortKey::Name,
                        SortKey::Modified,
                        SortKey::Size,
                        SortKey::Star,
                    ] {
                        ui.selectable_value(&mut self.settings.sort, k, sort_name(k));
                    }
                });
            if old != (self.settings.sort, self.settings.desc) {
                self.dirty = true;
            }
            ui.add_space(8.0);
            if !self.search.is_empty() && theme::icon_button(ui, "×", "Clear search").clicked() {
                self.search.clear();
                self.dirty = true;
            }
            let search_id = egui::Id::unique("shiori-search");
            let resp = ui.add_sized(
                [ui.available_width(), 30.0],
                egui::TextEdit::singleline(&mut self.search)
                    .id(search_id)
                    .hint_text("🔍  Search   trip #family star:3 ext:pdf label:red")
                    .margin(Margin::symmetric(10, 5)),
            );
            if ui.input(|i| i.modifiers.command && i.key_pressed(Key::F)) {
                resp.request_focus();
            }
            if resp.changed() {
                self.dirty = true;
            }
        });
    }

    fn breadcrumb(&self, ui: &mut egui::Ui, path: &str, actions: &mut Vec<Action>) {
        let p = theme::pal(ui);
        let root = self
            .roots
            .iter()
            .filter(|r| path == r.as_str() || path.starts_with(&crate::db::subtree_range(r).0))
            .max_by_key(|r| r.len());
        let Some(root) = root else {
            ui.label(RichText::new(path).size(16.0).strong());
            return;
        };
        let mut cur = PathBuf::from(root);
        let mut crumbs = vec![(
            cur.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| root.clone()),
            root.clone(),
        )];
        let rest = Path::new(path)
            .strip_prefix(root)
            .ok()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        for comp in rest.components() {
            cur.push(comp);
            crumbs.push((
                comp.as_os_str().to_string_lossy().to_string(),
                crate::db::path_key(&cur),
            ));
        }
        let last = crumbs.len() - 1;
        for (i, (name, target)) in crumbs.into_iter().enumerate() {
            if i > 0 {
                ui.label(RichText::new("›").size(15.0).color(p.text_weak));
            }
            if i == last {
                ui.label(RichText::new(name).size(16.0).strong().color(p.text_strong));
            } else {
                let r = ui.add(
                    egui::Label::new(RichText::new(name).size(16.0).color(p.text_weak))
                        .sense(Sense::click()),
                );
                if r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    actions.push(Action::EnterDir(target));
                }
            }
        }
    }

    fn row_context_menu(
        &self,
        ui: &mut egui::Ui,
        row: &FileRow,
        ids: &[i64],
        actions: &mut Vec<Action>,
    ) {
        if ui.button("Open").clicked() {
            if let Some(idx) = self.rows.iter().position(|r| r.id == row.id) {
                actions.push(Action::Open(idx));
            }
            ui.close();
        }
        if ui.button(reveal_label()).clicked() {
            actions.push(Action::Reveal(row.path.clone()));
            ui.close();
        }
        if ui.button("Copy Path").clicked() {
            ui.ctx().copy_text(row.path.clone());
            ui.close();
        }
        ui.separator();
        ui.menu_button("Star", |ui| {
            for n in 0..=5u8 {
                if ui
                    .button(if n == 0 {
                        "None".to_string()
                    } else {
                        "★".repeat(n as usize)
                    })
                    .clicked()
                {
                    actions.push(Action::SetStar(ids.to_vec(), n));
                    ui.close();
                }
            }
        });
        ui.menu_button("Label", |ui| {
            for (i, (_, _, c)) in LABELS.iter().enumerate() {
                let ja = label_name(i);
                let txt = if i == 0 {
                    RichText::new(ja)
                } else {
                    RichText::new(format!("● {ja}")).color(rgb(*c))
                };
                if ui.button(txt).clicked() {
                    actions.push(Action::SetLabel(ids.to_vec(), i as u8));
                    ui.close();
                }
            }
        });
        let manual: Vec<&Profile> = self
            .profiles
            .iter()
            .map(|p| &p.0)
            .filter(|p| p.query.is_none())
            .collect();
        if !manual.is_empty() {
            ui.menu_button("Add to Virtual Folder", |ui| {
                for p in manual {
                    if ui.button(&p.name).clicked() {
                        actions.push(Action::AddToProfile(p.id, ids.to_vec()));
                        ui.close();
                    }
                }
            });
        }
        if let Scope::Profile(pid) = self.scope {
            if self
                .profiles
                .iter()
                .any(|p| p.0.id == pid && p.0.query.is_none())
                && ui.button("Remove from This Virtual Folder").clicked()
            {
                actions.push(Action::RemoveFromProfile(pid, ids.to_vec()));
                ui.close();
            }
        }
        ui.separator();
        if ids.len() == 1 && ui.button("Rename…  (F2)").clicked() {
            actions.push(Action::StartRename(row.id));
            ui.close();
        }
        if ui.button("Move to Trash…").clicked() {
            actions.push(Action::AskTrash(ids.to_vec()));
            ui.close();
        }
        if ui
            .button("Remove from Shiori (keep file on disk)")
            .clicked()
        {
            actions.push(Action::Forget(ids.to_vec()));
            ui.close();
        }
    }

    fn list_view(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let p = theme::pal(ui);
        let row_h = 30.0;
        // Column dividers only as a faint hint; they light up when hovered for resizing.
        ui.visuals_mut().widgets.noninteractive.bg_stroke.color = p.border.gamma_multiply(0.45);
        let mut table = TableBuilder::new(ui)
            .striped(false)
            .sense(Sense::click_and_drag())
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(34.0))
            .column(Column::initial(220.0).at_least(80.0).clip(true))
            .column(Column::initial(76.0).at_least(30.0).clip(true))
            .column(Column::initial(140.0).at_least(40.0).clip(true))
            .column(Column::initial(72.0).at_least(40.0).clip(true))
            .column(Column::initial(128.0).at_least(60.0).clip(true))
            .column(Column::remainder().clip(true))
            .auto_shrink([false, false]);
        if let Some(i) = self.scroll_to.take() {
            table = table.scroll_to_row(i, None);
        }
        let rows = &self.rows;
        let selected = &self.selected;
        let thumbs = &mut self.thumbs;
        let show_path = !matches!(
            self.scope,
            Scope::Folder {
                recursive: false,
                ..
            }
        );
        let mut menu: Option<(usize, Vec<i64>, egui::Response)> = None;
        table
            .header(26.0, |mut h| {
                for t in [
                    "",
                    "NAME",
                    "RATING",
                    "TAGS",
                    "SIZE",
                    "MODIFIED",
                    if show_path { "LOCATION" } else { "COMMENT" },
                ] {
                    h.col(|ui| {
                        ui.label(RichText::new(t).size(11.0).strong().color(p.text_weak));
                    });
                }
            })
            .body(|body| {
                body.rows(row_h, rows.len(), |mut row| {
                    let idx = row.index();
                    let f = &rows[idx];
                    row.set_selected(selected.contains(&f.id));
                    row.col(|ui| {
                        let tex = if !f.is_dir && has_thumbnail(&f.ext) {
                            thumbs.get(f.id, &f.path, 64)
                        } else {
                            None
                        };
                        let (rect, _) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::hover());
                        if let Some(tex) = tex {
                            let s = fit(tex.size_vec2(), Vec2::splat(24.0));
                            egui::Image::new((tex.id(), s))
                                .corner_radius(4)
                                .paint_at(ui, egui::Rect::from_center_size(rect.center(), s));
                        } else {
                            ui.painter().text(
                                rect.center(),
                                egui::Align2::CENTER_CENTER,
                                file_icon(f),
                                egui::FontId::proportional(15.0),
                                if f.is_dir { p.accent_text } else { p.text_weak },
                            );
                        }
                    });
                    row.col(|ui| {
                        if f.label > 0 {
                            let (r, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                            ui.painter().circle_filled(
                                r.center(),
                                4.5,
                                rgb(LABELS[f.label as usize].2),
                            );
                        }
                        let mut t = RichText::new(&f.name).color(p.text_strong);
                        if f.missing {
                            t = t.strikethrough().color(p.text_weak);
                        }
                        ui.add(egui::Label::new(t).truncate().selectable(false));
                    });
                    row.col(|ui| {
                        if f.star > 0 {
                            ui.label(
                                RichText::new("★".repeat(f.star as usize))
                                    .size(12.0)
                                    .color(p.star),
                            );
                        }
                    });
                    row.col(|ui| {
                        if !f.tags.is_empty() {
                            let rect = ui.available_rect_before_wrap();
                            theme::paint_chip_row(ui, rect, &f.tags, 11.0, false);
                        }
                    });
                    row.col(|ui| {
                        if !f.is_dir {
                            ui.label(RichText::new(human_size(f.size)).color(p.text_weak));
                        }
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(fmt_time(f.mtime)).color(p.text_weak));
                    });
                    row.col(|ui| {
                        let t = if show_path {
                            Path::new(&f.path)
                                .parent()
                                .map(|p| p.to_string_lossy().to_string())
                                .unwrap_or_default()
                        } else {
                            f.comment.lines().next().unwrap_or("").to_string()
                        };
                        ui.add(
                            egui::Label::new(RichText::new(t).color(p.text_weak))
                                .truncate()
                                .selectable(false),
                        );
                    });
                    let resp = row.response();
                    handle_item_response(&resp, idx, f, selected, actions);
                    if resp.secondary_clicked() || resp.context_menu_opened() {
                        let ids = if selected.contains(&f.id) {
                            rows.iter()
                                .filter(|r| selected.contains(&r.id))
                                .map(|r| r.id)
                                .collect()
                        } else {
                            vec![f.id]
                        };
                        menu = Some((idx, ids, resp));
                    }
                });
            });
        if let Some((idx, ids, resp)) = menu {
            let row = self.rows[idx].clone();
            resp.context_menu(|ui| self.row_context_menu(ui, &row, &ids, actions));
        }
    }

    fn grid_view(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let p = theme::pal(ui);
        let thumb = self.settings.thumb_size;
        let gap = 12.0;
        let pad = 8.0;
        let card_w = thumb + pad * 2.0;
        let card_h = thumb + pad + 52.0;
        let width = ui.available_width() - gap;
        let cols = (((width + gap) / (card_w + gap)).floor() as usize).max(1);
        // spread leftover width evenly so the grid fills the row
        let card_w = ((width + gap) / cols as f32 - gap).max(card_w);
        let n_rows = self.rows.len().div_ceil(cols);
        let rows = &self.rows;
        let selected = &self.selected;
        let thumbs = &mut self.thumbs;
        let mut menu: Option<(usize, Vec<i64>, egui::Response)> = None;
        let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]);
        if let Some(i) = self.scroll_to.take() {
            area = area.vertical_scroll_offset((i / cols) as f32 * (card_h + gap));
        }
        ui.spacing_mut().item_spacing = Vec2::splat(gap);
        area.show_rows(ui, card_h, n_rows, |ui, range| {
            for r in range {
                ui.horizontal(|ui| {
                    ui.add_space(gap / 2.0);
                    for c in 0..cols {
                        let idx = r * cols + c;
                        let Some(f) = rows.get(idx) else { break };
                        let (rect, resp) = ui.allocate_exact_size(
                            Vec2::new(card_w, card_h),
                            Sense::click_and_drag(),
                        );
                        if !ui.is_rect_visible(rect) {
                            continue;
                        }
                        let sel = selected.contains(&f.id);
                        let painter = ui.painter_at(rect.expand(2.0));
                        let (fill, stroke) = if sel {
                            (p.accent_soft, Stroke::new(2.0, p.accent))
                        } else if resp.hovered() {
                            (p.hover, Stroke::new(1.0, p.border_strong))
                        } else {
                            (p.raised, Stroke::new(1.0, p.border))
                        };
                        painter.rect(rect, 10.0, fill, stroke, egui::StrokeKind::Inside);

                        let img_rect = egui::Rect::from_min_size(
                            rect.min + Vec2::new(pad, pad),
                            Vec2::new(card_w - pad * 2.0, thumb),
                        );
                        let tex = if !f.is_dir && has_thumbnail(&f.ext) {
                            thumbs.get(f.id, &f.path, if thumb > 128.0 { 256 } else { 128 })
                        } else {
                            None
                        };
                        if let Some(tex) = tex {
                            let s = fit(tex.size_vec2(), img_rect.size());
                            egui::Image::new((tex.id(), s))
                                .corner_radius(6)
                                .paint_at(ui, egui::Rect::from_center_size(img_rect.center(), s));
                        } else {
                            painter.rect_filled(img_rect, 6.0, if p.dark { p.bg } else { p.panel });
                            painter.text(
                                img_rect.center(),
                                egui::Align2::CENTER_CENTER,
                                file_icon(f),
                                egui::FontId::proportional(thumb * 0.36),
                                if f.is_dir { p.accent_text } else { p.text_weak },
                            );
                            if !f.is_dir && !f.ext.is_empty() {
                                painter.text(
                                    img_rect.center_bottom() + Vec2::new(0.0, -10.0),
                                    egui::Align2::CENTER_BOTTOM,
                                    f.ext.to_uppercase(),
                                    egui::FontId::proportional(10.0),
                                    p.text_weak,
                                );
                            }
                        }
                        // badges over the thumbnail
                        if f.star > 0 {
                            let stars = "★".repeat(f.star as usize);
                            let g = painter.layout_no_wrap(
                                stars,
                                egui::FontId::proportional(10.5),
                                p.star,
                            );
                            let bg = egui::Rect::from_min_size(
                                img_rect.left_bottom() + Vec2::new(5.0, -g.size().y - 9.0),
                                g.size() + Vec2::new(10.0, 4.0),
                            );
                            painter.rect_filled(
                                bg,
                                bg.height() / 2.0,
                                Color32::from_black_alpha(150),
                            );
                            painter.galley(bg.min + Vec2::new(5.0, 2.0), g, p.star);
                        }
                        if f.label > 0 {
                            let c = img_rect.right_top() + Vec2::new(-10.0, 10.0);
                            painter.circle_filled(c, 6.5, Color32::from_black_alpha(90));
                            painter.circle_filled(c, 5.0, rgb(LABELS[f.label as usize].2));
                        }

                        let name_color = if f.missing {
                            p.text_weak
                        } else {
                            p.text_strong
                        };
                        let galley = egui::WidgetText::from(
                            RichText::new(&f.name).size(12.5).color(name_color),
                        )
                        .into_galley(
                            ui,
                            Some(egui::TextWrapMode::Truncate),
                            card_w - pad * 2.0,
                            egui::TextStyle::Body,
                        );
                        painter.galley(
                            egui::pos2(
                                rect.center().x - galley.size().x / 2.0,
                                img_rect.bottom() + 8.0,
                            ),
                            galley,
                            name_color,
                        );
                        if !f.tags.is_empty() {
                            let chips = egui::Rect::from_min_size(
                                egui::pos2(rect.left() + pad, img_rect.bottom() + 28.0),
                                Vec2::new(card_w - pad * 2.0, 18.0),
                            );
                            theme::paint_chip_row(ui, chips, &f.tags, 10.0, true);
                        }
                        let resp = resp.on_hover_text(&f.path);
                        handle_item_response(&resp, idx, f, selected, actions);
                        if resp.secondary_clicked() || resp.context_menu_opened() {
                            let ids = if sel {
                                rows.iter()
                                    .filter(|r| selected.contains(&r.id))
                                    .map(|r| r.id)
                                    .collect()
                            } else {
                                vec![f.id]
                            };
                            menu = Some((idx, ids, resp));
                        }
                    }
                });
            }
        });
        if let Some((idx, ids, resp)) = menu {
            let row = self.rows[idx].clone();
            resp.context_menu(|ui| self.row_context_menu(ui, &row, &ids, actions));
        }
    }

    fn detail_panel(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let p = theme::pal(ui);
        let sel: Vec<FileRow> = self.selected_rows().into_iter().cloned().collect();
        if sel.is_empty() {
            ui.add_space(60.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("🔖").size(36.0).color(p.border_strong));
                ui.add_space(6.0);
                ui.label(
                    RichText::new("Nothing selected")
                        .size(14.0)
                        .strong()
                        .color(p.text),
                );
                ui.label(
                    RichText::new(
                        "Select a file to see its preview\nand edit its tags, rating and notes",
                    )
                    .size(12.0)
                    .color(p.text_weak),
                );
            });
            return;
        }
        let ids: Vec<i64> = sel.iter().map(|r| r.id).collect();
        let single = sel.len() == 1;
        let first = &sel[0];

        // reload editors when the selection changes
        let key = if single {
            Some(first.id)
        } else {
            Some(-(ids.len() as i64))
        };
        if self.detail_for != key {
            self.detail_for = key;
            self.comment_buf = if single {
                first.comment.clone()
            } else {
                String::new()
            };
        }

        let panel_h = ui.available_height();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let live_html = single
                    && !first.is_dir
                    && !first.missing
                    && htmlview::supports(&first.ext)
                    && self.htmlview.available();
                if single {
                    let w = ui.available_width();
                    if live_html {
                        // The page itself is a native view laid over this area by `show_html`;
                        // what is painted here shows while a menu or dialog hides it.
                        let well_h = (w * 0.8).clamp(200.0, (panel_h * 0.55).max(200.0));
                        let (well, _) =
                            ui.allocate_exact_size(Vec2::new(w, well_h), Sense::hover());
                        ui.painter()
                            .rect_filled(well, 10.0, if p.dark { p.bg } else { p.input });
                        ui.painter().text(
                            well.center(),
                            egui::Align2::CENTER_CENTER,
                            file_icon(first),
                            egui::FontId::proportional(56.0),
                            p.text_weak,
                        );
                        let page = well.shrink(8.0).intersect(ui.clip_rect());
                        if page.width() >= 80.0 && page.height() >= 80.0 {
                            self.html_want = Some((PathBuf::from(&first.path), page));
                        }
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Live page").size(11.0).color(p.text_weak));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let on = self.settings.html_scripts;
                                    let text = if on { "Scripts: On" } else { "Scripts: Off" };
                                    let color = if on { p.accent_text } else { p.text_weak };
                                    let button = egui::Button::new(
                                        RichText::new(text).size(11.0).color(color),
                                    )
                                    .frame_when_inactive(false);
                                    if ui
                                        .add(button)
                                        .on_hover_text(
                                            "Presentations need their scripts to fit the panel \
                                             and to animate.\nTurn this off for pages you do \
                                             not trust. Either way a page cannot leave its folder.",
                                        )
                                        .clicked()
                                    {
                                        self.settings.html_scripts = !on;
                                    }
                                },
                            );
                        });
                    } else {
                        let tex = if !first.is_dir && has_thumbnail(&first.ext) {
                            self.thumbs.get(first.id, &first.path, 512)
                        } else {
                            None
                        };
                        // preview well
                        let well_h = match &tex {
                            Some(t) => fit(t.size_vec2(), Vec2::new(w - 16.0, 280.0)).y + 16.0,
                            None => 150.0,
                        };
                        let (well, _) =
                            ui.allocate_exact_size(Vec2::new(w, well_h), Sense::hover());
                        ui.painter()
                            .rect_filled(well, 10.0, if p.dark { p.bg } else { p.input });
                        if let Some(tex) = tex {
                            let s = fit(tex.size_vec2(), well.size() - Vec2::splat(16.0));
                            egui::Image::new((tex.id(), s))
                                .corner_radius(6)
                                .paint_at(ui, egui::Rect::from_center_size(well.center(), s));
                        } else {
                            ui.painter().text(
                                well.center(),
                                egui::Align2::CENTER_CENTER,
                                file_icon(first),
                                egui::FontId::proportional(56.0),
                                if first.is_dir {
                                    p.accent_text
                                } else {
                                    p.text_weak
                                },
                            );
                        }
                    }
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(&first.name)
                                    .strong()
                                    .size(15.5)
                                    .color(p.text_strong),
                            )
                            .wrap(),
                        );
                    });
                    if first.missing {
                        ui.label(RichText::new("⚠ Not found on disk").color(p.danger));
                    }
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().button_padding = Vec2::new(10.0, 3.0);
                        if theme::primary_button(ui, "Open").clicked() {
                            if let Some(idx) = self.rows.iter().position(|r| r.id == first.id) {
                                actions.push(Action::Open(idx));
                            }
                        }
                        if ui.button(reveal_label()).clicked() {
                            actions.push(Action::Reveal(first.path.clone()));
                        }
                        if ui.button("Rename…").on_hover_text("F2").clicked() {
                            actions.push(Action::StartRename(first.id));
                        }
                    });
                    ui.add_space(4.0);
                    egui::Grid::new("info")
                        .num_columns(2)
                        .spacing([12.0, 4.0])
                        .show(ui, |ui| {
                            let key = |ui: &mut egui::Ui, t: &str| {
                                ui.label(RichText::new(t).size(12.0).color(p.text_weak));
                            };
                            if !first.is_dir {
                                key(ui, "Kind");
                                ui.label(RichText::new(kind_name(first)).size(12.0));
                                ui.end_row();
                                key(ui, "Size");
                                ui.label(RichText::new(human_size(first.size)).size(12.0));
                                ui.end_row();
                            }
                            key(ui, "Modified");
                            ui.label(RichText::new(fmt_time(first.mtime)).size(12.0));
                            ui.end_row();
                            key(ui, "Where");
                            let parent = Path::new(&first.path)
                                .parent()
                                .map(|p| p.to_string_lossy().to_string())
                                .unwrap_or_default();
                            let link = egui::Label::new(
                                RichText::new(&parent).size(12.0).color(p.accent_text),
                            )
                            .truncate()
                            .sense(Sense::click());
                            if ui
                                .add(link)
                                .on_hover_text(&first.path)
                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                .clicked()
                            {
                                actions.push(Action::Reveal(first.path.clone()));
                            }
                            ui.end_row();
                        });
                } else {
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(format!("{} items selected", sel.len()))
                            .size(16.0)
                            .strong()
                            .color(p.text_strong),
                    );
                    ui.label(
                        RichText::new("Changes here apply to every selected item")
                            .size(12.0)
                            .color(p.text_weak),
                    );
                }

                ui.add_space(6.0);
                ui.separator();

                theme::caption(ui, "RATING");
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    let cur = if sel.iter().all(|r| r.star == first.star) {
                        first.star
                    } else {
                        0
                    };
                    let hover_n = ui
                        .ctx()
                        .memory(|m| m.data.get_temp::<u8>(egui::Id::unique("star-hover")))
                        .unwrap_or(0);
                    let mut now_hover = 0u8;
                    for n in 1..=5u8 {
                        let lit = if hover_n > 0 { n <= hover_n } else { n <= cur };
                        let (rect, resp) =
                            ui.allocate_exact_size(Vec2::new(26.0, 26.0), Sense::click());
                        ui.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            if lit { "★" } else { "☆" },
                            egui::FontId::proportional(20.0),
                            if lit { p.star } else { p.border_strong },
                        );
                        if resp.hovered() {
                            now_hover = n;
                        }
                        if resp
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .clicked()
                        {
                            actions
                                .push(Action::SetStar(ids.clone(), if cur == n { 0 } else { n }));
                        }
                    }
                    ui.ctx().memory_mut(|m| {
                        m.data
                            .insert_temp(egui::Id::unique("star-hover"), now_hover)
                    });
                });

                theme::caption(ui, "LABEL");
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for (i, (_, _, c)) in LABELS.iter().enumerate() {
                        let (rect, resp) =
                            ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
                        let on = sel.iter().all(|r| r.label as usize == i);
                        if resp.hovered() {
                            ui.painter().circle_filled(rect.center(), 11.5, p.hover);
                        }
                        if i == 0 {
                            ui.painter().circle_stroke(
                                rect.center(),
                                7.0,
                                Stroke::new(1.5, p.text_weak),
                            );
                            ui.painter().line_segment(
                                [
                                    rect.center() + Vec2::new(-4.5, 4.5),
                                    rect.center() + Vec2::new(4.5, -4.5),
                                ],
                                Stroke::new(1.5, p.text_weak),
                            );
                        } else {
                            ui.painter().circle_filled(rect.center(), 7.5, rgb(*c));
                        }
                        if on {
                            ui.painter().circle_stroke(
                                rect.center(),
                                10.5,
                                Stroke::new(2.0, p.text_strong),
                            );
                        }
                        if resp.on_hover_text(label_name(i)).clicked() {
                            actions.push(Action::SetLabel(ids.clone(), i as u8));
                        }
                    }
                });

                theme::caption(ui, "TAGS");
                let mut all_tags: Vec<(String, usize)> = vec![];
                for r in &sel {
                    for t in &r.tags {
                        match all_tags.iter_mut().find(|x| &x.0 == t) {
                            Some(x) => x.1 += 1,
                            None => all_tags.push((t.clone(), 1)),
                        }
                    }
                }
                if !all_tags.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(5.0, 5.0);
                        for (t, n) in &all_tags {
                            let suffix = if *n < sel.len() {
                                format!(" ({n})  ×")
                            } else {
                                "  ×".to_string()
                            };
                            if theme::tag_chip(ui, t, &suffix, 12.0, Sense::click())
                                .on_hover_text("Click to remove this tag")
                                .clicked()
                            {
                                actions.push(Action::RemoveTag(ids.clone(), t.clone()));
                            }
                        }
                    });
                    ui.add_space(2.0);
                }
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.tag_input)
                        .hint_text("Add a tag…  (Enter, comma for several)")
                        .margin(Margin::symmetric(8, 5))
                        .desired_width(f32::INFINITY),
                );
                if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    for t in self.tag_input.split([',', '、']) {
                        let t = t.trim();
                        if !t.is_empty() {
                            actions.push(Action::AddTag(ids.clone(), t.to_string()));
                        }
                    }
                    self.tag_input.clear();
                    resp.request_focus();
                }
                // suggestions from existing tags
                let q = self.tag_input.trim().trim_start_matches('#').to_lowercase();
                let suggestions: Vec<String> = self
                    .tags
                    .iter()
                    .map(|t| t.0.clone())
                    .filter(|t| !all_tags.iter().any(|a| &a.0 == t))
                    .filter(|t| q.is_empty() || t.to_lowercase().contains(&q))
                    .take(12)
                    .collect();
                if !suggestions.is_empty() {
                    ui.add_space(2.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(5.0, 5.0);
                        ui.label(RichText::new("Suggested").size(11.0).color(p.text_weak));
                        for t in suggestions {
                            if theme::tag_chip(ui, &t, "  +", 11.0, Sense::click()).clicked() {
                                actions.push(Action::AddTag(ids.clone(), t.clone()));
                                self.tag_input.clear();
                            }
                        }
                    });
                }

                theme::caption(ui, "NOTES");
                let resp = ui.add(
                    egui::TextEdit::multiline(&mut self.comment_buf)
                        .hint_text(if single {
                            "Write a note (saved automatically)"
                        } else {
                            "Typing here replaces the comment of every selected item"
                        })
                        .margin(Margin::same(8))
                        .desired_rows(4)
                        .desired_width(f32::INFINITY),
                );
                if resp.lost_focus() {
                    let changed = if single {
                        self.comment_buf != first.comment
                    } else {
                        !self.comment_buf.is_empty()
                    };
                    if changed {
                        let r = self.db.set_comment(&ids, &self.comment_buf);
                        if self.report(r).is_some() {
                            self.dirty = true;
                        }
                    }
                }

                let manual: Vec<Profile> = self
                    .profiles
                    .iter()
                    .map(|p| p.0.clone())
                    .filter(|p| p.query.is_none())
                    .collect();
                if !manual.is_empty() {
                    theme::caption(ui, "VIRTUAL FOLDERS");
                    let member: HashSet<i64> = if single {
                        self.db
                            .profiles_of(first.id)
                            .unwrap_or_default()
                            .into_iter()
                            .collect()
                    } else {
                        HashSet::new()
                    };
                    for prof in manual {
                        if single {
                            let mut on = member.contains(&prof.id);
                            if ui.checkbox(&mut on, &prof.name).changed() {
                                actions.push(if on {
                                    Action::AddToProfile(prof.id, ids.clone())
                                } else {
                                    Action::RemoveFromProfile(prof.id, ids.clone())
                                });
                            }
                        } else if ui.button(format!("+ {}", prof.name)).clicked() {
                            actions.push(Action::AddToProfile(prof.id, ids.clone()));
                        }
                    }
                }
                ui.add_space(12.0);
            });
    }

    fn dialogs(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        // rename
        if let Some((id, mut name)) = self.rename.take() {
            let mut open = true;
            let mut done = false;
            egui::Window::new("Rename")
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut name)
                            .margin(Margin::symmetric(8, 5))
                            .desired_width(340.0),
                    );
                    r.request_focus();
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    ui.add_space(6.0);
                    theme::button_row(ui, |ui| {
                        if theme::primary_button(ui, "Rename").clicked() || enter {
                            done = true;
                        }
                    });
                });
            if done {
                let r = self.db.rename(id, &name);
                if self.report(r).is_some() {
                    self.thumbs.invalidate(id);
                    self.dirty = true;
                }
            } else if open && !ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.rename = Some((id, name));
            }
        }

        // trash confirm
        if let Some(ids) = self.confirm_trash.take() {
            let mut keep = true;
            egui::Window::new("Move to Trash")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!("Move {} item(s) to the Trash?", ids.len()));
                    ui.label(
                        RichText::new("Their tags, ratings and notes in Shiori are removed too.")
                            .size(12.0)
                            .color(theme::pal(ui).text_weak),
                    );
                    ui.add_space(8.0);
                    theme::button_row(ui, |ui| {
                        if theme::danger_button(ui, "Move to Trash").clicked() {
                            let r = self.db.trash(&ids);
                            if self.report(r).is_some() {
                                self.notify(format!("Moved {} item(s) to the Trash", ids.len()));
                            }
                            self.dirty = true;
                            self.sidebar_dirty = true;
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape))
                        {
                            keep = false;
                        }
                    });
                });
            if keep {
                self.confirm_trash = Some(ids);
            }
        }

        // profile editor
        if let Some(mut pe) = self.profile_edit.take() {
            let mut keep = true;
            let title = if pe.id.is_some() {
                "Edit Virtual Folder"
            } else {
                "New Virtual Folder"
            };
            let current_search = self.search.clone();
            egui::Window::new(title)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    let p = theme::pal(ui);
                    ui.label(RichText::new("Name").size(12.0).color(p.text_weak));
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut pe.name)
                            .margin(Margin::symmetric(8, 5))
                            .desired_width(340.0),
                    );
                    if pe.name.is_empty() {
                        r.request_focus();
                    }
                    ui.add_space(6.0);
                    if ui
                        .checkbox(&mut pe.smart, "Smart folder — filled automatically by a search")
                        .changed()
                        && pe.smart
                        && pe.query.is_empty()
                    {
                        pe.query = current_search.clone();
                    }
                    if pe.smart {
                        ui.add(
                            egui::TextEdit::singleline(&mut pe.query)
                                .margin(Margin::symmetric(8, 5))
                                .desired_width(340.0)
                                .hint_text("#work star:3 ext:pdf"),
                        );
                        ui.label(
                            RichText::new("Files matching this search appear here automatically.")
                                .size(12.0)
                                .color(p.text_weak),
                        );
                    } else {
                        ui.label(
                            RichText::new("Add files by dragging them onto the folder, or right-click → Add to Virtual Folder.")
                                .size(12.0)
                                .color(p.text_weak),
                        );
                    }
                    ui.add_space(8.0);
                    theme::button_row(ui, |ui| {
                        let ok = !pe.name.trim().is_empty();
                        if ui.add_enabled_ui(ok, |ui| theme::primary_button(ui, "Save")).inner.clicked() {
                            let q = pe.smart.then_some(pe.query.as_str());
                            let r = match pe.id {
                                Some(id) => {
                                    self.db.update_profile(id, pe.name.trim(), q).map(|_| id)
                                }
                                None => self.db.create_profile(pe.name.trim(), q),
                            };
                            if let Some(id) = self.report(r) {
                                actions.push(Action::SetScope(Scope::Profile(id)));
                                if pe.smart {
                                    actions.push(Action::SetSearch(String::new()));
                                }
                            }
                            self.sidebar_dirty = true;
                            self.dirty = true;
                            keep = false;
                        }
                        if ui.button("Cancel").clicked()
                            || ui.input(|i| i.key_pressed(Key::Escape))
                        {
                            keep = false;
                        }
                    });
                });
            if keep {
                self.profile_edit = Some(pe);
            }
        }

        // tag rename
        if let Some(mut te) = self.tag_edit.take() {
            let mut keep = true;
            egui::Window::new("Rename Tag")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut te.new)
                            .margin(Margin::symmetric(8, 5))
                            .desired_width(300.0),
                    );
                    ui.weak("Using an existing tag name merges the two tags");
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    ui.add_space(6.0);
                    let clicked =
                        theme::button_row(ui, |ui| theme::primary_button(ui, "Rename").clicked());
                    if enter || clicked {
                        let res = self.db.rename_tag(&te.old, &te.new);
                        self.report(res);
                        self.dirty = true;
                        self.sidebar_dirty = true;
                        keep = false;
                    }
                    if ui.input(|i| i.key_pressed(Key::Escape)) {
                        keep = false;
                    }
                });
            if keep {
                self.tag_edit = Some(te);
            }
        }
    }

    fn empty_state(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let p = theme::pal(ui);
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() / 2.0 - 90.0).max(20.0));
            if self.roots.is_empty() {
                ui.label(RichText::new("🗀").size(44.0).color(p.border_strong));
                ui.add_space(8.0);
                ui.label(
                    RichText::new("Add a folder to get started")
                        .size(17.0)
                        .strong()
                        .color(p.text_strong),
                );
                ui.label(
                    RichText::new(
                        "Shiori indexes it in the background. Your files stay where they are.",
                    )
                    .color(p.text_weak),
                );
                ui.add_space(12.0);
                ui.spacing_mut().button_padding = Vec2::new(16.0, 6.0);
                if theme::primary_button(ui, "Add Folder…").clicked() {
                    actions.push(Action::AddRoot);
                }
                ui.add_space(6.0);
                ui.label(
                    RichText::new("or drop a folder onto this window")
                        .size(12.0)
                        .color(p.text_weak),
                );
            } else if !self.scanning.is_empty() {
                ui.add(egui::Spinner::new().size(28.0));
                ui.add_space(8.0);
                ui.label(RichText::new("Indexing…").size(15.0).color(p.text));
            } else {
                ui.label(RichText::new("🔍").size(40.0).color(p.border_strong));
                ui.add_space(8.0);
                ui.label(
                    RichText::new("No matching files")
                        .size(16.0)
                        .strong()
                        .color(p.text_strong),
                );
                if !self.search.is_empty() {
                    ui.label(RichText::new("Try a different search").color(p.text_weak));
                    ui.add_space(8.0);
                    if ui.button("Clear Search").clicked() {
                        actions.push(Action::SetSearch(String::new()));
                    }
                }
            }
        });
    }

    fn toast_overlay(&mut self, ctx: &egui::Context) {
        let Some((msg, t)) = &self.toast else { return };
        let age = t.elapsed().as_secs_f32();
        if age > 5.0 {
            self.toast = None;
            return;
        }
        let alpha = ((5.0 - age) / 0.4).clamp(0.0, 1.0);
        let p = if ctx.theme() == egui::Theme::Dark {
            &theme::DARK
        } else {
            &theme::LIGHT
        };
        egui::Area::new(egui::Id::unique("toast"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -40.0])
            .interactable(false)
            .show(ctx, |ui| {
                ui.set_opacity(alpha);
                theme::card_frame(p)
                    .shadow(ui.visuals().popup_shadow)
                    .inner_margin(Margin::symmetric(14, 8))
                    .show(ui, |ui| {
                        ui.label(RichText::new(msg.as_str()).color(p.text_strong));
                    });
            });
        ctx.request_repaint_after(Duration::from_millis(100));
    }

    fn poll_scans(&mut self) {
        while let Ok(msg) = self.scan_rx.try_recv() {
            match msg {
                ScanMsg::Progress { root, count } => {
                    self.scan_progress.insert(root, count);
                }
                ScanMsg::Done {
                    root,
                    count,
                    removed,
                } => {
                    self.scanning.remove(&root);
                    self.scan_progress.remove(&root);
                    self.dirty = true;
                    self.sidebar_dirty = true;
                    let name = Path::new(&root)
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or(root);
                    let mut m = format!("Scanned \"{name}\" ({count} items)");
                    if removed > 0 {
                        m.push_str(&format!(", {removed} removed"));
                    }
                    self.notify(m);
                }
                ScanMsg::Error { root, message } => {
                    self.scanning.remove(&root);
                    self.scan_progress.remove(&root);
                    self.notify(format!("Scan failed: {root}: {message}"));
                }
            }
        }
    }
}

impl eframe::App for ShioriApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_scans();
        self.thumbs.poll(&ctx);
        self.handle_dropped_files(&ctx);
        if self.sidebar_dirty {
            self.refresh_sidebar();
        }
        if self.dirty {
            self.requery();
        }
        if !self.scanning.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(300));
        }

        let mut actions = Vec::new();
        self.handle_keys(&ctx, &mut actions);

        let p = if ctx.theme() == egui::Theme::Dark {
            &theme::DARK
        } else {
            &theme::LIGHT
        };

        egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(230.0)
            .size_range(170.0..=400.0)
            .frame(theme::panel_frame(p).inner_margin(Margin {
                left: 8,
                right: 8,
                top: 12,
                bottom: 8,
            }))
            .show(ui, |ui| {
                self.sidebar(ui, &mut actions);
            });
        egui::Panel::right("detail")
            .resizable(true)
            .default_size(310.0)
            // wide enough to read a previewed page or presentation
            .size_range(240.0..=760.0)
            .frame(theme::panel_frame(p).inner_margin(Margin::symmetric(14, 14)))
            .show(ui, |ui| {
                self.detail_panel(ui, &mut actions);
            });
        egui::Panel::bottom("status")
            .frame(theme::panel_frame(p).inner_margin(Margin::symmetric(12, 4)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let weak = |t: String| RichText::new(t).size(11.5).color(p.text_weak);
                    let total: i64 = self.rows.iter().filter(|r| !r.is_dir).map(|r| r.size).sum();
                    ui.label(weak(format!(
                        "{} items · {}",
                        self.rows.len(),
                        human_size(total)
                    )));
                    if self.rows.len() >= RESULT_LIMIT {
                        ui.label(
                            RichText::new(format!("showing first {RESULT_LIMIT}"))
                                .size(11.5)
                                .color(p.star),
                        );
                    }
                    if !self.selected.is_empty() {
                        ui.label(weak("·".into()));
                        ui.label(
                            RichText::new(format!("{} selected", self.selected.len()))
                                .size(11.5)
                                .color(p.accent_text),
                        );
                    }
                    if !self.scanning.is_empty() {
                        ui.label(weak("·".into()));
                        ui.add(egui::Spinner::new().size(11.0));
                        ui.label(weak("Scanning…".into()));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (icon, tip, next) = match self.settings.theme {
                            ThemeChoice::System => {
                                ("◑", "Appearance: follow system", ThemeChoice::Light)
                            }
                            ThemeChoice::Light => ("☀", "Appearance: light", ThemeChoice::Dark),
                            ThemeChoice::Dark => ("🌙", "Appearance: dark", ThemeChoice::System),
                        };
                        if theme::icon_button(ui, icon, tip).clicked() {
                            self.settings.theme = next;
                            ctx.set_theme(next.preference());
                        }
                        ui.label(weak(format!("{:.1} ms", self.query_ms)));
                    });
                });
            });
        egui::Panel::top("toolbar")
            .frame(egui::Frame::new().fill(p.bg).inner_margin(Margin {
                left: 14,
                right: 14,
                top: 10,
                bottom: 10,
            }))
            .show_separator_line(true)
            .show(ui, |ui| {
                self.toolbar(ui, &mut actions);
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg).inner_margin(Margin {
                left: 8,
                right: 8,
                top: 6,
                bottom: 0,
            }))
            .show(ui, |ui| {
                if self.rows.is_empty() {
                    self.empty_state(ui, &mut actions);
                    return;
                }
                match self.settings.view {
                    ViewMode::List => self.list_view(ui, &mut actions),
                    ViewMode::Grid => self.grid_view(ui, &mut actions),
                }
            });

        self.toast_overlay(&ctx);

        self.dialogs(&ctx, &mut actions);
        self.show_html(&ctx, frame);

        for a in actions {
            self.apply(&ctx, a);
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, "shiori-settings", &self.settings);
    }
}

impl ShioriApp {
    /// Places the live HTML page over the area the detail panel reserved for
    /// it this frame, or hides it. A native view draws on top of everything
    /// egui paints, so it steps aside for menus, dialogs and drags.
    fn show_html(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        let covered = self.rename.is_some()
            || self.profile_edit.is_some()
            || self.tag_edit.is_some()
            || self.confirm_trash.is_some()
            || ctx.any_popup_open()
            || egui::DragAndDrop::has_any_payload(ctx);
        let ppp = ctx.pixels_per_point();
        let window = ctx.content_rect().size() * ppp;
        let want = self.html_want.take().map(|(path, r)| {
            // Pages are laid out about 560 px wide and scaled down to fit a narrow
            // panel, so they look like the page and not like its phone layout.
            let css_width = r.width() * ctx.zoom_factor();
            let at = htmlview::Placement {
                x: (r.left() * ppp).round() as i32,
                y: (r.top() * ppp).round() as i32,
                width: (r.width() * ppp).round() as u32,
                height: (r.height() * ppp).round() as u32,
                window: [window.x.round() as u32, window.y.round() as u32],
                zoom: ((css_width / 560.0).clamp(0.5, 1.0) * 50.0).round() / 50.0,
            };
            (path, at)
        });
        self.htmlview.update(
            frame,
            want.as_ref().map(|(path, at)| (path.as_path(), *at)),
            covered,
            self.settings.html_scripts,
        );
        // a click anywhere in the app takes the keyboard back from the page
        if ctx.input(|i| i.pointer.any_pressed()) {
            self.htmlview.release_focus();
        }
    }
}

fn handle_item_response(
    resp: &egui::Response,
    idx: usize,
    f: &FileRow,
    selected: &HashSet<i64>,
    actions: &mut Vec<Action>,
) {
    if resp.double_clicked() {
        actions.push(Action::Open(idx));
    } else if resp.clicked() {
        let m = resp.ctx.input(|i| i.modifiers);
        actions.push(Action::Click(idx, m));
    } else if resp.secondary_clicked() && !selected.contains(&f.id) {
        actions.push(Action::Click(idx, Modifiers::NONE));
    }
    if resp.drag_started() {
        let ids: Vec<i64> = if selected.contains(&f.id) {
            selected.iter().copied().collect()
        } else {
            vec![f.id]
        };
        resp.dnd_set_drag_payload(ids);
    }
    if resp.dragged() {
        if let Some(pos) = resp.ctx.pointer_interact_pos() {
            let n = if selected.contains(&f.id) {
                selected.len()
            } else {
                1
            };
            egui::Area::new(egui::Id::unique("drag-ghost"))
                .order(egui::Order::Tooltip)
                .fixed_pos(pos + Vec2::new(12.0, 8.0))
                .show(&resp.ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.label(if n > 1 {
                            format!("{n} items")
                        } else {
                            f.name.clone()
                        });
                    });
                });
        }
    }
}

fn drop_highlight(ui: &egui::Ui, resp: &egui::Response) {
    let p = theme::pal(ui);
    ui.painter().rect_stroke(
        resp.rect,
        6.0,
        Stroke::new(2.0, p.accent),
        egui::StrokeKind::Inside,
    );
}

fn kind_name(f: &FileRow) -> String {
    if f.ext.is_empty() {
        "File".to_string()
    } else {
        format!("{} file", f.ext.to_uppercase())
    }
}

fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

fn label_name(i: usize) -> &'static str {
    [
        "None", "Red", "Orange", "Yellow", "Green", "Blue", "Purple", "Gray",
    ][i]
}

fn tag_token(t: &str) -> String {
    if t.contains(char::is_whitespace) {
        format!("\"#{t}\"")
    } else {
        format!("#{t}")
    }
}

fn sort_name(k: SortKey) -> &'static str {
    match k {
        SortKey::Name => "Name",
        SortKey::Modified => "Modified",
        SortKey::Size => "Size",
        SortKey::Star => "Star",
    }
}

fn fit(size: Vec2, bounds: Vec2) -> Vec2 {
    if size.x <= 0.0 || size.y <= 0.0 {
        return bounds;
    }
    size * (bounds.x / size.x).min(bounds.y / size.y)
}

fn file_icon(f: &FileRow) -> &'static str {
    if f.is_dir {
        return "🗀";
    }
    match f.ext.as_str() {
        e if is_image(e) => "🖼",
        "heic" | "raw" | "cr2" | "nef" | "arw" | "psd" | "svg" | "ai" => "🖼",
        "mp3" | "wav" | "flac" | "aac" | "m4a" | "ogg" => "🎵",
        "mp4" | "mov" | "avi" | "mkv" | "webm" | "m4v" => "🎞",
        "zip" | "7z" | "rar" | "tar" | "gz" | "dmg" | "lzh" => "📦",
        "pdf" => "📕",
        "doc" | "docx" | "pages" | "rtf" => "📝",
        "xls" | "xlsx" | "numbers" | "csv" => "📊",
        "ppt" | "pptx" | "key" => "📽",
        "txt" | "md" | "log" => "📄",
        "rs" | "py" | "js" | "ts" | "c" | "cpp" | "h" | "java" | "go" | "rb" | "html" | "css"
        | "json" | "toml" | "yaml" | "yml" => "📜",
        "exe" | "app" | "msi" | "bat" | "sh" => "⚙",
        _ => "📄",
    }
}

pub fn human_size(n: i64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

fn fmt_time(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%Y/%m/%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

fn reveal_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Show in Finder"
    } else if cfg!(windows) {
        "Show in Explorer"
    } else {
        "Open Containing Folder"
    }
}

/// Shows the file in the OS file manager, selected.
fn reveal(path: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn();
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("explorer")
            .raw_arg(format!("/select,\"{path}\""))
            .spawn();
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let p = Path::new(path);
        let dir = if p.is_dir() {
            p
        } else {
            p.parent().unwrap_or(p)
        };
        let _ = open::that_detached(dir);
    }
}
