//! Thumbnails, made on worker threads (newest request first, so what is on
//! screen right now is done before rows the user already scrolled past).
//!
//! Making one is kept cheap:
//! * finished thumbnails are saved to a disk cache, so the next run (or the
//!   next time a row scrolls back into view) only reads a small file;
//! * JPEG photos use the preview the camera embedded, or are decoded at
//!   1/2–1/8 scale, instead of decoding every pixel and shrinking;
//! * textures on the GPU are dropped least-recently-used first, within a
//!   memory budget, instead of all at once.

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};
use image::{DynamicImage, RgbaImage};
use std::collections::{HashMap, HashSet};
use std::io::{BufReader, Seek};
use std::path::{Path, PathBuf};
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

struct Job {
    key: Key,
    path: PathBuf,
    /// The file's modification time and length: a changed file gets a new cache entry.
    mtime: i64,
    len: i64,
}

enum Res {
    Ok(Key, ColorImage),
    Failed(Key),
}

struct Queue {
    stack: Mutex<Vec<Job>>,
    cv: Condvar,
}

struct Entry {
    tex: TextureHandle,
    bytes: usize,
    used: u64,
}

const MAX_PENDING: usize = 400;
/// GPU memory kept for thumbnails before the least recently drawn ones are dropped.
const TEXTURE_BUDGET: usize = 256 << 20;
/// Size the disk cache is trimmed to at startup, oldest files first.
const DISK_BUDGET: u64 = 512 << 20;

pub struct Thumbs {
    queue: Arc<Queue>,
    rx: Receiver<Res>,
    cache: HashMap<Key, Entry>,
    bytes: usize,
    frame: u64,
    requested: HashSet<Key>,
    failed: HashSet<Key>,
}

