//! Thumbnail decoding on worker threads (newest request first, so what is on
//! screen right now gets decoded before rows the user already scrolled past).

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};

pub const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "bmp", "tif", "tiff", "ico",
];

pub fn is_image(ext: &str) -> bool {
    IMAGE_EXTS.contains(&ext)
}

/// Whether we can show a picture of this file's contents (decoded by us or by the OS).
pub fn has_thumbnail(ext: &str) -> bool {
    is_image(ext) || crate::osthumb::supports(ext)
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Key {
    pub id: i64,
    pub size: u32,
}

enum Res {
    Ok(Key, ColorImage),
    Failed(Key),
}

struct Queue {
    stack: Mutex<Vec<(Key, PathBuf)>>,
    cv: Condvar,
}

const MAX_PENDING: usize = 400;

pub struct Thumbs {
    queue: Arc<Queue>,
    rx: Receiver<Res>,
    cache: HashMap<Key, TextureHandle>,
    requested: HashSet<Key>,
    failed: HashSet<Key>,
}

impl Thumbs {
    pub fn new(ctx: &egui::Context) -> Self {
        let queue = Arc::new(Queue {
            stack: Mutex::new(Vec::new()),
            cv: Condvar::new(),
        });
        let (tx, rx) = channel();
        let workers = std::thread::available_parallelism()
            .map(|n| n.get().clamp(2, 6))
            .unwrap_or(2);
        for _ in 0..workers {
            let q = queue.clone();
            let tx: Sender<Res> = tx.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || loop {
                let job = {
                    let mut st = q.stack.lock().unwrap();
                    loop {
                        if let Some(j) = st.pop() {
                            break j;
                        }
                        st = q.cv.wait(st).unwrap();
                    }
                };
                let (key, path) = job;
                let res = match decode(&path, key.size) {
                    Some(img) => Res::Ok(key, img),
                    None => Res::Failed(key),
                };
                if tx.send(res).is_err() {
                    return;
                }
                ctx.request_repaint();
            });
        }
        Self {
            queue,
            rx,
            cache: HashMap::new(),
            requested: HashSet::new(),
            failed: HashSet::new(),
        }
    }

    /// Moves finished decodes into GPU textures. Call once per frame.
    pub fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(r) = self.rx.try_recv() {
            match r {
                Res::Ok(key, img) => {
                    self.requested.remove(&key);
                    let tex = ctx.load_texture(
                        format!("thumb-{}-{}", key.id, key.size),
                        img,
                        TextureOptions::LINEAR,
                    );
                    self.cache.insert(key, tex);
                }
                Res::Failed(key) => {
                    self.requested.remove(&key);
                    self.failed.insert(key);
                }
            }
        }
        // keep memory bounded
        if self.cache.len() > 3000 {
            self.cache.clear();
        }
    }

    /// Returns the texture if ready, otherwise queues it.
    pub fn get(&mut self, id: i64, path: &str, size: u32) -> Option<&TextureHandle> {
        let key = Key { id, size };
        if self.cache.contains_key(&key) {
            return self.cache.get(&key);
        }
        if self.failed.contains(&key) || self.requested.contains(&key) {
            return None;
        }
        self.requested.insert(key);
        let mut st = self.queue.stack.lock().unwrap();
        st.push((key, PathBuf::from(path)));
        if st.len() > MAX_PENDING {
            let (old, _) = st.remove(0);
            self.requested.remove(&old);
        }
        drop(st);
        self.queue.cv.notify_one();
        None
    }

    /// Forget a file's thumbnails (after it changed on disk).
    pub fn invalidate(&mut self, id: i64) {
        self.cache.retain(|k, _| k.id != id);
        self.failed.retain(|k| k.id != id);
    }
}

fn decode(path: &PathBuf, size: u32) -> Option<ColorImage> {
    let ext = crate::db::ext_of(path);
    if is_image(&ext) {
        if let Some(img) = decode_image(path, size) {
            return Some(img);
        }
    }
    crate::osthumb::thumbnail(path, size)
}

fn decode_image(path: &PathBuf, size: u32) -> Option<ColorImage> {
    let reader = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?;
    let img = reader.decode().ok()?;
    Some(to_color_image(img.thumbnail(size, size)))
}

pub fn to_color_image(img: image::DynamicImage) -> ColorImage {
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    ColorImage::from_rgba_unmultiplied([w as usize, h as usize], rgba.as_raw())
}
