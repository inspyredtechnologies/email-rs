//! OAuth 2.0 sign-in for Microsoft (Outlook.com, Microsoft 365 / Exchange Online)
//! and Google, used for IMAP/SMTP via XOAUTH2.
//!
//! Desktop flow: authorization code + PKCE with a loopback redirect. The user
//! signs in in their browser; we never see their password. The refresh token is
//! stored in Windows Credential Manager in place of a password.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::model::AuthMethod;

struct Provider {
    authorize_url: &'static str,
    token_url: &'static str,
    scopes: &'static str,
    extra_params: &'static [(&'static str, &'static str)],
}

fn provider(method: AuthMethod) -> Option<Provider> {
    match method {
        AuthMethod::Password => None,
        // The "common" tenant accepts both personal Microsoft accounts and work/school (Entra ID) accounts.
        AuthMethod::Microsoft => Some(Provider {
            authorize_url: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
            token_url: "https://login.microsoftonline.com/common/oauth2/v2.0/token",
            scopes: "https://outlook.office.com/IMAP.AccessAsUser.All https://outlook.office.com/SMTP.Send offline_access",
            extra_params: &[("prompt", "select_account")],
        }),
        AuthMethod::Google => Some(Provider {
            authorize_url: "https://accounts.google.com/o/oauth2/v2/auth",
            token_url: "https://oauth2.googleapis.com/token",
            scopes: "https://mail.google.com/",
            extra_params: &[("access_type", "offline"), ("prompt", "consent")],
        }),
    }
}

/// The app registration to sign in with. Each provider requires one; see README.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ClientConfig {
    pub client_id: String,
    /// Google "Desktop app" clients have a (non-confidential) secret; Microsoft public clients don't.
    #[serde(default)]
    pub client_secret: String,
}

#[derive(Default, Serialize, Deserialize)]
struct ClientFile {
    #[serde(default)]
    microsoft: ClientConfig,
    #[serde(default)]
    google: ClientConfig,
}

fn client_file_path() -> std::path::PathBuf {
    crate::store::data_dir().join("oauth.json")
}

fn read_client_file() -> ClientFile {
    std::fs::read(client_file_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Client registration: saved settings first, then IDs baked in at build time
/// (`OUTLOOK_RS_MS_CLIENT_ID`, `OUTLOOK_RS_GOOGLE_CLIENT_ID`, `OUTLOOK_RS_GOOGLE_CLIENT_SECRET`).
pub fn load_client(method: AuthMethod) -> ClientConfig {
    let file = read_client_file();
    let (saved, id, secret) = match method {
        AuthMethod::Microsoft => (file.microsoft, option_env!("OUTLOOK_RS_MS_CLIENT_ID"), None),
        AuthMethod::Google => {
            (file.google, option_env!("OUTLOOK_RS_GOOGLE_CLIENT_ID"), option_env!("OUTLOOK_RS_GOOGLE_CLIENT_SECRET"))
        }
        AuthMethod::Password => return ClientConfig::default(),
    };
    if !saved.client_id.trim().is_empty() {
        return saved;
    }
    ClientConfig { client_id: id.unwrap_or_default().into(), client_secret: secret.unwrap_or_default().into() }
}

pub fn save_client(method: AuthMethod, client: &ClientConfig) {
    let mut file = read_client_file();
    match method {
        AuthMethod::Microsoft => file.microsoft = client.clone(),
        AuthMethod::Google => file.google = client.clone(),
        AuthMethod::Password => return,
    }
    let path = client_file_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_vec_pretty(&file) {
        let _ = std::fs::write(path, json);
    }
}

#[derive(Clone, Debug)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Instant,
}

fn random_token(bytes: usize) -> Result<String, String> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|e| format!("No secure random source: {e}"))?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                match std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(v) => {
                        out.push(v);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Query parameters from the request line of an HTTP GET, e.g. `GET /?code=x&state=y HTTP/1.1`.
fn query_params(request: &str) -> Vec<(String, String)> {
    let target = request.lines().next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("");
    let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect()
}

fn authorize_url(p: &Provider, client: &ClientConfig, redirect: &str, state: &str, challenge: &str, login_hint: &str) -> String {
    let mut params: Vec<(&str, &str)> = vec![
        ("client_id", client.client_id.trim()),
        ("response_type", "code"),
        ("redirect_uri", redirect),
        ("scope", p.scopes),
        ("state", state),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
    ];
    if !login_hint.is_empty() {
        params.push(("login_hint", login_hint));
    }
    params.extend_from_slice(p.extra_params);
    let query: Vec<String> = params.iter().map(|(k, v)| format!("{k}={}", percent_encode(v))).collect();
    format!("{}?{}", p.authorize_url, query.join("&"))
}

const DONE_PAGE: &str = "<!doctype html><html><head><meta charset=utf-8><title>Outlook RS</title></head>\
<body style=\"font-family:Segoe UI,sans-serif;padding:48px;color:#262626\">\
<h2 style=\"color:#0072C6;font-weight:400\">You're signed in</h2>\
<p>You can close this tab and return to Outlook RS.</p></body></html>";

/// Wait on the loopback listener for the browser redirect carrying `code`.
fn wait_for_code(listener: &TcpListener, state: &str, cancel: &AtomicBool) -> Result<String, String> {
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("Sign-in was cancelled.".into());
        }
        if Instant::now() > deadline {
            return Err("Sign-in timed out. Try again.".into());
        }
        let mut stream = match listener.accept() {
            Ok((s, _)) => s,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(e) => return Err(e.to_string()),
        };
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut buf = vec![0u8; 16 * 1024];
        let n = stream.read(&mut buf).unwrap_or(0);
        let request = String::from_utf8_lossy(&buf[..n]).into_owned();
        let params = query_params(&request);
        let get = |k: &str| params.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());

        // Browsers also ask for /favicon.ico; only the redirect carries code or error.
        if get("code").is_none() && get("error").is_none() {
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            continue;
        }
        let (status, body) = match get("error") {
            Some(_) => ("400 Bad Request", "<p>Sign-in failed. Return to Outlook RS for details.</p>"),
            None => ("200 OK", DONE_PAGE),
        };
        let _ = write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        if let Some(err) = get("error") {
            let desc = get("error_description").unwrap_or_default();
            return Err(format!("Sign-in failed: {err}. {desc}").trim().to_string());
        }
        if get("state").as_deref() != Some(state) {
            return Err("Sign-in response didn't match this request. Try again.".into());
        }
        return get("code").ok_or_else(|| "No authorization code was returned.".into());
    }
}