impl Thumbs {
    /// `cache_dir` is where finished thumbnails are kept between runs (`None`: memory only).
    pub fn new(ctx: &egui::Context, cache_dir: Option<PathBuf>) -> Self {
        let queue = Arc::new(Queue {
            stack: Mutex::new(Vec::new()),
            cv: Condvar::new(),
        });
        // a format version in the path, so a future change starts a fresh cache
        let cache_dir = cache_dir.map(|d| d.join("v1"));
        if let Some(dir) = cache_dir.clone() {
            std::thread::spawn(move || trim_disk_cache(&dir, DISK_BUDGET));
        }
        let (tx, rx) = channel();
        let workers = std::thread::available_parallelism()
            .map(|n| n.get().clamp(2, 6))
            .unwrap_or(2);
        for _ in 0..workers {
            let q = queue.clone();
            let tx: Sender<Res> = tx.clone();
            let ctx = ctx.clone();
            let cache_dir = cache_dir.clone();
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
                let res = match load(&job, cache_dir.as_deref()) {
                    Some(img) => Res::Ok(job.key, to_color_image(&img)),
                    None => Res::Failed(job.key),
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
            bytes: 0,
            frame: 0,
            requested: HashSet::new(),
            failed: HashSet::new(),
        }
    }

    /// Moves finished thumbnails into GPU textures. Call once per frame.
    pub fn poll(&mut self, ctx: &egui::Context) {
        self.frame += 1;
        while let Ok(r) = self.rx.try_recv() {
            match r {
                Res::Ok(key, img) => {
                    self.requested.remove(&key);
                    let bytes = img.width() * img.height() * 4;
                    let tex = ctx.load_texture(
                        format!("thumb-{}-{}", key.id, key.size),
                        img,
                        TextureOptions::LINEAR,
                    );
                    self.bytes += bytes;
                    let old = self.cache.insert(
                        key,
                        Entry {
                            tex,
                            bytes,
                            used: self.frame,
                        },
                    );
                    if let Some(old) = old {
                        self.bytes -= old.bytes;
                    }
                }
                Res::Failed(key) => {
                    self.requested.remove(&key);
                    self.failed.insert(key);
                }
            }
        }
        if self.bytes > TEXTURE_BUDGET {
            self.evict(TEXTURE_BUDGET * 3 / 4);
        }
    }

    /// Drops the least recently drawn textures until at most `target` bytes are left.
    fn evict(&mut self, target: usize) {
        let mut by_age: Vec<(u64, Key)> = self.cache.iter().map(|(k, e)| (e.used, *k)).collect();
        by_age.sort_unstable_by_key(|(used, _)| *used);
        for (used, key) in by_age {
            // never what is on screen right now
            if self.bytes <= target || used + 1 >= self.frame {
                break;
            }
            if let Some(e) = self.cache.remove(&key) {
                self.bytes -= e.bytes;
            }
        }
    }

    /// Returns the texture if ready, otherwise queues it.
    pub fn get(&mut self, f: &crate::db::FileRow, size: u32) -> Option<&TextureHandle> {
        let key = Key { id: f.id, size };
        if let Some(e) = self.cache.get_mut(&key) {
            e.used = self.frame;
            return Some(&e.tex);
        }
        if self.failed.contains(&key) || self.requested.contains(&key) {
            return None;
        }
        self.requested.insert(key);
        let mut st = self.queue.stack.lock().unwrap();
        st.push(Job {
            key,
            path: PathBuf::from(&f.path),
            mtime: f.mtime,
            len: f.size,
        });
        if st.len() > MAX_PENDING {
            let old = st.remove(0);
            self.requested.remove(&old.key);
        }
        drop(st);
        self.queue.cv.notify_one();
        None
    }

    /// Forget a file's thumbnails (after it changed on disk).
    pub fn invalidate(&mut self, id: i64) {
        let gone: Vec<Key> = self.cache.keys().filter(|k| k.id == id).copied().collect();
        for k in gone {
            if let Some(e) = self.cache.remove(&k) {
                self.bytes -= e.bytes;
            }
        }
        self.failed.retain(|k| k.id != id);
    }
}

// ------------------------------------------------------------------ making one

/// From the disk cache when possible, otherwise decoded (and then cached).
fn load(job: &Job, cache_dir: Option<&Path>) -> Option<RgbaImage> {
    let cached = cache_dir.map(|d| d.join(cache_name(job)));
    if let Some(img) = cached.as_deref().and_then(read_cached) {
        return Some(img);
    }
    let img = make(&job.path, job.key.size)?;
    if let Some(c) = &cached {
        write_cached(c, &img);
    }
    Some(img)
}

pub fn make(path: &Path, size: u32) -> Option<RgbaImage> {
    let ext = crate::db::ext_of(path);
    if is_image(&ext) {
        let img = if matches!(ext.as_str(), "jpg" | "jpeg") {
            jpeg_thumb(path, size).or_else(|| image_thumb(path, size))
        } else {
            image_thumb(path, size)
        };
        if let Some(img) = img {
            return Some(img);
        }
    }
    crate::osthumb::thumbnail(path, size)
}

/// Any format the `image` crate reads, turned upright.
fn image_thumb(path: &Path, size: u32) -> Option<RgbaImage> {
    use image::ImageDecoder;
    let reader = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?;
    let mut decoder = reader.into_decoder().ok()?;
    let orientation = decoder.orientation().ok();
    let mut img = DynamicImage::from_decoder(decoder).ok()?;
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    Some(img.thumbnail(size, size).to_rgba8())
}

/// JPEG photos: the camera's embedded preview when it is big enough, otherwise
/// a decode at reduced scale (the decoder skips most of the work for 1/2–1/8).
fn jpeg_thumb(path: &Path, size: u32) -> Option<RgbaImage> {
    let mut file = BufReader::new(std::fs::File::open(path).ok()?);
    let exif = exif::Reader::new().read_from_container(&mut file).ok();
    let orientation = exif
        .as_ref()
        .and_then(|e| e.get_field(exif::Tag::Orientation, exif::In::PRIMARY))
        .and_then(|f| f.value.get_uint(0))
        .and_then(|v| image::metadata::Orientation::from_exif(v as u8));

    file.rewind().ok()?;
    let mut decoder = jpeg_decoder::Decoder::new(file);
    decoder.read_info().ok()?;
    let info = decoder.info()?;
    let (w, h) = (info.width as u32, info.height as u32);

    let mut img = match exif.as_ref().and_then(|e| embedded_preview(e, w, h, size)) {
        Some(img) => img,
        None => scaled_decode(&mut decoder, size)?,
    };
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    Some(img.thumbnail(size, size).to_rgba8())
}

/// Decodes at the smallest of 1/1, 1/2, 1/4, 1/8 scale that still covers `size`.
fn scaled_decode<R: std::io::Read>(
    decoder: &mut jpeg_decoder::Decoder<R>,
    size: u32,
) -> Option<DynamicImage> {
    let s = size.min(u16::MAX as u32) as u16;
    decoder.scale(s, s).ok()?;
    let pixels = decoder.decode().ok()?;
    let info = decoder.info()?;
    let (w, h) = (info.width as u32, info.height as u32);
    match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => {
            image::RgbImage::from_raw(w, h, pixels).map(DynamicImage::ImageRgb8)
        }
        jpeg_decoder::PixelFormat::L8 => {
            image::GrayImage::from_raw(w, h, pixels).map(DynamicImage::ImageLuma8)
        }
        // CMYK and 16-bit JPEGs are rare: the image crate handles them
        _ => None,
    }
}

