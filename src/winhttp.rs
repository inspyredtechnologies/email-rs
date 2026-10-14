//! A small blocking HTTP client over Windows' WinHTTP, used for Exchange Server
//! (EWS and Autodiscover). WinHTTP handles Negotiate (Kerberos/NTLM) and Basic
//! with explicit credentials, including the channel binding Exchange's Extended
//! Protection may require, uses the system proxy, and trusts the Windows
//! certificate store (so corporate CAs work).

use std::ffi::c_void;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, GetLastError};
use windows_sys::Win32::Networking::WinHttp::*;

pub type Res<T> = Result<T, String>;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Owned WinHTTP handle.
struct Handle(*mut c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { WinHttpCloseHandle(self.0) };
        }
    }
}

fn last_error(what: &str) -> String {
    let code = unsafe { GetLastError() };
    let why = match code {
        ERROR_WINHTTP_NAME_NOT_RESOLVED => "the server name couldn't be resolved".to_string(),
        ERROR_WINHTTP_CANNOT_CONNECT => "couldn't connect to the server".to_string(),
        ERROR_WINHTTP_TIMEOUT => "the server didn't respond in time".to_string(),
        ERROR_WINHTTP_SECURE_FAILURE => "the server's security certificate isn't trusted".to_string(),
        other => format!("Windows error {other}"),
    };
    format!("{what}: {why}")
}

/// (https, host, port, path+query) of an http(s) URL.
pub fn split_url(url: &str) -> Res<(bool, String, u16, String)> {
    let (secure, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        return Err(format!("Not an http(s) URL: {url}"));
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() => (h, p.parse().map_err(|_| format!("Bad port in {url}"))?),
        _ => (authority, if secure { 443 } else { 80 }),
    };
    if host.is_empty() {
        return Err(format!("No host in {url}"));
    }
    Ok((secure, host.to_string(), port, path.to_string()))
}

pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
    /// Redirect target when redirects aren't followed.
    pub location: Option<String>,
}

pub struct Client {
    session: Handle,
    credentials: Option<(Vec<u16>, Vec<u16>)>,
}

impl Client {
    /// `credentials` = (user, password); the user may be `DOMAIN\user`, a UPN or an email address.
    pub fn new(credentials: Option<(&str, &str)>, timeout_secs: i32) -> Res<Self> {
        let agent = wide("E-Mail RS");
        let session = Handle(unsafe { WinHttpOpen(agent.as_ptr(), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, null(), null(), 0) });
        if session.0.is_null() {
            return Err(last_error("Couldn't start WinHTTP"));
        }
        let ms = timeout_secs * 1000;
        unsafe { WinHttpSetTimeouts(session.0, ms, ms, ms, ms) };
        Ok(Self { session, credentials: credentials.map(|(u, p)| (wide(u), wide(p))) })
    }

    fn header(req: &Handle, info: u32) -> Option<String> {
        let mut len = 0u32;
        unsafe { WinHttpQueryHeaders(req.0, info, null(), null_mut(), &mut len, null_mut()) };
        if unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER || len == 0 {
            return None;
        }
        let mut buf = vec![0u16; len as usize / 2 + 1];
        let ok = unsafe { WinHttpQueryHeaders(req.0, info, null(), buf.as_mut_ptr().cast(), &mut len, null_mut()) };
        (ok != 0).then(|| String::from_utf16_lossy(&buf[..len as usize / 2]))
    }

