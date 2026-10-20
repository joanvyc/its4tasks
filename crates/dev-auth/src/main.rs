//! A stand-in for a real sign-in service (Clerk or similar), for trying
//! its4tasks locally with several users. **Never use it for real:** anyone
//! can sign in as anyone, and its signing key is public in this repository
//! (`crates/server/tests/fixtures`).
//!
//! It signs JWTs with `iss` = its own address and `sub` = `user_<name>`, and
//! offers what its4tasks-server expects of an authority:
//! - `GET /.well-known/jwks.json`: its key.
//! - `GET /sign-in?redirect=<url>`: asks for a name, then sets the
//!   `__session` cookie to a token and goes back to `url`. The cookie is for
//!   this host, and cookies ignore ports, so a server on another port of the
//!   same host gets it, as Clerk sets it for the app's own domain.
//! - `GET /authorize`, `POST /token`: OAuth 2 authorization code with PKCE
//!   (S256) and refresh tokens, for the native app.
//! - A mock of Clerk's script, at the path Clerk serves it from
//!   (`/npm/@clerk/clerk-js@6/dist/clerk.browser.js`), with
//!   `GET /v1/client/token` for its tokens: signing in also sets a
//!   long-lived `__client` cookie, and that gets fresh tokens, as a Clerk
//!   session does. Point `$ITS4TASKS_CLERK_FRONTEND_API` at dev-auth to try
//!   its4tasks-server's Clerk support. `POST /v1/client/revoke?sub=…` cuts
//!   a subject's session off, as signing out elsewhere would, and
//!   `POST /v1/client/restore?sub=…` brings it back.
//!
//! Listens on `$ITS4TASKS_DEV_AUTH_ADDR`, by default 127.0.0.1:7980.
//! Tokens last `$ITS4TASKS_DEV_AUTH_TTL` seconds, by default an hour.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{AppendHeaders, Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

const KEY: &[u8] = include_bytes!("../../server/tests/fixtures/dev-key.pem");
const JWKS: &str = include_str!("../../server/tests/fixtures/dev-jwks.json");
const KEY_ID: &str = "its4tasks-dev";
const CLERK_JS: &str = include_str!("clerk.browser.js");
/// The session cookie the mock Clerk's tokens come from.
const CLIENT_COOKIE: &str = "__client";
const DEFAULT_ADDR: &str = "127.0.0.1:7980";

struct Authority {
    /// Its own address, which is also the tokens' `iss`.
    url: String,
    /// How long tokens last, in seconds.
    ttl: i64,
    key: EncodingKey,
    /// Codes handed out by `/authorize` and not yet swapped for tokens.
    codes: Mutex<HashMap<String, Grant>>,
    /// Refresh tokens, by token, with the subject they're for.
    refresh: Mutex<HashMap<String, String>>,
    /// Subjects whose mock Clerk session is cut off for now.
    revoked: Mutex<HashSet<String>>,
}

/// What an authorization code stands for.
struct Grant {
    sub: String,
    client_id: String,
    redirect_uri: String,
    challenge: String,
}

impl Authority {
    fn token(&self, sub: &str) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(KEY_ID.to_owned());
        let now = chrono::Utc::now().timestamp();
        let claims = json!({ "iss": self.url, "sub": sub, "iat": now, "exp": now + self.ttl });
        encode(&header, &claims, &self.key).expect("the dev key signs")
    }
}

type Shared = Arc<Authority>;

fn app(authority: Authority) -> Router {
    Router::new()
        .route("/.well-known/jwks.json", get(jwks))
        .route("/.well-known/openid-configuration", get(openid))
        .route("/sign-in", get(sign_in_form).post(sign_in))
        .route("/authorize", get(authorize_form).post(authorize))
        .route("/token", axum::routing::post(token))
        .route(
            "/npm/@clerk/clerk-js@6/dist/clerk.browser.js",
            get(clerk_js),
        )
        .route("/v1/client/token", get(client_token))
        .route("/v1/client/revoke", axum::routing::post(revoke))
        .route("/v1/client/restore", axum::routing::post(restore))
        .with_state(Arc::new(authority))
}