/// The preview JPEG inside the EXIF block, if it covers `size` and has the
/// photo's proportions (some cameras pad it with black bars).
fn embedded_preview(exif: &exif::Exif, w: u32, h: u32, size: u32) -> Option<DynamicImage> {
    let field = |tag| {
        exif.get_field(tag, exif::In::THUMBNAIL)
            .and_then(|f| f.value.get_uint(0))
            .map(|v| v as usize)
    };
    let start = field(exif::Tag::JPEGInterchangeFormat)?;
    let len = field(exif::Tag::JPEGInterchangeFormatLength)?;
    let bytes = exif.buf().get(start..start.checked_add(len)?)?;
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg).ok()?;
    let (tw, th) = (img.width(), img.height());
    let fits = tw.max(th) >= size;
    let same_shape =
        w > 0 && h > 0 && ((tw as f32 / th as f32) - (w as f32 / h as f32)).abs() < 0.02;
    (fits && same_shape).then_some(img)
}

// ------------------------------------------------------------------ disk cache

/// FNV-1a: a hash that stays the same across runs and Rust versions.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |h, b| {
        (h ^ *b as u64).wrapping_mul(0x100000001b3)
    })
}

fn cache_name(job: &Job) -> PathBuf {
    let h = fnv1a(job.path.to_string_lossy().as_bytes());
    PathBuf::from(format!("{:02x}", h >> 56)).join(format!(
        "{h:016x}-{}-{}-{}",
        job.mtime, job.len, job.key.size
    ))
}

fn read_cached(path: &Path) -> Option<RgbaImage> {
    let reader = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?;
    Some(reader.decode().ok()?.to_rgba8())
}

/// Opaque thumbnails are stored as JPEG (small), others as PNG (keeps transparency).
fn write_cached(path: &Path, img: &RgbaImage) {
    let Some(dir) = path.parent() else { return };
    let _ = std::fs::create_dir_all(dir);
    let opaque = img.pixels().all(|p| p.0[3] == 255);
    let mut buf = Vec::new();
    let ok = if opaque {
        let rgb = DynamicImage::ImageRgba8(img.clone()).to_rgb8();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 88)
            .encode_image(&rgb)
            .is_ok()
    } else {
        img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .is_ok()
    };
    if !ok {
        return;
    }
    // write next to it, then rename, so a reader never sees half a file
    let tmp = path
        .with_extension(format!("tmp{:?}", std::thread::current().id()).replace(['(', ')'], ""));
    if std::fs::write(&tmp, &buf).is_ok() && std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Deletes the oldest cached thumbnails once the cache outgrows `budget`.
