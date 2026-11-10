//! The egui user interface.

use crate::db::{Db, FileRow, Profile, Scope, SortKey, LABELS};
use crate::query::Filter;
use crate::scan::{spawn_scan, ScanMsg};
use crate::thumbs::{has_thumbnail, is_image, Thumbs};
use eframe::egui::{self, Color32, Key, Modifiers, RichText, Sense, Vec2};
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

#[derive(serde::Serialize, serde::Deserialize)]
struct Settings {
    view: ViewMode,
    sort: SortKey,
    desc: bool,
    thumb_size: f32,
    recursive: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            view: ViewMode::List,
            sort: SortKey::Name,
            desc: false,
            thumb_size: 128.0,
            recursive: false,
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
}

impl ShioriApp {
    pub fn new(cc: &eframe::CreationContext<'_>, db: Db) -> Self {
        crate::fonts::install(&cc.egui_ctx);
        let settings: Settings = cc
            .storage
            .and_then(|s| eframe::get_value(s, "shiori-settings"))
            .unwrap_or_default();
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
                self.report(r);
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
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space(6.0);
            ui.heading(RichText::new("Shiori").strong());
            ui.add_space(4.0);

            let quick = [
                ("📚 All Files", Scope::All, ""),
                ("⭐ Starred", Scope::All, "star:1"),
                ("🏷 Untagged", Scope::All, "is:untagged is:file"),
                ("💬 With Comments", Scope::All, "is:comment"),
                ("⚠ Missing Files", Scope::All, "is:missing"),
            ];
            for (label, scope, q) in quick {
                let active = self.scope == scope && self.search == q;
                if ui.selectable_label(active, label).clicked() {
                    actions.push(Action::SetScope(scope));
                    actions.push(Action::SetSearch(q.to_string()));
                }
            }

            ui.separator();
            egui::CollapsingHeader::new("📁 Folders").default_open(true).show(ui, |ui| {
                for root in &self.roots {
                    let name = Path::new(root)
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| root.clone());
                    let active = matches!(&self.scope, Scope::Folder { path, .. } if path == root);
                    let mut text = name.clone();
                    if let Some(n) = self.scan_progress.get(root) {
                        text = format!("{name}  ⟳{n}");
                    } else if self.scanning.contains(root) {
                        text = format!("{name}  ⟳");
                    }
                    let resp = ui.selectable_label(active, text).on_hover_text(root);
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
                        if ui.button("Remove from Shiori").clicked() {
                            actions.push(Action::RemoveRoot(root.clone()));
                            ui.close();
                        }
                    });
                }
                if ui.small_button("+ Add Folder").clicked() {
                    actions.push(Action::AddRoot);
                }
            });

            egui::CollapsingHeader::new("📂 Virtual Folders").default_open(true).show(ui, |ui| {
                for (p, n) in &self.profiles {
                    let active = self.scope == Scope::Profile(p.id);
                    let text = if p.query.is_some() {
                        format!("🔎 {}", p.name)
                    } else {
                        format!("{}  ({n})", p.name)
                    };
                    let resp = ui.selectable_label(active, text);
                    if resp.clicked() {
                        actions.push(Action::SetScope(Scope::Profile(p.id)));
                    }
                    if p.query.is_none() {
                        if let Some(ids) = resp.dnd_release_payload::<Vec<i64>>() {
                            actions.push(Action::AddToProfile(p.id, (*ids).clone()));
                        }
                        if resp.dnd_hover_payload::<Vec<i64>>().is_some() {
                            ui.painter().rect_stroke(
                                resp.rect,
                                3.0,
                                (2.0, ui.visuals().selection.bg_fill),
                                egui::StrokeKind::Inside,
                            );
                        }
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Edit…").clicked() {
                            actions.push(Action::EditProfile(Some(p.id), false));
                            ui.close();
                        }
                        if ui.button("Delete").clicked() {
                            actions.push(Action::DeleteProfile(p.id));
                            ui.close();
                        }
                    });
                }
                ui.horizontal(|ui| {
                    if ui.small_button("+ New").on_hover_text("A virtual folder you fill by hand").clicked() {
                        actions.push(Action::EditProfile(None, false));
                    }
                    if ui
                        .small_button("+ Smart")
                        .on_hover_text("A virtual folder filled automatically by a search (saves the current search)")
                        .clicked()
                    {
                        actions.push(Action::EditProfile(None, true));
                    }
                });
            });

            egui::CollapsingHeader::new("🏷 Tags").default_open(true).show(ui, |ui| {
                if self.tags.is_empty() {
                    ui.weak("Add tags from the panel on the right");
                }
                let current = Filter::parse(&self.search);
                for (t, n) in &self.tags {
                    let active = current.tags.iter().any(|x| x.eq_ignore_ascii_case(t));
                    let resp = ui.selectable_label(active, format!("#{t}  ({n})"));
                    if resp.clicked() {
                        let token = tag_token(t);
                        let cmd = ui.input(|i| i.modifiers.command);
                        let q = if cmd {
                            if active {
                                self.search.replace(&token, "").trim().to_string()
                            } else {
                                format!("{} {}", self.search.trim(), token).trim().to_string()
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
            });

            egui::CollapsingHeader::new("🎨 Labels").default_open(true).show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (i, (en, _, c)) in LABELS.iter().enumerate().skip(1) {
                        let (rect, resp) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::click());
                        ui.painter().circle_filled(rect.center(), 7.0, rgb(*c));
                        let resp = resp.on_hover_text(label_name(i));
                        if resp.clicked() {
                            actions.push(Action::SetScope(Scope::All));
                            actions.push(Action::SetSearch(format!("label:{en}")));
                        }
                        if let Some(ids) = resp.dnd_release_payload::<Vec<i64>>() {
                            actions.push(Action::SetLabel((*ids).clone(), i as u8));
                        }
                    }
                });
            });
            ui.add_space(8.0);
            ui.weak("Tip: drag files onto a tag, virtual folder or label to apply it");
        });
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!self.back.is_empty(), egui::Button::new("◀"))
                .on_hover_text("Back")
                .clicked()
            {
                if let Some(s) = self.back.pop() {
                    self.scope = s;
                    self.dirty = true;
                }
            }
            if let Scope::Folder { path, recursive } = self.scope.clone() {
                let at_root = self.roots.contains(&path);
                if ui
                    .add_enabled(!at_root, egui::Button::new("⬆"))
                    .on_hover_text("Up one folder")
                    .clicked()
                {
                    if let Some(parent) = Path::new(&path).parent() {
                        actions.push(Action::EnterDir(crate::db::path_key(parent)));
                    }
                }
                let mut rec = recursive;
                if ui.checkbox(&mut rec, "Include subfolders").changed() {
                    self.settings.recursive = rec;
                    self.scope = Scope::Folder {
                        path: path.clone(),
                        recursive: rec,
                    };
                    self.dirty = true;
                }
                self.breadcrumb(ui, &path, actions);
            } else {
                let title = match &self.scope {
                    Scope::All => "All Files".to_string(),
                    Scope::Profile(id) => self
                        .profiles
                        .iter()
                        .find(|p| p.0.id == *id)
                        .map(|p| format!("📂 {}", p.0.name))
                        .unwrap_or_default(),
                    Scope::Folder { .. } => unreachable!(),
                };
                ui.strong(title);
            }
        });
        ui.horizontal(|ui| {
            let search_id = egui::Id::unique("shiori-search");
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .id(search_id)
                    .hint_text("🔍 Search  e.g. trip #family star:3 ext:jpg label:red is:untagged")
                    .desired_width(
                        (ui.available_width()
                            - if self.settings.view == ViewMode::Grid {
                                420.0
                            } else {
                                300.0
                            })
                        .max(140.0),
                    ),
            );
            if ui.input(|i| i.modifiers.command && i.key_pressed(Key::F)) {
                resp.request_focus();
            }
            if resp.changed() {
                self.dirty = true;
            }
            if !self.search.is_empty() && ui.small_button("×").clicked() {
                self.search.clear();
                self.dirty = true;
            }
            ui.separator();
            let old = (self.settings.sort, self.settings.desc);
            egui::ComboBox::from_id_salt("sort")
                .width(80.0)
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
            if ui
                .button(if self.settings.desc { "↓" } else { "↑" })
                .on_hover_text("Ascending / descending")
                .clicked()
            {
                self.settings.desc = !self.settings.desc;
            }
            if old != (self.settings.sort, self.settings.desc) {
                self.dirty = true;
            }
            ui.separator();
            ui.selectable_value(&mut self.settings.view, ViewMode::List, "☰")
                .on_hover_text("List view");
            ui.selectable_value(&mut self.settings.view, ViewMode::Grid, "🖼")
                .on_hover_text("Thumbnail view");
            if self.settings.view == ViewMode::Grid {
                ui.spacing_mut().slider_width = 90.0;
                ui.add(
                    egui::Slider::new(&mut self.settings.thumb_size, 64.0..=256.0)
                        .show_value(false),
                );
            }
            if ui.button("⟳").on_hover_text("Rescan all folders").clicked() {
                actions.push(Action::Rescan(self.roots.clone()));
            }
        });
    }

    fn breadcrumb(&self, ui: &mut egui::Ui, path: &str, actions: &mut Vec<Action>) {
        let root = self
            .roots
            .iter()
            .filter(|r| path == r.as_str() || path.starts_with(&crate::db::subtree_range(r).0))
            .max_by_key(|r| r.len());
        let Some(root) = root else {
            ui.label(path);
            return;
        };
        let mut cur = PathBuf::from(root);
        let root_name = cur
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| root.clone());
        if ui.link(root_name).clicked() {
            actions.push(Action::EnterDir(root.clone()));
        }
        let rest = Path::new(path)
            .strip_prefix(root)
            .ok()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        for comp in rest.components() {
            cur.push(comp);
            ui.label("›");
            if ui.link(comp.as_os_str().to_string_lossy()).clicked() {
                actions.push(Action::EnterDir(crate::db::path_key(&cur)));
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
        let row_h = 22.0;
        let text_h = ui.text_style_height(&egui::TextStyle::Body);
        let mut table = TableBuilder::new(ui)
            .striped(true)
            .sense(Sense::click_and_drag())
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(24.0))
            .column(Column::initial(240.0).at_least(80.0).clip(true))
            .column(Column::initial(70.0).at_least(30.0).clip(true))
            .column(Column::initial(130.0).at_least(40.0).clip(true))
            .column(Column::initial(70.0).at_least(40.0).clip(true))
            .column(Column::initial(115.0).at_least(60.0).clip(true))
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
            .header(20.0, |mut h| {
                for t in [
                    "",
                    "Name",
                    "Star",
                    "Tags",
                    "Size",
                    "Modified",
                    if show_path { "Location" } else { "Comment" },
                ] {
                    h.col(|ui| {
                        ui.strong(t);
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
                        if let Some(tex) = tex {
                            let size = fit(tex.size_vec2(), Vec2::splat(text_h + 4.0));
                            ui.add(egui::Image::new((tex.id(), size)));
                        } else {
                            ui.label(file_icon(f));
                        }
                    });
                    row.col(|ui| {
                        if f.label > 0 {
                            ui.label(RichText::new("●").color(rgb(LABELS[f.label as usize].2)));
                        }
                        let mut t = RichText::new(&f.name);
                        if f.missing {
                            t = t.strikethrough().weak();
                        }
                        ui.add(egui::Label::new(t).truncate().selectable(false));
                    });
                    row.col(|ui| {
                        if f.star > 0 {
                            ui.label(RichText::new("★".repeat(f.star as usize)).color(STAR));
                        }
                    });
                    row.col(|ui| {
                        if !f.tags.is_empty() {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(
                                        f.tags
                                            .iter()
                                            .map(|t| format!("#{t}"))
                                            .collect::<Vec<_>>()
                                            .join(" "),
                                    )
                                    .color(TAG),
                                )
                                .truncate()
                                .selectable(false),
                            );
                        }
                    });
                    row.col(|ui| {
                        if !f.is_dir {
                            ui.label(human_size(f.size));
                        }
                    });
                    row.col(|ui| {
                        ui.label(fmt_time(f.mtime));
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
                            egui::Label::new(RichText::new(t).weak())
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
        let cell = self.settings.thumb_size;
        let cell_w = cell + 16.0;
        let cell_h = cell + 44.0;
        let width = ui.available_width();
        let cols = ((width / cell_w).floor() as usize).max(1);
        let n_rows = self.rows.len().div_ceil(cols);
        let rows = &self.rows;
        let selected = &self.selected;
        let thumbs = &mut self.thumbs;
        let mut menu: Option<(usize, Vec<i64>, egui::Response)> = None;
        let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]);
        if let Some(i) = self.scroll_to.take() {
            let y = (i / cols) as f32 * (cell_h + ui.spacing().item_spacing.y);
            area = area.vertical_scroll_offset(y);
        }
        area.show_rows(ui, cell_h, n_rows, |ui, range| {
            for r in range {
                ui.horizontal(|ui| {
                    for c in 0..cols {
                        let idx = r * cols + c;
                        let Some(f) = rows.get(idx) else { break };
                        let (rect, resp) = ui.allocate_exact_size(
                            Vec2::new(cell_w, cell_h),
                            Sense::click_and_drag(),
                        );
                        if !ui.is_rect_visible(rect) {
                            continue;
                        }
                        let p = ui.painter_at(rect);
                        let sel = selected.contains(&f.id);
                        if sel {
                            p.rect_filled(
                                rect.shrink(1.0),
                                6.0,
                                ui.visuals().selection.bg_fill.gamma_multiply(0.6),
                            );
                        } else if resp.hovered() {
                            p.rect_filled(
                                rect.shrink(1.0),
                                6.0,
                                ui.visuals().widgets.hovered.weak_bg_fill,
                            );
                        }
                        let img_rect = egui::Rect::from_min_size(
                            rect.min + Vec2::new(8.0, 6.0),
                            Vec2::splat(cell),
                        );
                        let tex = if !f.is_dir && has_thumbnail(&f.ext) {
                            thumbs.get(f.id, &f.path, if cell > 128.0 { 256 } else { 128 })
                        } else {
                            None
                        };
                        if let Some(tex) = tex {
                            let s = fit(tex.size_vec2(), img_rect.size());
                            let r = egui::Rect::from_center_size(img_rect.center(), s);
                            p.image(
                                tex.id(),
                                r,
                                egui::Rect::from_min_max(
                                    egui::pos2(0.0, 0.0),
                                    egui::pos2(1.0, 1.0),
                                ),
                                Color32::WHITE,
                            );
                        } else {
                            p.text(
                                img_rect.center(),
                                egui::Align2::CENTER_CENTER,
                                file_icon(f),
                                egui::FontId::proportional(cell * 0.45),
                                ui.visuals().text_color(),
                            );
                        }
                        if f.label > 0 {
                            p.circle_filled(
                                img_rect.left_top() + Vec2::new(8.0, 8.0),
                                6.0,
                                rgb(LABELS[f.label as usize].2),
                            );
                        }
                        if f.star > 0 {
                            p.text(
                                img_rect.right_top() + Vec2::new(-2.0, 2.0),
                                egui::Align2::RIGHT_TOP,
                                "★".repeat(f.star as usize),
                                egui::FontId::proportional(12.0),
                                STAR,
                            );
                        }
                        let name = egui::WidgetText::from(RichText::new(&f.name).size(12.0));
                        let galley = name.into_galley(
                            ui,
                            Some(egui::TextWrapMode::Truncate),
                            cell_w - 8.0,
                            egui::TextStyle::Body,
                        );
                        let pos = egui::pos2(
                            rect.center().x - galley.size().x / 2.0,
                            img_rect.bottom() + 4.0,
                        );
                        p.galley(pos, galley, ui.visuals().text_color());
                        if !f.tags.is_empty() {
                            let t = f
                                .tags
                                .iter()
                                .map(|t| format!("#{t}"))
                                .collect::<Vec<_>>()
                                .join(" ");
                            let g = egui::WidgetText::from(RichText::new(t).size(10.0).color(TAG))
                                .into_galley(
                                    ui,
                                    Some(egui::TextWrapMode::Truncate),
                                    cell_w - 8.0,
                                    egui::TextStyle::Small,
                                );
                            let pos = egui::pos2(
                                rect.center().x - g.size().x / 2.0,
                                img_rect.bottom() + 20.0,
                            );
                            p.galley(pos, g, TAG);
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
        let sel: Vec<FileRow> = self.selected_rows().into_iter().cloned().collect();
        if sel.is_empty() {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                ui.weak("Select a file to edit\nits tags, stars and comment");
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

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(6.0);
                if single {
                    let w = ui.available_width();
                    let tex = if !first.is_dir && has_thumbnail(&first.ext) {
                        self.thumbs.get(first.id, &first.path, 512)
                    } else {
                        None
                    };
                    ui.vertical_centered(|ui| {
                        if let Some(tex) = tex {
                            let s = fit(tex.size_vec2(), Vec2::new(w, 260.0));
                            ui.add(egui::Image::new((tex.id(), s)));
                        } else {
                            ui.label(RichText::new(file_icon(first)).size(64.0));
                        }
                    });
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.add(
                            egui::Label::new(RichText::new(&first.name).strong().size(15.0)).wrap(),
                        );
                        if ui.small_button("✏").on_hover_text("Rename (F2)").clicked() {
                            actions.push(Action::StartRename(first.id));
                        }
                    });
                    if first.missing {
                        ui.colored_label(Color32::from_rgb(220, 120, 60), "⚠ Not found on disk");
                    }
                    egui::Grid::new("info")
                        .num_columns(2)
                        .spacing([8.0, 2.0])
                        .show(ui, |ui| {
                            ui.weak("Location");
                            let parent = Path::new(&first.path)
                                .parent()
                                .map(|p| p.to_string_lossy().to_string())
                                .unwrap_or_default();
                            let link = egui::Label::new(
                                RichText::new(&parent).color(ui.visuals().hyperlink_color),
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
                            if !first.is_dir {
                                ui.weak("Size");
                                ui.label(format!(
                                    "{} ({} bytes)",
                                    human_size(first.size),
                                    first.size
                                ));
                                ui.end_row();
                            }
                            ui.weak("Modified");
                            ui.label(fmt_time(first.mtime));
                            ui.end_row();
                        });
                    ui.horizontal(|ui| {
                        if ui.button("Open").clicked() {
                            if let Some(idx) = self.rows.iter().position(|r| r.id == first.id) {
                                actions.push(Action::Open(idx));
                            }
                        }
                        if ui.button(reveal_label()).clicked() {
                            actions.push(Action::Reveal(first.path.clone()));
                        }
                    });
                } else {
                    ui.heading(format!("{} items selected", sel.len()));
                    ui.weak("Changes here apply to every selected item");
                }

                ui.separator();
                ui.strong("Stars");
                ui.horizontal(|ui| {
                    let cur = if sel.iter().all(|r| r.star == first.star) {
                        first.star
                    } else {
                        0
                    };
                    for n in 1..=5u8 {
                        let filled = n <= cur;
                        let t = RichText::new(if filled { "★" } else { "☆" })
                            .size(20.0)
                            .color(if filled {
                                STAR
                            } else {
                                ui.visuals().weak_text_color()
                            });
                        if ui.add(egui::Button::new(t).frame(false)).clicked() {
                            actions
                                .push(Action::SetStar(ids.clone(), if cur == n { 0 } else { n }));
                        }
                    }
                });

                ui.strong("Label");
                ui.horizontal_wrapped(|ui| {
                    for (i, (_, _, c)) in LABELS.iter().enumerate() {
                        let (rect, resp) =
                            ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
                        let on = sel.iter().all(|r| r.label as usize == i);
                        if i == 0 {
                            ui.painter().circle_stroke(
                                rect.center(),
                                8.0,
                                (1.5, ui.visuals().weak_text_color()),
                            );
                            ui.painter().line_segment(
                                [
                                    rect.center() + Vec2::new(-5.0, 5.0),
                                    rect.center() + Vec2::new(5.0, -5.0),
                                ],
                                (1.5, ui.visuals().weak_text_color()),
                            );
                        } else {
                            ui.painter().circle_filled(rect.center(), 8.0, rgb(*c));
                        }
                        if on {
                            ui.painter().circle_stroke(
                                rect.center(),
                                10.5,
                                (2.0, ui.visuals().strong_text_color()),
                            );
                        }
                        if resp.on_hover_text(label_name(i)).clicked() {
                            actions.push(Action::SetLabel(ids.clone(), i as u8));
                        }
                    }
                });

                ui.strong("Tags");
                let mut all_tags: Vec<(String, usize)> = vec![];
                for r in &sel {
                    for t in &r.tags {
                        match all_tags.iter_mut().find(|x| &x.0 == t) {
                            Some(x) => x.1 += 1,
                            None => all_tags.push((t.clone(), 1)),
                        }
                    }
                }
                ui.horizontal_wrapped(|ui| {
                    for (t, n) in &all_tags {
                        let partial = *n < sel.len();
                        let text = if partial {
                            format!("#{t} ({n}) ×")
                        } else {
                            format!("#{t} ×")
                        };
                        if ui
                            .add(
                                egui::Button::new(RichText::new(text).color(TAG))
                                    .corner_radius(10.0),
                            )
                            .on_hover_text("Click to remove this tag")
                            .clicked()
                        {
                            actions.push(Action::RemoveTag(ids.clone(), t.clone()));
                        }
                    }
                });
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.tag_input)
                        .hint_text("Add tags (Enter; comma-separated)")
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
                let suggestions: Vec<&String> = self
                    .tags
                    .iter()
                    .map(|t| &t.0)
                    .filter(|t| !all_tags.iter().any(|a| &a.0 == *t))
                    .filter(|t| q.is_empty() || t.to_lowercase().contains(&q))
                    .take(12)
                    .collect();
                if !suggestions.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        ui.weak("Suggestions:");
                        for t in suggestions {
                            if ui.small_button(format!("#{t}")).clicked() {
                                actions.push(Action::AddTag(ids.clone(), t.clone()));
                                self.tag_input.clear();
                            }
                        }
                    });
                }

                ui.strong("Comment");
                let resp = ui.add(
                    egui::TextEdit::multiline(&mut self.comment_buf)
                        .hint_text(if single {
                            "Write a note (saved automatically)"
                        } else {
                            "Typing here replaces the comment of every selected item"
                        })
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
                    ui.strong("Virtual Folders");
                    let member: HashSet<i64> = if single {
                        self.db
                            .profiles_of(first.id)
                            .unwrap_or_default()
                            .into_iter()
                            .collect()
                    } else {
                        HashSet::new()
                    };
                    for p in manual {
                        if single {
                            let mut on = member.contains(&p.id);
                            if ui.checkbox(&mut on, &p.name).changed() {
                                actions.push(if on {
                                    Action::AddToProfile(p.id, ids.clone())
                                } else {
                                    Action::RemoveFromProfile(p.id, ids.clone())
                                });
                            }
                        } else if ui.button(format!("+ {}", p.name)).clicked() {
                            actions.push(Action::AddToProfile(p.id, ids.clone()));
                        }
                    }
                }
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
                    let r = ui.add(egui::TextEdit::singleline(&mut name).desired_width(320.0));
                    r.request_focus();
                    if (r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)))
                        || ui.button("Rename").clicked()
                    {
                        done = true;
                    }
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
                    ui.horizontal(|ui| {
                        if ui.button("Move to Trash").clicked() {
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
            let title = match (pe.id, pe.smart) {
                (None, false) => "New Virtual Folder",
                (None, true) => "New Smart Folder",
                (Some(_), _) => "Edit Virtual Folder",
            };
            egui::Window::new(title)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    egui::Grid::new("pe").num_columns(2).show(ui, |ui| {
                        ui.label("Name");
                        let r =
                            ui.add(egui::TextEdit::singleline(&mut pe.name).desired_width(300.0));
                        if pe.name.is_empty() {
                            r.request_focus();
                        }
                        ui.end_row();
                        if pe.smart {
                            ui.label("Search");
                            ui.add(
                                egui::TextEdit::singleline(&mut pe.query)
                                    .desired_width(300.0)
                                    .hint_text("#work star:3 ext:pdf"),
                            );
                            ui.end_row();
                        }
                    });
                    if pe.smart {
                        ui.weak("Files matching this search appear automatically");
                    } else {
                        ui.weak(
                            "Add files by dragging them here or via right-click → Add to Virtual Folder",
                        );
                    }
                    ui.horizontal(|ui| {
                        let ok = !pe.name.trim().is_empty();
                        if ui.add_enabled(ok, egui::Button::new("Save")).clicked() {
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
                    let r = ui.text_edit_singleline(&mut te.new);
                    ui.weak("Using an existing tag name merges the two tags");
                    if (r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)))
                        || ui.button("Rename").clicked()
                    {
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
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
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

        egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(220.0)
            .size_range(160.0..=400.0)
            .show(ui, |ui| {
                self.sidebar(ui, &mut actions);
            });
        egui::Panel::right("detail")
            .resizable(true)
            .default_size(300.0)
            .size_range(220.0..=520.0)
            .show(ui, |ui| {
                self.detail_panel(ui, &mut actions);
            });
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                let total: i64 = self.rows.iter().filter(|r| !r.is_dir).map(|r| r.size).sum();
                ui.label(format!("{} items", self.rows.len()));
                if self.rows.len() >= RESULT_LIMIT {
                    ui.colored_label(
                        Color32::from_rgb(220, 150, 50),
                        format!("(showing first {RESULT_LIMIT})"),
                    );
                }
                ui.weak(format!("{} · {:.1}ms", human_size(total), self.query_ms));
                if !self.selected.is_empty() {
                    ui.separator();
                    ui.label(format!("{} selected", self.selected.len()));
                }
                if !self.scanning.is_empty() {
                    ui.separator();
                    ui.spinner();
                    ui.label("Scanning…");
                }
                if let Some((msg, t)) = &self.toast {
                    if t.elapsed() < Duration::from_secs(5) {
                        ui.separator();
                        ui.label(msg);
                        ctx.request_repaint_after(Duration::from_millis(500));
                    }
                }
            });
        });
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.add_space(4.0);
            self.toolbar(ui, &mut actions);
            ui.add_space(2.0);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            if self.rows.is_empty() {
                ui.centered_and_justified(|ui| {
                    if self.roots.is_empty() {
                        ui.label("Click \"+ Add Folder\" on the left, or drop a folder onto this window to get started");
                    } else if !self.scanning.is_empty() {
                        ui.label("Scanning…");
                    } else {
                        ui.weak("No matching files");
                    }
                });
                return;
            }
            match self.settings.view {
                ViewMode::List => self.list_view(ui, &mut actions),
                ViewMode::Grid => self.grid_view(ui, &mut actions),
            }
        });

        self.dialogs(&ctx, &mut actions);

        for a in actions {
            self.apply(&ctx, a);
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, "shiori-settings", &self.settings);
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

const STAR: Color32 = Color32::from_rgb(240, 180, 30);
const TAG: Color32 = Color32::from_rgb(90, 150, 220);

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
        return "📁";
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