#[tokio::main]
async fn main() {
    let addr = std::env::var("ITS4TASKS_DEV_AUTH_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.into());
    let ttl = std::env::var("ITS4TASKS_DEV_AUTH_TTL")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3600);
    let authority = Authority {
        url: format!("http://{addr}"),
        ttl,
        key: EncodingKey::from_rsa_pem(KEY).expect("the dev key parses"),
        codes: Mutex::new(HashMap::new()),
        refresh: Mutex::new(HashMap::new()),
        revoked: Mutex::new(HashSet::new()),
    };
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("can't listen on {addr}: {e}"));
    eprintln!("its4tasks-dev-auth: FOR LOCAL TESTING ONLY, issuing as http://{addr}");
    axum::serve(listener, app(authority)).await.expect("serves");
}

async fn jwks() -> Response {
    ([(header::CONTENT_TYPE, "application/json")], JWKS).into_response()
}

async fn openid(State(a): State<Shared>) -> Json<serde_json::Value> {
    Json(json!({
        "issuer": a.url,
        "authorization_endpoint": format!("{}/authorize", a.url),
        "token_endpoint": format!("{}/token", a.url),
        "jwks_uri": format!("{}/.well-known/jwks.json", a.url),
        "code_challenge_methods_supported": ["S256"],
    }))
}

#[derive(Deserialize)]
struct SignInQuery {
    redirect: String,
}

async fn sign_in_form(Query(q): Query<SignInQuery>) -> Response {
    if !local(&q.redirect) {
        return not_local();
    }
    page(
        "Sign in to its4tasks (dev)",
        &format!(
            r#"<input type="hidden" name="redirect" value="{}">"#,
            escape(&q.redirect)
        ),
    )
}

#[derive(Deserialize)]
struct SignIn {
    name: String,
    redirect: String,
}

async fn sign_in(State(a): State<Shared>, Form(f): Form<SignIn>) -> Response {
    if !local(&f.redirect) {
        return not_local();
    }
    let sub = subject(&f.name);
    let token = a.token(&sub);
    let session = format!("__session={token}; Path=/; HttpOnly; SameSite=Lax");
    let client = format!("{CLIENT_COOKIE}={sub}; Path=/; HttpOnly; SameSite=Lax; Max-Age=2592000");
    (
        AppendHeaders([(header::SET_COOKIE, session), (header::SET_COOKIE, client)]),
        Redirect::to(&f.redirect),
    )
        .into_response()
}

/// The mock of Clerk's script. Pages load it cross-origin.
async fn clerk_js() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/javascript"),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        CLERK_JS,
    )
        .into_response()
}

/// A fresh token for the `__client` session, for the mock Clerk script on
/// a page of this machine.
async fn client_token(State(a): State<Shared>, headers: HeaderMap) -> Response {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .filter(|o| local(o))
        .unwrap_or("null")
        .to_owned();
    let cors = [
        (header::ACCESS_CONTROL_ALLOW_ORIGIN, origin),
        (header::ACCESS_CONTROL_ALLOW_CREDENTIALS, "true".to_owned()),
        (header::VARY, "Origin".to_owned()),
    ];
    match cookie(&headers, CLIENT_COOKIE) {
        Some(sub) if sub.starts_with("user_") && !a.revoked.lock().unwrap().contains(&sub) => {
            (cors, Json(json!({ "jwt": a.token(&sub) }))).into_response()
        }
        _ => (cors, StatusCode::UNAUTHORIZED).into_response(),
    }
}

#[derive(Deserialize)]
struct SubjectQuery {
    sub: String,
}

/// Cuts a subject's mock Clerk session off, for testing signing out.
async fn revoke(State(a): State<Shared>, Query(q): Query<SubjectQuery>) -> StatusCode {
    a.revoked.lock().unwrap().insert(q.sub);
    StatusCode::NO_CONTENT
}

