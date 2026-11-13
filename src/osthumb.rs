//! Thumbnails rendered by the operating system, for files we cannot decode
//! ourselves: PDF pages, videos, Office documents, HEIC photos and so on.
//!
//! * macOS: Quick Look (`qlmanage -t`), the same previews Finder shows. It also
//!   renders web pages (HTML).
//! * Windows: the Shell thumbnail cache (`IShellItemImageFactory`), the same
//!   previews Explorer shows. Formats depend on installed thumbnail handlers.

use image::RgbaImage;
use std::path::Path;

/// Extensions worth asking the OS about. Everything else falls back to an icon
/// without spending a process launch or a COM call.
const EXTS: &[&str] = &[
    // documents
    "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "rtf", "pages", "numbers", "key", "odt",
    "ods", "odp", "epub", // images the image crate does not read
    "heic", "heif", "avif", "psd", "svg", "ai", "eps", "dng", "cr2", "cr3", "nef", "arw", "raf",
    "orf", // video
    "mp4", "mov", "m4v", "avi", "mkv", "wmv", "webm", "mpg", "mpeg", "3gp",
];

/// Web pages: Quick Look renders them, Explorer has no thumbnails for them.
const MAC_ONLY_EXTS: &[&str] = &["html", "htm"];

pub fn supports(ext: &str) -> bool {
    (cfg!(any(target_os = "macos", windows)) && EXTS.contains(&ext))
        || (cfg!(target_os = "macos") && MAC_ONLY_EXTS.contains(&ext))
}

pub fn thumbnail(path: &Path, size: u32) -> Option<RgbaImage> {
    if !supports(&crate::db::ext_of(path)) {
        return None;
    }
    imp::thumbnail(path, size)
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    pub fn thumbnail(path: &Path, size: u32) -> Option<RgbaImage> {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let out = std::env::temp_dir().join(format!("shiori-ql-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&out).ok()?;
        let result = run(path, size, &out);
        let _ = std::fs::remove_dir_all(&out);
        result
    }

    fn run(path: &Path, size: u32, out: &Path) -> Option<RgbaImage> {
        let mut child = Command::new("/usr/bin/qlmanage")
            .arg("-t")
            .arg("-s")
            .arg(size.to_string())
            .arg("-o")
            .arg(out)
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        // Quick Look plugins can hang on broken files; don't let one block a worker.
        let start = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if start.elapsed() > Duration::from_secs(15) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(_) => return None,
            }
        }
        // qlmanage writes "<file name>.png" into the output folder
        let png = std::fs::read_dir(out)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")))?;
        let img = image::open(png).ok()?;
        Some(img.thumbnail(size, size).to_rgba8())
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::ffi::c_void;
    use windows::core::HSTRING;
    use windows::Win32::Foundation::SIZE;
    use windows::Win32::Graphics::Gdi::{
        DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP,
    };
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
    use windows::Win32::UI::Shell::{
        IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK,
        SIIGBF_THUMBNAILONLY,
    };

    thread_local! {
        // COM must be initialised once on every worker thread that uses the Shell.
        static COM: () = unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        };
    }

    pub fn thumbnail(path: &Path, size: u32) -> Option<RgbaImage> {
        COM.with(|_| ());
        let s = size as i32;
        unsafe {
            let factory: IShellItemImageFactory =
                SHCreateItemFromParsingName(&HSTRING::from(path), None).ok()?;
            let hbmp = factory
                .GetImage(
                    SIZE { cx: s, cy: s },
                    SIIGBF_THUMBNAILONLY | SIIGBF_BIGGERSIZEOK,
                )
                .ok()?;
            let img = bitmap_to_image(hbmp);
            let _ = DeleteObject(hbmp.into());
            let img = image::DynamicImage::ImageRgba8(img?);
            Some(img.thumbnail(size, size).to_rgba8())
        }
    }

    unsafe fn bitmap_to_image(hbmp: HBITMAP) -> Option<image::RgbaImage> {
        let mut bm = BITMAP::default();
        let got = GetObjectW(
            hbmp.into(),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bm as *mut BITMAP as *mut c_void),
        );
        if got == 0 || bm.bmWidth <= 0 || bm.bmHeight == 0 {
            return None;
        }
        let (w, h) = (bm.bmWidth, bm.bmHeight.abs());
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down rows
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let hdc = GetDC(None);
        let lines = GetDIBits(
            hdc,
            hbmp,
            0,
            h as u32,
            Some(buf.as_mut_ptr() as *mut c_void),
            &mut info,
            DIB_RGB_COLORS,
        );
        ReleaseDC(None, hdc);
        if lines == 0 {
            return None;
        }
        // BGRA -> RGBA. Many thumbnails leave alpha at 0 even though they are opaque.
        let opaque = buf.chunks_exact(4).all(|p| p[3] == 0);
        for p in buf.chunks_exact_mut(4) {
            p.swap(0, 2);
            if opaque {
                p[3] = 255;
            }
        }
        let img = image::RgbaImage::from_raw(w as u32, h as u32, buf)?;
        Some(img)
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod imp {
    use super::*;

    pub fn thumbnail(_path: &Path, _size: u32) -> Option<RgbaImage> {
        None
    }
}