    /// Send a request, answering an authentication challenge with the
    /// credentials (Negotiate preferred, then NTLM, then Basic).
    pub fn request(&self, method: &str, url: &str, content_type: Option<&str>, body: &[u8], follow_redirects: bool) -> Res<Response> {
        let (secure, host, port, path) = split_url(url)?;
        let host_w = wide(&host);
        let connect = Handle(unsafe { WinHttpConnect(self.session.0, host_w.as_ptr(), port, 0) });
        if connect.0.is_null() {
            return Err(last_error(&format!("Couldn't reach {host}")));
        }
        let (method_w, path_w) = (wide(method), wide(&path));
        let flags = if secure { WINHTTP_FLAG_SECURE } else { 0 };
        let req = Handle(unsafe { WinHttpOpenRequest(connect.0, method_w.as_ptr(), path_w.as_ptr(), null(), null(), null(), flags) });
        if req.0.is_null() {
            return Err(last_error(&format!("Couldn't open a request to {host}")));
        }
        if !follow_redirects {
            let never = WINHTTP_OPTION_REDIRECT_POLICY_NEVER;
            unsafe { WinHttpSetOption(req.0, WINHTTP_OPTION_REDIRECT_POLICY, (&never as *const u32).cast(), 4) };
        }
        let headers = content_type.map(|ct| wide(&format!("Content-Type: {ct}\r\n")));
        let (hdr_ptr, hdr_len) = match &headers {
            Some(h) => (h.as_ptr(), (h.len() - 1) as u32),
            None => (null(), 0),
        };

        let mut answered_challenge = false;
        let status = loop {
            let ok = unsafe {
                WinHttpSendRequest(req.0, hdr_ptr, hdr_len, body.as_ptr().cast(), body.len() as u32, body.len() as u32, 0)
            };
            if ok == 0 || unsafe { WinHttpReceiveResponse(req.0, null_mut()) } == 0 {
                return Err(last_error(&format!("Request to {host} failed")));
            }
            let mut status = 0u32;
            let mut size = 4u32;
            unsafe {
                WinHttpQueryHeaders(
                    req.0,
                    WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                    null(),
                    (&mut status as *mut u32).cast(),
                    &mut size,
                    null_mut(),
                )
            };
            match (&self.credentials, status, answered_challenge) {
                (Some((user, pass)), 401, false) => {
                    let (mut supported, mut first, mut target) = (0u32, 0u32, 0u32);
                    unsafe { WinHttpQueryAuthSchemes(req.0, &mut supported, &mut first, &mut target) };
                    let scheme = [WINHTTP_AUTH_SCHEME_NEGOTIATE, WINHTTP_AUTH_SCHEME_NTLM, WINHTTP_AUTH_SCHEME_BASIC]
                        .into_iter()
                        .find(|s| supported & s != 0)
                        .ok_or_else(|| format!("{host} asked for an unsupported sign-in method."))?;
                    unsafe { WinHttpSetCredentials(req.0, WINHTTP_AUTH_TARGET_SERVER, scheme, user.as_ptr(), pass.as_ptr(), null_mut()) };
                    answered_challenge = true;
                }
                _ => break status as u16,
            }
        };

        let location = Self::header(&req, WINHTTP_QUERY_LOCATION);
        let mut out = Vec::new();
        loop {
            let mut avail = 0u32;
            if unsafe { WinHttpQueryDataAvailable(req.0, &mut avail) } == 0 {
                return Err(last_error(&format!("Reading the response from {host} failed")));
            }
            if avail == 0 {
                break;
            }
            let start = out.len();
            out.resize(start + avail as usize, 0);
            let mut read = 0u32;
            if unsafe { WinHttpReadData(req.0, out[start..].as_mut_ptr().cast(), avail, &mut read) } == 0 {
                return Err(last_error(&format!("Reading the response from {host} failed")));
            }
            out.truncate(start + read as usize);
        }
        Ok(Response { status, body: out, location })
    }
}

/// This PC's Windows time zone id (e.g. "AUS Eastern Standard Time"), as EWS expects.
pub fn windows_time_zone() -> Option<String> {
    use windows_sys::Win32::System::Time::{DYNAMIC_TIME_ZONE_INFORMATION, GetDynamicTimeZoneInformation};
    let mut info = DYNAMIC_TIME_ZONE_INFORMATION::default();
    let r = unsafe { GetDynamicTimeZoneInformation(&mut info) };
    if r == u32::MAX {
        return None;
    }
    let name = &info.TimeZoneKeyName;
    let len = name.iter().position(|c| *c == 0).unwrap_or(name.len());
    let s = String::from_utf16_lossy(&name[..len]);
    (!s.is_empty()).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn splits_urls() {
        assert_eq!(split_url("https://mail.contoso.com/EWS/Exchange.asmx").unwrap(), (true, "mail.contoso.com".into(), 443, "/EWS/Exchange.asmx".into()));
        assert_eq!(split_url("http://localhost:8080").unwrap(), (false, "localhost".into(), 8080, "/".into()));
        assert!(split_url("ftp://x").is_err());
    }

    #[test]
    fn has_a_time_zone() {
        assert!(windows_time_zone().is_some_and(|z| !z.is_empty()));
    }

    /// A local server that demands Basic auth: the client must answer the
    /// challenge and resend the body.
    #[test]
    fn answers_basic_challenge_and_resends_body() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..2 {
                let (mut s, _) = listener.accept().unwrap();
                let mut buf = vec![0u8; 8192];
                let mut req = Vec::new();
                // Read headers + the 5-byte body.
                while !String::from_utf8_lossy(&req).contains("\r\n\r\nhello") {
                    let n = s.read(&mut buf).unwrap();
                    if n == 0 {
                        break;
                    }
                    req.extend_from_slice(&buf[..n]);
                }
                let text = String::from_utf8_lossy(&req).into_owned();
                let authed = text.lines().any(|l| l.eq_ignore_ascii_case("authorization: basic ZG9tYWluXGFsZXg6cHc="));
                seen.push(text);
                let resp = if authed {
                    "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_string()
                } else {
                    "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"x\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                };
                s.write_all(resp.as_bytes()).unwrap();
            }
            seen
        });
        let client = Client::new(Some(("domain\\alex", "pw")), 10).unwrap();
        let resp = client.request("POST", &format!("http://127.0.0.1:{port}/EWS/Exchange.asmx"), Some("text/xml"), b"hello", true).unwrap();
        assert_eq!((resp.status, resp.body.as_slice()), (200, b"ok".as_slice()));
        let seen = server.join().unwrap();
        assert!(seen[1].starts_with("POST /EWS/Exchange.asmx HTTP/1.1"));
        assert!(seen[1].to_ascii_lowercase().contains("content-type: text/xml"));
    }
}