/// Undoes `revoke`.
async fn restore(State(a): State<Shared>, Query(q): Query<SubjectQuery>) -> StatusCode {
    a.revoked.lock().unwrap().remove(&q.sub);
    StatusCode::NO_CONTENT
}

/// The cookie called `name`, if the request carries it.
fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|c| c.trim().strip_prefix(name)?.strip_prefix('='))
        .map(str::to_owned)
}

#[derive(Deserialize)]
struct AuthorizeQuery {
    response_type: String,
    client_id: String,
    redirect_uri: String,
    code_challenge: String,
    code_challenge_method: String,
    #[serde(default)]
    state: String,
}

async fn authorize_form(Query(q): Query<AuthorizeQuery>) -> Response {
    if q.response_type != "code" || q.code_challenge_method != "S256" {
        return (
            StatusCode::BAD_REQUEST,
            "only response_type=code with S256 PKCE",
        )
            .into_response();
    }
    if !local(&q.redirect_uri) {
        return not_local();
    }
    let hidden = [
        ("client_id", &q.client_id),
        ("redirect_uri", &q.redirect_uri),
        ("code_challenge", &q.code_challenge),
        ("state", &q.state),
    ]
    .iter()
    .map(|(k, v)| format!(r#"<input type="hidden" name="{k}" value="{}">"#, escape(v)))
    .collect::<String>();
    page("Sign in to the its4tasks app (dev)", &hidden)
}

#[derive(Deserialize)]
struct Authorize {
    name: String,
    client_id: String,
    redirect_uri: String,
    code_challenge: String,
    state: String,
}

async fn authorize(State(a): State<Shared>, Form(f): Form<Authorize>) -> Response {
    if !local(&f.redirect_uri) {
        return not_local();
    }
    let code = Uuid::new_v4().simple().to_string();
    a.codes.lock().unwrap().insert(
        code.clone(),
        Grant {
            sub: subject(&f.name),
            client_id: f.client_id,
            redirect_uri: f.redirect_uri.clone(),
            challenge: f.code_challenge,
        },
    );
    let sep = if f.redirect_uri.contains('?') {
        '&'
    } else {
        '?'
    };
    let back = format!(
        "{}{sep}code={code}&state={}",
        f.redirect_uri,
        escape_query(&f.state)
    );
    Redirect::to(&back).into_response()
}

#[derive(Deserialize)]
struct TokenRequest {
    grant_type: String,
    code: Option<String>,
    code_verifier: Option<String>,
    redirect_uri: Option<String>,
    client_id: Option<String>,
    refresh_token: Option<String>,
}

async fn token(State(a): State<Shared>, Form(f): Form<TokenRequest>) -> Response {
    let bad = |why: &str| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid_grant", "error_description": why })),
        )
            .into_response()
    };
    let sub = match f.grant_type.as_str() {
        "authorization_code" => {
            let Some(grant) = f.code.and_then(|c| a.codes.lock().unwrap().remove(&c)) else {
                return bad("unknown or used code");
            };
            let challenge = f
                .code_verifier
                .map(|v| URL_SAFE_NO_PAD.encode(Sha256::digest(v.as_bytes())));
            if challenge.as_deref() != Some(grant.challenge.as_str()) {
                return bad("the code verifier doesn't match");
            }
            if f.redirect_uri.as_deref() != Some(grant.redirect_uri.as_str())
                || f.client_id.as_deref() != Some(grant.client_id.as_str())
            {
                return bad("the client or redirect doesn't match");
            }
            grant.sub
        }
        "refresh_token" => {
            let refresh = a.refresh.lock().unwrap();
            match f.refresh_token.and_then(|t| refresh.get(&t).cloned()) {
                Some(sub) => sub,
                None => return bad("unknown refresh token"),
            }
        }
        _ => return bad("unsupported grant type"),
    };
    let refresh = Uuid::new_v4().simple().to_string();
    a.refresh
        .lock()
        .unwrap()
        .insert(refresh.clone(), sub.clone());
    Json(json!({
        "access_token": a.token(&sub),
        "token_type": "Bearer",
        "expires_in": a.ttl,
        "refresh_token": refresh,
    }))
    .into_response()
}

