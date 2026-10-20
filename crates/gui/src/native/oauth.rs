//! Signing the native app in to a server through the browser: OAuth 2
//! authorization code with PKCE, as a public client, with the authority
//! sending the browser back to a one-off listener on a loopback port. The
//! server says which authority to use (`GET /api/auth`).

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// A signed-in session's tokens.
#[derive(Debug, Clone)]
pub struct Tokens {
    pub access: String,
    pub refresh: Option<String>,
    /// When `access` stops working, as far as the authority said.
    pub expires: Instant,
    /// How long `access` was good for when it came.
    pub lifetime: Duration,
}

impl Tokens {
    /// Time to get a new access token: its last quarter, or last minute if
    /// it lasts long. Some authorities' tokens last a minute in all.
    pub fn expiring(&self) -> bool {
        let margin = (self.lifetime / 4).min(Duration::from_secs(60));
        self.expires.saturating_duration_since(Instant::now()) < margin
    }
}

/// How the server's authority signs apps in.
#[derive(Debug, Deserialize)]
struct OAuth {
    authorize_url: String,
    token_url: String,
    client_id: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
}

/// How long to wait for someone to sign in in the browser.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .http_status_as_error(false)
        .build()
        .into()
}

fn oauth(server: &str) -> Result<OAuth, String> {
    let url = format!("{}/api/auth", server.trim_end_matches('/'));
    let mut response = agent()
        .get(&url)
        .call()
        .map_err(|e| format!("{url}: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("{server} doesn't offer signing in for apps"));
    }
    response
        .body_mut()
        .read_json()
        .map_err(|e| format!("{url}: {e}"))
}

/// Signs in through the browser. Blocks until done, or until it fails or
/// times out.
pub fn sign_in(server: &str) -> Result<Tokens, String> {
    let oauth = oauth(server)?;
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let redirect = format!(
        "http://127.0.0.1:{}/callback",
        listener.local_addr().map_err(|e| e.to_string())?.port()
    );
    let verifier = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = Uuid::new_v4().simple().to_string();
    let url = format!(
        "{}{}response_type=code&client_id={}&redirect_uri={}&code_challenge={challenge}\
         &code_challenge_method=S256&state={state}",
        oauth.authorize_url,
        if oauth.authorize_url.contains('?') {
            '&'
        } else {
            '?'
        },
        encode(&oauth.client_id),
        encode(&redirect),
    );
    webbrowser::open(&url).map_err(|e| format!("couldn't open the browser: {e}"))?;
    let code = wait_for_code(&listener, &state)?;
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("code_verifier", verifier.as_str()),
        ("redirect_uri", redirect.as_str()),
        ("client_id", oauth.client_id.as_str()),
    ];
    tokens(&oauth.token_url, &form)
}

/// A new access token from a refresh token, without the browser.
pub fn refresh(server: &str, refresh_token: &str) -> Result<Tokens, String> {
    let oauth = oauth(server)?;
    let form = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", oauth.client_id.as_str()),
    ];
    let mut tokens = tokens(&oauth.token_url, &form)?;
    // Authorities may keep the refresh token as it was.
    tokens
        .refresh
        .get_or_insert_with(|| refresh_token.to_owned());
    Ok(tokens)
}

fn tokens(token_url: &str, form: &[(&str, &str)]) -> Result<Tokens, String> {
    let mut response = agent()
        .post(token_url)
        .send_form(form.iter().copied())
        .map_err(|e| format!("{token_url}: {e}"))?;
    let status = response.status();
    if !status.is_success() {
        #[derive(Deserialize)]
        struct Refusal {
            error: String,
            error_description: Option<String>,
        }
        let why = match response.body_mut().read_json::<Refusal>() {
            Ok(r) => r.error_description.unwrap_or(r.error),
            Err(_) => status.to_string(),
        };
        return Err(format!("the sign-in service refused: {why}"));
    }
    let answer: TokenResponse = response
        .body_mut()
        .read_json()
        .map_err(|e| format!("{token_url}: {e}"))?;
    let lifetime = Duration::from_secs(answer.expires_in.unwrap_or(3600));
    Ok(Tokens {
        access: answer.access_token,
        refresh: answer.refresh_token,
        expires: Instant::now() + lifetime,
        lifetime,
    })
}

/// Waits for the browser to come back with the code, and thanks it.
fn wait_for_code(listener: &TcpListener, state: &str) -> Result<String, String> {
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let deadline = Instant::now() + SIGN_IN_TIMEOUT;
    loop {
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() > deadline {
                    return Err("nobody signed in in the browser".into());
                }
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(e) => return Err(e.to_string()),
        };
        stream.set_nonblocking(false).map_err(|e| e.to_string())?;
        let mut line = String::new();
        BufReader::new(&stream)
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        // GET /callback?code=…&state=… HTTP/1.1
        let query = line
            .split_whitespace()
            .nth(1)
            .and_then(|path| path.split_once('?'))
            .map(|(_, q)| q)
            .unwrap_or("");
        let param = |name: &str| {
            query
                .split('&')
                .find_map(|kv| kv.strip_prefix(name)?.strip_prefix('='))
                .map(decode)
        };
        let (code, came_back) = (param("code"), param("state"));
        let ok = code.is_some() && came_back.as_deref() == Some(state);
        let page = if ok {
            "Signed in. You can close this tab and go back to its4tasks."
        } else {
            "Signing in didn't work. Go back to its4tasks and try again."
        };
        let mut stream = stream;
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n\
             <!doctype html><title>its4tasks</title><p style=\"font-family:sans-serif\">{page}</p>"
        );
        if ok {
            return Ok(code.expect("checked"));
        }
        // A stray request (say, for a favicon): keep waiting for the real one.
        if came_back.is_some() {
            return Err("the sign-in came back for someone else".into());
        }
    }
}

/// Escapes everything but unreserved characters, for a URL's query.
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Undoes `encode` (and `+` for space).
fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = bytes
                    .get(i + 1..i + 3)
                    .and_then(|h| std::str::from_utf8(h).ok())
                    .and_then(|h| u8::from_str_radix(h, 16).ok());
                match hex {
                    Some(b) => {
                        out.push(b);
                        i += 3;
                        continue;
                    }
                    None => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_values_round_trip() {
        let s = "http://127.0.0.1:5555/callback?x=1 2";
        assert_eq!(decode(&encode(s)), s);
        assert_eq!(decode("a+b%2Fc%"), "a b/c%");
    }
}
