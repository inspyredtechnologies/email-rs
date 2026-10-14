//! Preparing untrusted HTML mail for display in the reading pane's web view.
//!
//! Defense in depth: JavaScript is disabled in the web view itself, and every
//! message also gets a Content-Security-Policy that blocks scripts, frames,
//! forms and (until the user opts in) all remote content such as tracking pixels.

/// Base styles roughly matching Outlook's reading pane.
const BASE_CSS: &str = "html{color-scheme:light}\
body{font-family:'Segoe UI',Calibri,Arial,sans-serif;font-size:14px;color:#262626;\
margin:14px 18px;overflow-wrap:break-word;background:#fff}\
img{max-width:100%;height:auto}";

fn csp(allow_remote: bool) -> String {
    let remote = if allow_remote { " http: https:" } else { "" };
    format!(
        "default-src 'none'; img-src data:{remote}; style-src 'unsafe-inline' data:{remote}; \
         font-src data:{remote}; base-uri 'none'; form-action 'none'"
    )
}

/// True if the message would load images or styles from the internet.
pub fn has_remote_content(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    ["src=\"http", "src='http", "src=http", "url(http", "url('http", "url(\"http", "background=\"http"]
        .iter()
        .any(|p| lower.contains(p))
}

/// Remove `<meta http-equiv="refresh">`, which could navigate the view on its own.
fn strip_meta_refresh(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut pos = 0;
    while let Some(start) = lower[pos..].find("<meta").map(|i| pos + i) {
        let end = lower[start..].find('>').map(|i| start + i + 1).unwrap_or(html.len());
        out.push_str(&html[pos..start]);
        if !lower[start..end].contains("refresh") {
            out.push_str(&html[start..end]);
        }
        pos = end;
    }
    out.push_str(&html[pos..]);
    out
}

/// Wrap message HTML with our charset, security policy and base styles.
pub fn prepare(html: &str, allow_remote: bool) -> String {
    let html = strip_meta_refresh(html);
    let head = format!(
        "<meta charset=\"utf-8\"><meta http-equiv=\"Content-Security-Policy\" content=\"{}\"><style>{BASE_CSS}</style>",
        csp(allow_remote)
    );
    // The policy must come before anything that loads resources, so put it right after <head>.
    let lower = html.to_ascii_lowercase();
    if let Some(open) = lower.find("<head") {
        if let Some(close) = lower[open..].find('>') {
            let at = open + close + 1;
            return format!("{}{head}{}", &html[..at], &html[at..]);
        }
    }
    format!("<!doctype html><html><head>{head}</head><body>{html}</body></html>")
}

/// Replace `cid:` references with inline `data:` URIs. Returns the content IDs used.
pub fn inline_cid_images(html: &mut String, parts: &[(String, String, Vec<u8>)]) -> Vec<String> {
    use base64::Engine;
    let mut used = Vec::new();
    for (cid, mime, data) in parts {
        let needle = format!("cid:{cid}");
        if html.contains(&needle) {
            let uri = format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(data));
            *html = html.replace(&needle, &uri);
            used.push(cid.clone());
        }
    }
    used
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injects_policy_after_head() {
        let out = prepare("<html><HEAD><title>x</title></HEAD><body>hi</body></html>", false);
        let csp_at = out.find("Content-Security-Policy").unwrap();
        assert!(csp_at > out.find("<HEAD>").unwrap() && csp_at < out.find("<title>").unwrap());
        assert!(out.contains("img-src data:;"));
        assert!(prepare("<p>hi</p>", true).contains("img-src data: http: https:;"));
        assert!(prepare("<p>hi</p>", false).starts_with("<!doctype html><html><head><meta charset"));
    }

    #[test]
    fn strips_meta_refresh_only() {
        let out = prepare("<head><META http-equiv=\"refresh\" content=\"0;url=http://x\"><meta name=a></head>", false);
        assert!(!out.to_lowercase().contains("refresh"));
        assert!(out.contains("<meta name=a>"));
    }

    #[test]
    fn detects_remote_content() {
        assert!(has_remote_content("<img SRC=\"https://t.example/p.gif\">"));
        assert!(has_remote_content("<td style=\"background:url(http://x/y.png)\">"));
        assert!(!has_remote_content("<img src=\"cid:logo\"><a href=\"https://x\">link</a>"));
    }

    #[test]
    fn inlines_cid_images() {
        let mut html = "<img src=\"cid:logo@x\">".to_string();
        let used = inline_cid_images(&mut html, &[("logo@x".into(), "image/png".into(), vec![1, 2, 3])]);
        assert_eq!(html, "<img src=\"data:image/png;base64,AQID\">");
        assert_eq!(used, vec!["logo@x".to_string()]);
    }
}
