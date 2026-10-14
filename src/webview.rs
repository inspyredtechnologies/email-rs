//! A WebView2 child window laid over the reading pane to render HTML mail.
//!
//! Content is served from a custom `emailrs://` protocol (no size limit, no
//! network), JavaScript is disabled, and any navigation away from the message
//! is cancelled: web links open in the default browser, `mailto:` links are
//! handed back to the app to open a compose window.

use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use eframe::egui;
use wry::dpi::{PhysicalPosition, PhysicalSize};
use wry::http::{Response, header::CONTENT_TYPE};
use wry::{NewWindowResponse, Rect, WebContext, WebView, WebViewBuilder};

/// On Windows, wry maps custom protocol `emailrs` to this origin.
const ORIGIN: &str = "http://emailrs.localhost/";

pub struct HtmlView {
    webview: WebView,
    content: Arc<Mutex<Vec<u8>>>,
    mailto: Arc<Mutex<Vec<String>>>,
    shown_key: Option<String>,
    loads: u64,
    visible: bool,
    bounds: Option<egui::Rect>,
    _context: WebContext,
}

fn open_link(url: &str, mailto: &Mutex<Vec<String>>, ctx: &egui::Context) {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("mailto:") {
        mailto.lock().unwrap().push(url.to_string());
        ctx.request_repaint();
    } else if lower.starts_with("http://") || lower.starts_with("https://") {
        let _ = open::that_detached(url);
    }
}

impl HtmlView {
    pub fn new(frame: &eframe::Frame, ctx: &egui::Context) -> Result<Self, String> {
        let content = Arc::new(Mutex::new(Vec::new()));
        let mailto = Arc::new(Mutex::new(Vec::new()));
        let mut context = WebContext::new(Some(crate::store::data_dir().join("webview2")));

        let served = content.clone();
        let (nav_mailto, nav_ctx) = (mailto.clone(), ctx.clone());
        let (win_mailto, win_ctx) = (mailto.clone(), ctx.clone());
        let webview = WebViewBuilder::new_with_web_context(&mut context)
            .with_javascript_disabled()
            .with_devtools(false)
            .with_clipboard(true)
            .with_hotkeys_zoom(true)
            .with_focused(false)
            .with_visible(false)
            .with_background_color((255, 255, 255, 255))
            .with_custom_protocol("emailrs".into(), move |_id, request| {
                let body = if request.uri().path().starts_with("/msg/") { served.lock().unwrap().clone() } else { Vec::new() };
                let status = if body.is_empty() { 404 } else { 200 };
                Response::builder()
                    .status(status)
                    .header(CONTENT_TYPE, "text/html; charset=utf-8")
                    .body(Cow::Owned(body))
                    .unwrap()
            })
            .with_navigation_handler(move |url| {
                if url.starts_with(ORIGIN) || url == "about:blank" {
                    return true;
                }
                open_link(&url, &nav_mailto, &nav_ctx);
                false
            })
            .with_new_window_req_handler(move |url, _features| {
                open_link(&url, &win_mailto, &win_ctx);
                NewWindowResponse::Deny
            })
            .build_as_child(frame)
            .map_err(|e| format!("HTML view unavailable (is the WebView2 runtime installed?): {e}"))?;

        Ok(Self { webview, content, mailto, shown_key: None, loads: 0, visible: false, bounds: None, _context: context })
    }

    /// Show `html` (produced lazily, only when `key` changes) over `rect` (egui points).
    pub fn show(&mut self, key: &str, html: impl FnOnce() -> String, rect: egui::Rect, pixels_per_point: f32) {
        if self.shown_key.as_deref() != Some(key) {
            *self.content.lock().unwrap() = html().into_bytes();
            self.loads += 1;
            // A fresh URL per message so the view never shows a cached page.
            let _ = self.webview.load_url(&format!("{ORIGIN}msg/{}", self.loads));
            self.shown_key = Some(key.to_string());
        }
        if self.bounds != Some(rect) {
            let ppp = pixels_per_point as f64;
            let _ = self.webview.set_bounds(Rect {
                position: PhysicalPosition::new((rect.min.x as f64 * ppp).round() as i32, (rect.min.y as f64 * ppp).round() as i32).into(),
                size: PhysicalSize::new((rect.width() as f64 * ppp).round().max(1.0) as u32, (rect.height() as f64 * ppp).round().max(1.0) as u32).into(),
            });
            self.bounds = Some(rect);
        }
        if !self.visible {
            let _ = self.webview.set_visible(true);
            self.visible = true;
        }
    }

    pub fn hide(&mut self) {
        if self.visible {
            let _ = self.webview.set_visible(false);
            self.visible = false;
        }
    }

    /// `mailto:` links the user clicked since the last call.
    pub fn take_mailto(&self) -> Vec<String> {
        std::mem::take(&mut *self.mailto.lock().unwrap())
    }
}