fn token_request(p: &Provider, form: &[(&str, &str)]) -> Result<Tokens, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .into();
    let resp = agent.post(p.token_url).send_form(form.iter().copied()).map_err(|e| format!("Token request failed: {e}"))?;
    let ok = resp.status().is_success();
    let text = resp.into_body().read_to_string().map_err(|e| e.to_string())?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|_| format!("Unexpected token response: {text}"))?;
    if !ok {
        let err = json["error"].as_str().unwrap_or("error");
        let desc = json["error_description"].as_str().unwrap_or("");
        return Err(format!("Sign-in failed ({err}): {desc}"));
    }
    let access_token = json["access_token"].as_str().ok_or("The token response had no access token.")?.to_string();
    let expires_in = json["expires_in"].as_u64().unwrap_or(3600);
    Ok(Tokens {
        access_token,
        refresh_token: json["refresh_token"].as_str().map(str::to_string),
        expires_at: Instant::now() + Duration::from_secs(expires_in.saturating_sub(60)),
    })
}

/// Microsoft Graph scopes for the calendar (a separate token from IMAP/SMTP's).
pub const GRAPH_CALENDAR_SCOPES: &str = "https://graph.microsoft.com/Calendars.ReadWrite offline_access";

/// Microsoft Graph scopes for Graph-protocol accounts: mail and calendar with one sign-in.
pub const GRAPH_MAIL_SCOPES: &str = "https://graph.microsoft.com/Mail.ReadWrite https://graph.microsoft.com/Mail.Send \
    https://graph.microsoft.com/Calendars.ReadWrite offline_access";

/// The provider, with its default mail scopes unless `scopes` overrides them.
fn require_client(method: AuthMethod, client: &ClientConfig, scopes: Option<&'static str>) -> Result<Provider, String> {
    let mut p = provider(method).ok_or("This account doesn't use OAuth sign-in.")?;
    if let Some(s) = scopes {
        p.scopes = s;
    }
    if client.client_id.trim().is_empty() {
        return Err(match method {
            AuthMethod::Microsoft => "A Microsoft app registration (client ID) is needed. Register a free app at entra.microsoft.com \
                and paste its Application (client) ID under Server settings. See README for the steps.",
            _ => "A Google OAuth client ID is needed. Create a Desktop app client at console.cloud.google.com \
                and paste its client ID and secret under Server settings. See README for the steps.",
        }
        .into());
    }
    Ok(p)
}

/// Interactive sign-in: opens the browser and blocks until the redirect arrives
/// (or `cancel` is set). Returns tokens including a refresh token. `scopes`
/// overrides the provider's IMAP/SMTP scopes (e.g. `GRAPH_MAIL_SCOPES`).
pub fn sign_in_scoped(
    method: AuthMethod,
    client: &ClientConfig,
    login_hint: &str,
    cancel: Arc<AtomicBool>,
    scopes: Option<&'static str>,
) -> Result<Tokens, String> {
    let p = require_client(method, client, scopes)?;
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("Cannot start the sign-in listener: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect = format!("http://localhost:{port}");
    let verifier = random_token(48)?;
    let state = random_token(16)?;
    let url = authorize_url(&p, client, &redirect, &state, &pkce_challenge(&verifier), login_hint);
    open::that_detached(&url).map_err(|e| format!("Cannot open the browser: {e}"))?;

    let code = wait_for_code(&listener, &state, &cancel)?;
    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("client_id", client.client_id.trim()),
        ("code", code.as_str()),
        ("redirect_uri", redirect.as_str()),
        ("code_verifier", verifier.as_str()),
        ("scope", p.scopes),
    ];
    if !client.client_secret.trim().is_empty() {
        form.push(("client_secret", client.client_secret.trim()));
    }
    let tokens = token_request(&p, &form)?;
    if tokens.refresh_token.is_none() {
        return Err("The provider didn't return a refresh token, so you'd have to sign in every hour.".into());
    }
    Ok(tokens)
}

