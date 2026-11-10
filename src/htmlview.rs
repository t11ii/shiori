//! Live preview of HTML files: an OS web view laid over the preview area of
//! the detail panel.
//!
//! * macOS: WKWebView, the engine Safari uses.
//! * Windows: WebView2, the engine Edge uses (part of Windows 11 and of
//!   up-to-date Windows 10). Without it the panel falls back to the file icon.
//!
//! The page is shown as a document rather than run as an app: JavaScript is
//! off, nothing is stored, and the view cannot leave the folder of the file it
//! shows, so links to websites do nothing. "Open" is the way to the browser.
//!
//! A native view always draws on top of egui, so the app hides it whenever a
//! menu or dialog could overlap it (see [`HtmlView::update`]).

use std::path::Path;
use unicode_normalization::UnicodeNormalization;

const EXTS: &[&str] = &["html", "htm", "xhtml"];

pub fn supports(ext: &str) -> bool {
    cfg!(any(target_os = "macos", windows)) && EXTS.contains(&ext)
}

/// Where the page goes, in physical pixels from the top-left of the window's
/// content area. `window` is only compared: on macOS a child view has to be
/// placed again when the window is resized.
#[derive(Clone, Copy, PartialEq, Debug)]
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
pub struct Placement {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub window: [u32; 2],
    pub zoom: f32,
}