/// The subject for a name typed into the form.
fn subject(name: &str) -> String {
    let name: String = name
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    format!(
        "user_{}",
        if name.is_empty() { "anon".into() } else { name }
    )
}

/// Only send people back to this machine: the dev authority is no open
/// redirect.
fn local(url: &str) -> bool {
    ["http://127.0.0.1", "http://localhost", "http://[::1]"]
        .iter()
        .any(|p| {
            url.strip_prefix(p)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with([':', '/', '?']))
        })
}

fn not_local() -> Response {
    (
        StatusCode::BAD_REQUEST,
        "The dev authority only sends people back to this machine.",
    )
        .into_response()
}

fn page(title: &str, hidden: &str) -> Response {
    Html(format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>{title}</title>
<style>
  body {{ font-family: sans-serif; display: grid; place-items: center; height: 100vh; margin: 0; background: #f4f4f4; }}
  form {{ background: white; padding: 2rem; border-radius: 8px; box-shadow: 0 2px 8px #0002; }}
  input[name=name] {{ font-size: 1.1rem; padding: .4rem; }}
  button {{ font-size: 1.1rem; padding: .4rem 1rem; }}
  p {{ color: #a33; font-size: .9rem; }}
</style></head>
<body><form method="post">
  <h2>{title}</h2>
  <label>Sign in as <input name="name" autofocus required placeholder="alice"></label>
  {hidden}
  <button type="submit">Sign in</button>
  <p>Development sign-in: no password, anyone can be anyone.</p>
</form></body></html>"#
    ))
    .into_response()
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_query(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use its4tasks_server::auth::{Jwt, JwtConfig, WebSignIn};
    use tower::ServiceExt;

    use super::*;

    const URL: &str = "http://127.0.0.1:7980";

    fn authority() -> Router {
        app(Authority {
            url: URL.into(),
            ttl: 3600,
            key: EncodingKey::from_rsa_pem(KEY).unwrap(),
            codes: Mutex::new(HashMap::new()),
            refresh: Mutex::new(HashMap::new()),
            revoked: Mutex::new(HashSet::new()),
        })
    }

    async fn send(
        app: &Router,
        request: Request<Body>,
    ) -> (StatusCode, axum::http::HeaderMap, String) {
        let response = app.clone().oneshot(request).await.unwrap();
        let (status, headers) = (response.status(), response.headers().clone());
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, headers, String::from_utf8_lossy(&body).into_owned())
    }

    fn form(path: &str, body: &str) -> Request<Body> {
        Request::post(path)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body.to_owned()))
            .unwrap()
    }

    /// Checks a token the way its4tasks-server does.
    async fn subject_of(token: &str) -> String {
        let config = JwtConfig {
            issuer: URL.into(),
            jwks_url: String::new(),
            audience: None,
            web: WebSignIn::Redirect(String::new()),
            cookie: "__session".into(),
            oauth: None,
        };
        let jwt = Jwt::with_keys(config, serde_json::from_str(JWKS).unwrap());
        jwt.verify(token).await.unwrap().sub
    }

    #[tokio::test]
    async fn the_mock_clerk_gives_tokens_for_the_session() {
        let app = authority();
        let (_, headers, _) = send(
            &app,
            form(
                "/sign-in",
                "name=Bob&redirect=http%3A%2F%2F127.0.0.1%3A7979%2F",
            ),
        )
        .await;
        let client = headers
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|c| c.to_str().ok()?.split(';').next())
            .find(|c| c.starts_with("__client="))
            .unwrap()
            .to_owned();
        let ask = |cookie: Option<&str>| {
            let mut request =
                Request::get("/v1/client/token").header(header::ORIGIN, "http://127.0.0.1:7979");
            if let Some(cookie) = cookie {
                request = request.header(header::COOKIE, cookie);
            }
            request.body(Body::empty()).unwrap()
        };
        let (status, headers, body) = send(&app, ask(Some(&client))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            headers[header::ACCESS_CONTROL_ALLOW_ORIGIN],
            "http://127.0.0.1:7979"
        );
        let answer: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            subject_of(answer["jwt"].as_str().unwrap()).await,
            "user_bob"
        );
        assert_eq!(send(&app, ask(None)).await.0, StatusCode::UNAUTHORIZED);
        // Cut off, then back.
        let post = |path: &str| Request::post(path).body(Body::empty()).unwrap();
        send(&app, post("/v1/client/revoke?sub=user_bob")).await;
        assert_eq!(
            send(&app, ask(Some(&client))).await.0,
            StatusCode::UNAUTHORIZED
        );
        send(&app, post("/v1/client/restore?sub=user_bob")).await;
        assert_eq!(send(&app, ask(Some(&client))).await.0, StatusCode::OK);
        let (status, _, script) = send(
            &app,
            Request::get("/npm/@clerk/clerk-js@6/dist/clerk.browser.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(script.contains("window.Clerk"));
    }

    #[tokio::test]
    async fn signs_browsers_in_with_a_cookie() {
        let app = authority();
        let (status, headers, _) = send(
            &app,
            form(
                "/sign-in",
                "name=Alice&redirect=http%3A%2F%2F127.0.0.1%3A7979%2F",
            ),
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(headers[header::LOCATION], "http://127.0.0.1:7979/");
        let cookie = headers[header::SET_COOKIE].to_str().unwrap();
        let token = cookie
            .strip_prefix("__session=")
            .and_then(|c| c.split(';').next())
            .unwrap();
        assert_eq!(subject_of(token).await, "user_alice");
        // Nowhere but this machine.
        let (status, _, _) = send(
            &app,
            form("/sign-in", "name=a&redirect=https%3A%2F%2Fevil.test%2F"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn signs_apps_in_with_pkce() {
        let app = authority();
        let verifier = "a-long-random-verifier-string-0123456789";
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let redirect = "http%3A%2F%2F127.0.0.1%3A5555%2Fcallback";
        let (status, headers, _) = send(
            &app,
            form(
                "/authorize",
                &format!(
                    "name=bob&client_id=its4tasks&redirect_uri={redirect}&code_challenge={challenge}&state=xyz"
                ),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        let location = headers[header::LOCATION].to_str().unwrap().to_owned();
        let code = location
            .split("code=")
            .nth(1)
            .and_then(|c| c.split('&').next())
            .unwrap();
        assert!(location.ends_with("&state=xyz"));

        let swap = |verifier: &str| {
            form(
                "/token",
                &format!(
                    "grant_type=authorization_code&code={code}&code_verifier={verifier}&redirect_uri={redirect}&client_id=its4tasks"
                ),
            )
        };
        let (status, _, _) = send(&app, swap("wrong")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // A failed swap uses the code up.
        let (status, _, _) = send(&app, swap(verifier)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // Again, properly this time.
        let (_, headers, _) = send(
            &app,
            form(
                "/authorize",
                &format!(
                    "name=bob&client_id=its4tasks&redirect_uri={redirect}&code_challenge={challenge}&state="
                ),
            ),
        )
        .await;
        let location = headers[header::LOCATION].to_str().unwrap().to_owned();
        let code = location
            .split("code=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap();
        let request = form(
            "/token",
            &format!(
                "grant_type=authorization_code&code={code}&code_verifier={verifier}&redirect_uri={redirect}&client_id=its4tasks"
            ),
        );
        let (status, _, body) = send(&app, request).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let tokens: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            subject_of(tokens["access_token"].as_str().unwrap()).await,
            "user_bob"
        );

        let refresh = tokens["refresh_token"].as_str().unwrap();
        let (status, _, body) = send(
            &app,
            form(
                "/token",
                &format!("grant_type=refresh_token&refresh_token={refresh}"),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let again: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            subject_of(again["access_token"].as_str().unwrap()).await,
            "user_bob"
        );
    }
}