/// Exchange a stored refresh token for a fresh access token. Providers may rotate
/// the refresh token; callers should store `refresh_token` when it's `Some`.
/// Microsoft lets a refresh token mint tokens for another resource the user has
/// already consented to, so `scopes` may differ from the original sign-in's.
pub fn refresh_scoped(method: AuthMethod, client: &ClientConfig, refresh_token: &str, scopes: Option<&'static str>) -> Result<Tokens, String> {
    let p = require_client(method, client, scopes)?;
    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("client_id", client.client_id.trim()),
        ("refresh_token", refresh_token),
        ("scope", p.scopes),
    ];
    if !client.client_secret.trim().is_empty() {
        form.push(("client_secret", client.client_secret.trim()));
    }
    token_request(&p, &form)
}

/// SASL XOAUTH2 initial response (before base64, which the IMAP/SMTP libraries apply).
pub fn xoauth2(user: &str, access_token: &str) -> String {
    format!("user={user}\x01auth=Bearer {access_token}\x01\x01")
}

/// IMAP `AUTHENTICATE XOAUTH2` handler.
pub struct XOAuth2 {
    pub user: String,
    pub access_token: String,
}

impl imap::Authenticator for XOAuth2 {
    type Response = String;
    fn process(&self, challenge: &[u8]) -> String {
        // An empty challenge asks for credentials; a non-empty one is a JSON error
        // that must be acknowledged with an empty response before the server says NO.
        if challenge.is_empty() { xoauth2(&self.user, &self.access_token) } else { String::new() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_is_base64url_sha256() {
        // Expected value cross-checked with Python: urlsafe_b64encode(sha256(verifier)) without padding.
        assert_eq!(pkce_challenge("dBjftJeZ4CVP-mJ0kn3lX4sHp7z6t96NkZ74pGlODkU"), "GckQVkHDZnUlA7VmUv_6KHVElsWJIOA9TW23Q9ENmuI");
    }

    #[test]
    fn parses_redirect_query() {
        let req = "GET /?code=M.C5%2Fabc&state=xyz+1 HTTP/1.1\r\nHost: localhost\r\n\r\n";
        let params = query_params(req);
        assert_eq!(params[0], ("code".into(), "M.C5/abc".into()));
        assert_eq!(params[1], ("state".into(), "xyz 1".into()));
        assert_eq!(percent_decode("100%"), "100%");
    }

    #[test]
    fn builds_authorize_url() {
        let p = provider(AuthMethod::Microsoft).unwrap();
        let client = ClientConfig { client_id: "abc".into(), client_secret: String::new() };
        let url = authorize_url(&p, &client, "http://localhost:5000", "st", "ch", "me@contoso.com");
        assert!(url.starts_with("https://login.microsoftonline.com/common/oauth2/v2.0/authorize?client_id=abc&"));
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A5000"));
        assert!(url.contains("scope=https%3A%2F%2Foutlook.office.com%2FIMAP.AccessAsUser.All%20"));
        assert!(url.contains("login_hint=me%40contoso.com"));
        assert!(url.contains("code_challenge_method=S256"));
    }

    #[test]
    fn xoauth2_format() {
        assert_eq!(xoauth2("a@b.c", "tok"), "user=a@b.c\x01auth=Bearer tok\x01\x01");
        let auth = XOAuth2 { user: "a@b.c".into(), access_token: "tok".into() };
        assert_eq!(imap::Authenticator::process(&auth, b""), xoauth2("a@b.c", "tok"));
        assert_eq!(imap::Authenticator::process(&auth, b"{\"status\":\"401\"}"), "");
    }

    /// Hits Microsoft's real token endpoint; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn microsoft_token_endpoint_reports_errors() {
        let client = ClientConfig { client_id: "00000000-0000-0000-0000-000000000000".into(), client_secret: String::new() };
        let err = refresh_scoped(AuthMethod::Microsoft, &client, "not-a-real-token", None).unwrap_err();
        assert!(err.starts_with("Sign-in failed ("), "{err}");
    }

    #[test]
    fn loopback_receives_code() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let browser = std::thread::spawn(move || {
            let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            s.write_all(b"GET /?code=the-code&state=s1 HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
            let mut resp = String::new();
            s.read_to_string(&mut resp).unwrap();
            resp
        });
        let code = wait_for_code(&listener, "s1", &AtomicBool::new(false)).unwrap();
        assert_eq!(code, "the-code");
        assert!(browser.join().unwrap().starts_with("HTTP/1.1 200 OK"));
    }
}