/// `file://` URL of a path. `windows` selects drive-letter and UNC handling.
#[cfg_attr(not(any(target_os = "macos", windows, test)), allow(dead_code))]
fn file_url_from(path: &str, windows: bool) -> String {
    let mut out = String::from("file://");
    let path = if windows {
        let p = path
            .strip_prefix(r"\\?\")
            .unwrap_or(path)
            .replace('\\', "/");
        match p.strip_prefix("//") {
            Some(unc) => unc.to_string(), // \\server\share\x -> file://server/share/x
            None => format!("/{p}"),      // C:\x -> file:///C:/x
        }
    } else {
        path.to_string()
    };
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// A URL reduced to something comparable: no query or fragment, escapes
/// decoded, one spelling per character, and lower-cased on Windows where
/// paths ignore case. Engines write the same path differently from us, so
/// URLs are compared in this form.
#[cfg_attr(not(any(target_os = "macos", windows, test)), allow(dead_code))]
fn url_key(url: &str, windows: bool) -> String {
    let url = url.split(['#', '?']).next().unwrap_or("");
    let url = match url.strip_prefix("file://localhost/") {
        Some(rest) => format!("file:///{rest}"),
        None => url.to_string(),
    };
    let bytes = url.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
        match (bytes[i], bytes.get(i + 1), bytes.get(i + 2)) {
            (b'%', Some(&h), Some(&l)) if hex(h).is_some() && hex(l).is_some() => {
                out.push(hex(h).unwrap() * 16 + hex(l).unwrap());
                i += 3;
            }
            (b, ..) => {
                out.push(b);
                i += 1;
            }
        }
    }
    // macOS file names come in composed and decomposed spellings (が can be
    // one character or か + ゛); both name the same file.
    let s: String = String::from_utf8_lossy(&out).nfc().collect();
    if windows {
        s.to_lowercase()
    } else {
        s
    }
}

/// May the view go to `url` while it previews a file in the folder `base`
/// (a [`url_key`] ending in `/`)? Only pages in that folder, so that frames
/// and links of a saved page work but nothing is fetched from elsewhere.
#[cfg_attr(not(any(target_os = "macos", windows, test)), allow(dead_code))]
fn may_navigate(url: &str, base: &str, windows: bool) -> bool {
    if url.starts_with("about:") {
        return true;
    }
    let key = url_key(url, windows);
    !base.is_empty() && key.starts_with(base) && !key.split('/').any(|part| part == "..")
}

#[cfg(any(target_os = "macos", windows))]
mod imp {
    use super::*;
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;
    use wry::dpi::{PhysicalPosition, PhysicalSize};
    use wry::raw_window_handle::HasWindowHandle;
    use wry::{NewWindowResponse, Rect, WebContext, WebView, WebViewBuilder};

    /// What the navigation handler consults; shared with [`HtmlView`].
    #[derive(Default)]
    struct Gate {
        /// Folder the view may read from, as a [`url_key`] ending in `/`.
        base: String,
        /// We have just started loading a file ourselves: let that one through
        /// however the engine spells its URL.
        loading: bool,
    }

    pub struct HtmlView {
        // `view` is declared first so it is dropped before its context
        view: Option<WebView>,
        context: WebContext,
        /// Creating the view failed (no WebView2 runtime, say); don't retry every frame.
        failed: bool,
        gate: Rc<RefCell<Gate>>,
        /// File the view was last told to load, and whether that worked.
        shown: Option<(PathBuf, bool)>,
        placed: Option<Placement>,
        visible: bool,
    }

    fn rect(p: &Placement) -> Rect {
        Rect {
            position: PhysicalPosition::new(p.x, p.y).into(),
            size: PhysicalSize::new(p.width, p.height).into(),
        }
    }

    /// Starts loading a local page that may read the files in `dir`.
    #[cfg(target_os = "macos")]
    fn load(view: &WebView, file: &str, dir: &str) -> bool {
        use objc2_foundation::{NSString, NSURL};
        use wry::WebViewExtMacOS;
        let file = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(file), false);
        let dir = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(dir), true);
        // WebKit's own call for local files; it also gives the page read access
        // to `dir`, which holds its images and style sheets.
        // Safety: both URLs are valid and we are on the main thread, where the view lives.
        unsafe {
            view.webview()
                .loadFileURL_allowingReadAccessToURL(&file, &dir)
        }
        .is_some()
    }

    #[cfg(windows)]
    fn load(view: &WebView, file: &str, _dir: &str) -> bool {
        view.load_url(&file_url_from(file, true)).is_ok()
    }

    impl HtmlView {
        /// `data_dir` is where WebView2 keeps its profile on Windows (unused on macOS).
        pub fn new(data_dir: PathBuf) -> Self {
            Self {
                view: None,
                context: WebContext::new(Some(data_dir)),
                failed: false,
                gate: Rc::default(),
                shown: None,
                placed: None,
                visible: false,
            }
        }

        /// False once we know this machine cannot show pages.
        pub fn available(&self) -> bool {
            !self.failed
        }

        fn create<W: HasWindowHandle>(&mut self, window: &W, at: &Placement) {
            let gate = self.gate.clone();
            let built = WebViewBuilder::new_with_web_context(&mut self.context)
                .with_bounds(rect(at))
                .with_visible(false)
                .with_focused(false)
                .with_incognito(true)
                .with_javascript_disabled()
                .with_autoplay(false)
                .with_devtools(false)
                .with_hotkeys_zoom(false)
                .with_back_forward_navigation_gestures(false)
                .with_navigation_handler(move |url| {
                    let mut gate = gate.borrow_mut();
                    if gate.loading && url.starts_with("file:") {
                        gate.loading = false;
                        return true;
                    }
                    may_navigate(&url, &gate.base, cfg!(windows))
                })
                .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
                .with_download_started_handler(|_, _| false)
                // a file dropped on the page must not replace it
                .with_drag_drop_handler(|_| true)
                .build_as_child(window);
            match built {
                Ok(v) => self.view = Some(v),
                Err(_) => self.failed = true,
            }
        }

        /// Points the view at `path`; false when that cannot be done.
        fn open(&self, view: &WebView, path: &Path) -> bool {
            let (Some(file), Some(dir)) = (path.to_str(), path.parent().and_then(Path::to_str))
            else {
                return false;
            };
            let mut base = url_key(&file_url_from(dir, cfg!(windows)), cfg!(windows));
            if !base.ends_with('/') {
                base.push('/');
            }
            *self.gate.borrow_mut() = Gate {
                base,
                loading: true,
            };
            load(view, file, dir)
        }

        /// Call once per frame. `want` is the file to show and where; `covered`
        /// says that a menu, dialog or drag is on screen, which the page would
        /// otherwise paint over.
        pub fn update<W: HasWindowHandle>(
            &mut self,
            window: &W,
            want: Option<(&Path, Placement)>,
            covered: bool,
        ) {
            let Some((path, at)) = want else {
                self.hide();
                if self.shown.take().is_some() {
                    // stop whatever the page was doing while nobody looks at it
                    if let Some(v) = &self.view {
                        let _ = v.load_url("about:blank");
                    }
                }
                return;
            };
            if self.view.is_none() && !self.failed {
                self.create(window, &at);
            }
            let Some(view) = &self.view else { return };

            if self.shown.as_ref().map(|(p, _)| p.as_path()) != Some(path) {
                let ok = self.open(view, path);
                self.shown = Some((path.to_path_buf(), ok));
            }
            let loaded = self.shown.as_ref().is_some_and(|(_, ok)| *ok);
            if covered || !loaded {
                self.hide();
                return;
            }
            if self.placed != Some(at) {
                let _ = view.set_bounds(rect(&at));
                if self.placed.map(|p| p.zoom) != Some(at.zoom) {
                    let _ = view.zoom(at.zoom as f64);
                }
                self.placed = Some(at);
            }
            if !self.visible {
                let _ = view.set_visible(true);
                self.visible = true;
            }
        }

        fn hide(&mut self) {
            if self.visible {
                if let Some(v) = &self.view {
                    let _ = v.set_visible(false);
                }
                self.visible = false;
            }
        }

        /// Hand the keyboard back to the app after the user clicked in the page.
        pub fn release_focus(&self) {
            if self.visible {
                if let Some(v) = &self.view {
                    let _ = v.focus_parent();
                }
            }
        }
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod imp {
    use super::*;
    use std::path::PathBuf;

    pub struct HtmlView;

    impl HtmlView {
        pub fn new(_data_dir: PathBuf) -> Self {
            Self
        }

        pub fn available(&self) -> bool {
            false
        }

        pub fn update<W>(
            &mut self,
            _window: &W,
            _want: Option<(&Path, Placement)>,
            _covered: bool,
        ) {
        }

        pub fn release_focus(&self) {}
    }
}

pub use imp::HtmlView;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_urls() {
        assert_eq!(
            file_url_from("/Users/t/My Docs/旅行.html", false),
            "file:///Users/t/My%20Docs/%E6%97%85%E8%A1%8C.html"
        );
        assert_eq!(
            file_url_from(r"C:\Users\t\a b#1.html", true),
            "file:///C:/Users/t/a%20b%231.html"
        );
        assert_eq!(
            file_url_from(r"\\nas\share\p.html", true),
            "file://nas/share/p.html"
        );
        assert_eq!(file_url_from(r"\\?\C:\x.html", true), "file:///C:/x.html");
    }

    #[test]
    fn url_keys_ignore_escaping_and_fragments() {
        let ours = file_url_from("/a/My Docs/(1) 旅行.html", false);
        // the same file as an engine might report it back
        let theirs = "file:///a/My%20Docs/(1)%20%e6%97%85%e8%a1%8c.html#top";
        assert_eq!(url_key(&ours, false), url_key(theirs, false));
        // が as one character and as か followed by a combining mark
        assert_eq!(
            url_key(&file_url_from("/a/\u{304c}.html", false), false),
            url_key(&file_url_from("/a/\u{304b}\u{3099}.html", false), false)
        );
        assert_eq!(
            url_key("file://localhost/a/b.html?x=1", false),
            "file:///a/b.html"
        );
        assert_eq!(
            url_key("file:///C:/Users/T/A.HTML", true),
            "file:///c:/users/t/a.html"
        );
        // a stray percent sign is kept as it is
        assert_eq!(url_key("file:///a/100%.html", false), "file:///a/100%.html");
    }

    #[test]
    fn navigation_stays_in_the_folder() {
        let base = url_key(&file_url_from("/a/site", false), false) + "/";
        let ok = |u: &str| may_navigate(u, &base, false);
        assert!(ok("file:///a/site/index.html"));
        assert!(ok("file:///a/site/sub/page%202.html#x"));
        assert!(ok("about:blank"));
        assert!(!ok("file:///a/site2/index.html"));
        assert!(!ok("file:///a/secret.html"));
        assert!(!ok("https://example.com/"));
        assert!(!ok("http://localhost/a/site/"));
        assert!(!ok("javascript:alert(1)"));
        assert!(!ok("file:///a/site/../secret.html"));
        assert!(!ok("file:///a/site/%2e%2e/secret.html"));
        // nothing is allowed before a file has been chosen
        assert!(!may_navigate("file:///a/site/index.html", "", false));

        let win = url_key(&file_url_from(r"C:\Docs\Site", true), true) + "/";
        assert!(may_navigate("file:///c:/docs/site/Index.html", &win, true));
        assert!(!may_navigate("file:///C:/Docs/Other/x.html", &win, true));
    }
}