fn trim_disk_cache(dir: &Path, budget: u64) {
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = walkdir::WalkDir::new(dir)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| {
            let md = e.metadata().ok()?;
            Some((md.modified().ok()?, md.len(), e.into_path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    if total <= budget {
        return;
    }
    files.sort_unstable_by_key(|f| f.0);
    for (_, len, path) in files {
        if total <= budget * 3 / 4 {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total -= len;
        }
    }
}

pub fn to_color_image(img: &RgbaImage) -> ColorImage {
    let (w, h) = img.dimensions();
    ColorImage::from_rgba_unmultiplied([w as usize, h as usize], img.as_raw())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    fn jpeg_bytes(img: &RgbImage) -> Vec<u8> {
        let mut buf = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 95)
            .encode_image(img)
            .unwrap();
        buf
    }

    /// A JPEG with an EXIF block (orientation, optional embedded preview) after SOI.
    fn photo(main: &RgbImage, orientation: u16, preview: Option<&RgbImage>) -> Vec<u8> {
        let jpeg = jpeg_bytes(main);
        let preview = preview.map(jpeg_bytes);
        let field = exif::Field {
            tag: exif::Tag::Orientation,
            ifd_num: exif::In::PRIMARY,
            value: exif::Value::Short(vec![orientation]),
        };
        let mut w = exif::experimental::Writer::new();
        w.push_field(&field);
        if let Some(p) = &preview {
            w.set_jpeg(p, exif::In::THUMBNAIL);
        }
        let mut tiff = std::io::Cursor::new(Vec::new());
        w.write(&mut tiff, false).unwrap();
        let tiff = tiff.into_inner();
        let mut out = jpeg[..2].to_vec(); // SOI
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&((2 + 6 + tiff.len()) as u16).to_be_bytes());
        out.extend_from_slice(b"Exif\0\0");
        out.extend_from_slice(&tiff);
        out.extend_from_slice(&jpeg[2..]);
        out
    }

    fn halves(w: u32, h: u32, left: [u8; 3], right: [u8; 3]) -> RgbImage {
        RgbImage::from_fn(w, h, |x, _| Rgb(if x < w / 2 { left } else { right }))
    }

    fn near(p: &image::Rgba<u8>, c: [u8; 3]) -> bool {
        (0..3).all(|i| (p.0[i] as i32 - c[i] as i32).abs() < 40)
    }

    const RED: [u8; 3] = [220, 20, 20];
    const BLUE: [u8; 3] = [20, 20, 220];
    const GREEN: [u8; 3] = [20, 200, 20];

    #[test]
    fn jpeg_thumbnail_is_scaled_and_upright() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("photo.jpg");
        // 6 = stored sideways, shown rotated 90° clockwise: the left half ends up on top
        std::fs::write(&p, photo(&halves(1600, 800, RED, BLUE), 6, None)).unwrap();
        let t = make(&p, 64).unwrap();
        assert_eq!(t.dimensions(), (32, 64));
        assert!(near(t.get_pixel(16, 4), RED));
        assert!(near(t.get_pixel(16, 60), BLUE));
    }

    #[test]
    fn jpeg_uses_embedded_preview_when_big_enough() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("camera.jpg");
        let main = halves(800, 400, RED, RED);
        let preview = halves(160, 80, GREEN, GREEN);
        std::fs::write(&p, photo(&main, 1, Some(&preview))).unwrap();
        // small thumbnails come from the preview ...
        let small = make(&p, 128).unwrap();
        assert_eq!(small.dimensions(), (128, 64));
        assert!(near(small.get_pixel(64, 32), GREEN));
        // ... larger ones from the photo itself
        let big = make(&p, 256).unwrap();
        assert_eq!(big.dimensions(), (256, 128));
        assert!(near(big.get_pixel(128, 64), RED));
    }

    #[test]
    fn padded_preview_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("padded.jpg");
        // a 3:2 photo whose 4:3 preview has black bars
        let main = halves(600, 400, RED, RED);
        let preview = halves(160, 120, GREEN, GREEN);
        std::fs::write(&p, photo(&main, 1, Some(&preview))).unwrap();
        let t = make(&p, 64).unwrap();
        assert!(near(t.get_pixel(32, 20), RED));
    }

    #[test]
    fn disk_cache_serves_later_requests() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache");
        let p = dir.path().join("a.png");
        image::RgbaImage::from_pixel(300, 150, image::Rgba([10, 20, 30, 128]))
            .save(&p)
            .unwrap();
        let job = Job {
            key: Key { id: 1, size: 64 },
            path: p.clone(),
            mtime: 1,
            len: 2,
        };
        let first = load(&job, Some(&cache)).unwrap();
        assert_eq!(first.dimensions(), (64, 32));
        // the source is gone, yet the cached copy (with its transparency) is served
        std::fs::remove_file(&p).unwrap();
        let again = load(&job, Some(&cache)).unwrap();
        assert_eq!(again, first);
        // a changed file (other mtime) is not answered from the cache
        let changed = Job { mtime: 3, ..job };
        assert!(load(&changed, Some(&cache)).is_none());
    }

    #[test]
    fn disk_cache_is_trimmed_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..10 {
            std::fs::write(dir.path().join(format!("f{i}")), vec![0u8; 1000]).unwrap();
            let t = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1000 + i);
            std::fs::File::options()
                .write(true)
                .open(dir.path().join(format!("f{i}")))
                .unwrap()
                .set_modified(t)
                .unwrap();
        }
        trim_disk_cache(dir.path(), 8000);
        let mut left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        left.sort();
        assert_eq!(left, vec!["f4", "f5", "f6", "f7", "f8", "f9"]);
    }
}
