//! Dragging files out of Shiori into other apps (Finder / Explorer, editors,
//! mail, chat, browsers…).
//!
//! egui only knows drags inside its own window, which is how files get dropped
//! onto tags and virtual folders. When such a drag leaves the window we hand it
//! over to the OS as a real file drag (an `NSDraggingSession` on macOS, OLE
//! `DoDragDrop` on Windows) through the `drag` crate, so the other app receives
//! the files exactly as if they came from Finder or Explorer. Elsewhere (Linux
//! dev builds) drags simply stay inside the app.

use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How close (in points) to the window border a held drag turns into an OS drag.
/// Catches drags whose last in-window position never quite reaches outside.
const EDGE: f32 = 2.0;

#[derive(Default)]
pub struct DragOut {
    /// Set to true by the OS callback once the drag is dropped or cancelled.
    session: Option<Arc<AtomicBool>>,
    /// The files being dragged (canonical), so dropping them back onto Shiori
    /// itself does nothing.
    paths: Vec<PathBuf>,
    finished_at: Option<Instant>,
}

impl DragOut {
    /// Call after the views ran: true when a file drag held inside the app has
    /// reached the edge of the window or left it, and should become an OS drag.
    pub fn should_start(&self, ctx: &egui::Context) -> bool {
        if !cfg!(any(windows, target_os = "macos"))
            || self.session.is_some()
            || !egui::DragAndDrop::has_payload_of_type::<Vec<i64>>(ctx)
        {
            return false;
        }
        let inside = ctx.content_rect().shrink(EDGE);
        ctx.input(|i| {
            i.pointer.primary_down() && i.pointer.latest_pos().is_none_or(|p| !inside.contains(p))
        })
    }

    /// Ends the in-app drag and starts an OS drag of `paths` (absolute paths of
    /// existing files). On Windows this returns only after the drop.
    #[allow(unused_variables)]
    pub fn start(&mut self, ctx: &egui::Context, frame: &eframe::Frame, paths: Vec<PathBuf>) {
        egui::DragAndDrop::clear_payload(ctx);
        let paths: Vec<PathBuf> = paths.into_iter().filter(|p| p.exists()).collect();
        if paths.is_empty() {
            return;
        }
        let done = Arc::new(AtomicBool::new(false));
        self.session = Some(done.clone());
        self.paths = paths.iter().map(|p| canonical(p)).collect();
        #[cfg(any(windows, target_os = "macos"))]
        {
            let repaint = ctx.clone();
            let flag = done.clone();
            let res = drag::start_drag(
                frame,
                drag::DragItem::Files(paths),
                drag::Image::Raw(drag_icon_png()),
                move |_result, _at| {
                    flag.store(true, Ordering::SeqCst);
                    repaint.request_repaint();
                },
                drag::Options::default(),
            );
            if let Err(e) = res {
                eprintln!("shiori: could not start a file drag: {e}");
                done.store(true, Ordering::SeqCst);
                ctx.request_repaint();
            }
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        done.store(true, Ordering::SeqCst);
    }

    /// Call from `App::raw_input_hook`. The OS keeps the mouse-up that ends its
    /// drag, so egui would think the button is still held; feed egui that
    /// release once the drag is over.
    pub fn fix_input(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        let Some(done) = &self.session else { return };
        if !done.load(Ordering::SeqCst) {
            return;
        }
        self.session = None;
        self.finished_at = Some(Instant::now());
        let released = raw.events.iter().any(|e| {
            matches!(
                e,
                egui::Event::PointerButton {
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    ..
                }
            )
        });
        let (down, pos, modifiers) = ctx.input(|i| {
            (
                i.pointer.primary_down(),
                i.pointer.latest_pos(),
                i.modifiers,
            )
        });
        if down && !released {
            raw.events.push(egui::Event::PointerButton {
                pos: pos.unwrap_or_default(),
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers,
            });
        }
    }

    /// True for files dropped onto Shiori that are the very files it is
    /// dragging out (the user changed their mind and came back): ignore them.
    pub fn is_own_drop(&self, dropped: &[PathBuf]) -> bool {
        let recent = self.session.is_some()
            || self
                .finished_at
                .is_some_and(|t| t.elapsed() < Duration::from_secs(2));
        recent && !dropped.is_empty() && dropped.iter().all(|p| self.paths.contains(&canonical(p)))
    }
}

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// The picture under the cursor while dragging: the app icon, 64 px.
#[cfg(any(windows, target_os = "macos"))]
fn drag_icon_png() -> Vec<u8> {
    use std::sync::OnceLock;
    static PNG: OnceLock<Vec<u8>> = OnceLock::new();
    PNG.get_or_init(|| {
        let src: &[u8] = include_bytes!("../assets/icon/shiori-512.png");
        let mut out = std::io::Cursor::new(Vec::new());
        let ok = image::load_from_memory(src).ok().and_then(|img| {
            img.resize(64, 64, image::imageops::FilterType::Triangle)
                .write_to(&mut out, image::ImageFormat::Png)
                .ok()
        });
        match ok {
            Some(()) => out.into_inner(),
            None => src.to_vec(),
        }
    })
    .clone()
}
