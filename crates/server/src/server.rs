//! Users' boards and the web GUI over HTTP. Everything about a board needs
//! a signed-in user (see `auth`), and only ever reaches their own board.
//! - `GET /api/board`: the user's `BoardView`.
//! - `POST /api/changes` with a `Change`: an `Applied`, or a `Rejected` with
//!   404 (no such task or comment), 403 (it's on someone else's board) or
//!   409 (the board's rules refuse it, or a new id is taken).
//! - `POST /api/start`: a new user's board, with the welcome tasks; 409 if
//!   they have one.
//! - `POST /api/import` with an `its4tasks.json`'s text (or an older
//!   version's `board.json`): adds its tasks; 400 if
//!   it isn't one.
//! - The board in those answers leaves out archived tasks (see
//!   `Board::archived`), unless the request asks for them with
//!   `?archived=true`.
//! - `GET /api/auth`: how the native app signs in (OAuth), if it can.
//!   Answered to anyone.
//! - `/favicon.svg`: the site's icon (`page/favicon.svg`).
//! - `/`: the startup page (`page/index.html`). It checks sign-in and the
//!   board before loading anything big: signed-out visitors go to sign in,
//!   new users get the landing page, and everyone else the board app. The
//!   authority's sign-in page comes first if the server can tell (see
//!   `auth::JwtConfig::sign_in`).
//! - Anything else: the board app's files, built by `trunk build` in
//!   `crates/gui` and embedded in release builds. Their names don't change
//!   between builds, so they're sent with an ETag and `no-cache`.
//!
//! Without a valid token the API answers 401. Anything but a `GET` from a
//! page on another site is refused with 403 (see `same_origin`).

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use its4tasks_core::api::{ApiError, Applied, BoardView, Change, Rejected, now};
use its4tasks_core::model::Board;
use its4tasks_core::{import, welcome};
use rust_embed::Embed;
use serde::Deserialize;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

use crate::api::{Api, Failure};
use crate::auth::{Auth, Unauthorized, WebSignIn};
use crate::storage::User;

struct AppState {
    /// Changes are applied one at a time.
    api: Mutex<Api>,
    auth: Auth,
    /// The address browsers reach the server at, when it's behind a proxy
    /// that hides it (`$ITS4TASKS_PUBLIC_URL`), for coming back after
    /// signing in.
    public_url: Option<String>,
}

type Shared = Arc<AppState>;

/// The largest file `/api/import` takes, in bytes.
const IMPORT_LIMIT: usize = 16 << 20;

/// The web GUI (see build.rs). Debug builds read it from disk on each
/// request, so a new `trunk build` shows without rebuilding the server.
#[derive(Embed)]
#[folder = "../gui/dist"]
struct Web;

pub fn router(api: Api, auth: Auth) -> Router {
    let state = Arc::new(AppState {
        api: Mutex::new(api),
        auth,
        public_url: std::env::var("ITS4TASKS_PUBLIC_URL")
            .ok()
            .map(|u| u.trim_end_matches('/').to_owned()),
    });
    let api = Router::new()
        .route("/board", get(board))
        .route("/changes", post(change))
        .route("/start", post(start))
        .route(
            "/import",
            post(import_board).layer(DefaultBodyLimit::max(IMPORT_LIMIT)),
        )
        .route("/auth", get(auth_info))
        .layer(middleware::from_fn_with_state(state.clone(), same_origin));
    Router::new()
        .nest("/api", api)
        .fallback(web)
        .layer(middleware::from_fn(security_headers))
        .with_state(state)
}

/// Refuses requests that change something when a page on another site sent
/// them. The browser would sign those in with the session cookie, so a page
/// elsewhere could otherwise import into someone's board (CSRF). Browsers
/// say where a request comes from (`Sec-Fetch-Site`, or else `Origin`); the
/// native app says nothing, and is let through.
async fn same_origin(State(state): State<Shared>, request: Request, next: Next) -> Response {
    if request.method() != Method::GET && !from_here(&state, request.headers()) {
        return (StatusCode::FORBIDDEN, "Sent from another site.").into_response();
    }
    next.run(request).await
}

fn from_here(state: &AppState, headers: &HeaderMap) -> bool {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    match header("sec-fetch-site") {
        Some("same-origin" | "none") => return true,
        Some(_) => return false,
        None => {}
    }
    let Some(origin) = header(header::ORIGIN.as_str()) else {
        return true;
    };
    // `scheme://host[:port]`, without any path.
    let authority = |url: &str| {
        url.split_once("://").map(|(scheme, rest)| {
            (
                scheme.to_owned(),
                rest.split('/').next().unwrap_or("").to_owned(),
            )
        })
    };
    match (&state.public_url, authority(origin)) {
        (_, None) => false,
        (Some(public), origin) => authority(public) == origin,
        (None, Some((_, host))) => header(header::HOST.as_str()) == Some(host.as_str()),
    }
}

/// Headers for every answer: the browser mustn't guess content types, and
/// no other site may show these pages in a frame (clickjacking).
async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("frame-ancestors 'none'"),
    );
    response
}

pub async fn serve(listener: TcpListener, api: Api, auth: Auth) -> std::io::Result<()> {
    axum::serve(listener, router(api, auth)).await
}

/// Like `serve`, until `stop` finishes.
pub async fn serve_until(
    listener: TcpListener,
    api: Api,
    auth: Auth,
    stop: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(listener, router(api, auth))
        .with_graceful_shutdown(stop)
        .await
}

/// The signed-in user, made now if they're new; or the answer to give
/// instead.
async fn user(state: &AppState, headers: &HeaderMap) -> Result<User, Box<Response>> {
    let identity = state
        .auth
        .identify(headers)
        .await
        .map_err(|Unauthorized(why)| Box::new((StatusCode::UNAUTHORIZED, why).into_response()))?;
    let mut api = state.api.lock().await;
    api.sign_in(&identity.iss, &identity.sub)
        .await
        .map_err(|e| Box::new(internal(e.into())))
}

/// Which tasks a request wants in the board it's answered with.
#[derive(Deserialize)]
struct Shown {
    /// Archived ones too (see `Board::archived`).
    #[serde(default)]
    archived: bool,
}

impl Shown {
    fn board(&self, board: &Board) -> Board {
        if self.archived {
            board.clone()
        } else {
            board.without_archived(now())
        }
    }
}

async fn board(
    State(state): State<Shared>,
    Query(shown): Query<Shown>,
    headers: HeaderMap,
) -> Response {
    let user = match user(&state, &headers).await {
        Ok(user) => user,
        Err(response) => return *response,
    };
    let mut api = state.api.lock().await;
    match api.board(&user).await {
        Ok((started, board)) => Json(BoardView {
            started,
            board: shown.board(board),
            user: Some(user.id),
        })
        .into_response(),
        Err(e) => internal(e),
    }
}

async fn change(
    State(state): State<Shared>,
    Query(shown): Query<Shown>,
    headers: HeaderMap,
    Json(change): Json<Change>,
) -> Response {
    let user = match user(&state, &headers).await {
        Ok(user) => user,
        Err(response) => return *response,
    };
    let mut api = state.api.lock().await;
    let result = api.apply(&user, change).await;
    let board = match api.board(&user).await {
        Ok((_, board)) => shown.board(board),
        Err(e) => return internal(e),
    };
    match result {
        Ok(outcome) => Json(Applied { outcome, board }).into_response(),
        Err(Failure::Api(error)) => {
            (status(error), Json(Rejected { error, board })).into_response()
        }
        Err(e) => internal(e),
    }
}

/// The status a change refused for `error` is answered with.
fn status(error: ApiError) -> StatusCode {
    match error {
        ApiError::UnknownTask(_) | ApiError::UnknownNote(_) => StatusCode::NOT_FOUND,
        ApiError::Forbidden(_) => StatusCode::FORBIDDEN,
        ApiError::Move(_) | ApiError::Duplicate(_) | ApiError::Refused => StatusCode::CONFLICT,
        ApiError::TooBig => StatusCode::PAYLOAD_TOO_LARGE,
    }
}

/// Starts a new user's board with the welcome tasks.
async fn start(
    State(state): State<Shared>,
    Query(shown): Query<Shown>,
    headers: HeaderMap,
) -> Response {
    let user = match user(&state, &headers).await {
        Ok(user) => user,
        Err(response) => return *response,
    };
    let mut api = state.api.lock().await;
    match api.board(&user).await {
        Ok((true, _)) => (StatusCode::CONFLICT, "You already have a board.").into_response(),
        Ok((false, _)) => add_board(&mut api, &user, welcome::board(now()), &shown).await,
        Err(e) => internal(e),
    }
}

/// Adds the tasks of the `its4tasks.json` sent.
async fn import_board(
    State(state): State<Shared>,
    Query(shown): Query<Shown>,
    headers: HeaderMap,
    text: String,
) -> Response {
    let user = match user(&state, &headers).await {
        Ok(user) => user,
        Err(response) => return *response,
    };
    let board = match import::parse(&text) {
        Ok(board) => board,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("That isn't an its4tasks.json: {e}"),
            )
                .into_response();
        }
    };
    let mut api = state.api.lock().await;
    add_board(&mut api, &user, board, &shown).await
}

/// Adds `board`'s tasks to the user's, with new ids, and answers with the
/// user's `BoardView`.
async fn add_board(api: &mut Api, user: &User, board: Board, shown: &Shown) -> Response {
    let change = Change::Import {
        board: import::fresh_ids(board),
    };
    match api.apply(user, change).await {
        Ok(_) => {}
        Err(Failure::Api(e)) => return (status(e), e.to_string()).into_response(),
        Err(e) => return internal(e),
    }
    match api.board(user).await {
        Ok((started, board)) => Json(BoardView {
            started,
            board: shown.board(board),
            user: Some(user.id),
        })
        .into_response(),
        Err(e) => internal(e),
    }
}

async fn auth_info(State(state): State<Shared>) -> Response {
    match &state.auth {
        Auth::Jwt(jwt) => match &jwt.config.oauth {
            Some(oauth) => Json(oauth.clone()).into_response(),
            None => (
                StatusCode::NOT_FOUND,
                "This server has no sign-in for apps.",
            )
                .into_response(),
        },
        Auth::Local { .. } => StatusCode::NOT_FOUND.into_response(),
    }
}

/// A failure that isn't the client's doing.
fn internal(e: Failure) -> Response {
    eprintln!("its4tasks-server: {e}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Something went wrong on the server.",
    )
        .into_response()
}

/// The startup page, served at `/`.
const PAGE: &str = include_str!("page/index.html");

/// The site's icon: four focus cards, one in hand.
const FAVICON: &str = include_str!("page/favicon.svg");

async fn web(State(state): State<Shared>, uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.is_empty() || path == "index.html" {
        return page(&state, &uri, &headers).await;
    }
    if path == "favicon.svg" {
        return (
            [
                (header::CONTENT_TYPE, "image/svg+xml"),
                (header::CACHE_CONTROL, "max-age=86400"),
            ],
            FAVICON,
        )
            .into_response();
    }
    let Some(file) = Web::get(path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let etag = format!(
        "\"{}\"",
        file.metadata
            .sha256_hash()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    let cache = [
        (header::ETAG, etag.clone()),
        (header::CACHE_CONTROL, "no-cache".to_owned()),
    ];
    let unchanged = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|tags| tags.split(',').any(|t| t.trim() == etag));
    if unchanged {
        return (StatusCode::NOT_MODIFIED, cache).into_response();
    }
    let mime = file.metadata.mimetype().to_owned();
    (cache, [(header::CONTENT_TYPE, mime)], file.data).into_response()
}

/// The startup page. It needs someone signed in; the app's code doesn't.
/// A page signed in by Clerk's script sees to that itself (see `clerk`).
async fn page(state: &AppState, uri: &Uri, headers: &HeaderMap) -> Response {
    let jwt = match &state.auth {
        Auth::Jwt(jwt) => Some(jwt),
        Auth::Local { .. } => None,
    };
    if let Some(jwt) = jwt
        && let Some(sign_in) = jwt.config.sign_in(&page_url(state, headers, uri))
        && state.auth.identify(headers).await.is_err()
    {
        return Redirect::to(&sign_in).into_response();
    }
    let page = match jwt.map(|jwt| &jwt.config.web) {
        Some(WebSignIn::Clerk(clerk)) => {
            PAGE.replacen("</head>", &format!("{}</head>", clerk.script()), 1)
        }
        _ => PAGE.to_owned(),
    };
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        page,
    )
        .into_response()
}

/// The address of the page asked for, as the browser sees it.
fn page_url(state: &AppState, headers: &HeaderMap, uri: &Uri) -> String {
    let path = uri.path_and_query().map_or("/", |p| p.as_str());
    if let Some(public) = &state.public_url {
        return format!("{public}{path}");
    }
    let header = |name| headers.get(name).and_then(|v| v.to_str().ok());
    let host = header(header::HOST.as_str()).unwrap_or("localhost");
    let scheme = header("x-forwarded-proto").unwrap_or("http");
    format!("{scheme}://{host}{path}")
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use its4tasks_core::api::Outcome;
    use its4tasks_core::model::MessageKind;
    use jsonwebtoken::jwk::JwkSet;
    use jsonwebtoken::{EncodingKey, Header, encode};
    use serde_json::json;
    use tower::ServiceExt;
    use uuid::Uuid;

    use super::*;
    use crate::auth::{Jwt, JwtConfig, OAuth};

    const ISSUER: &str = "https://auth.test";

    fn config() -> JwtConfig {
        JwtConfig {
            issuer: ISSUER.into(),
            jwks_url: String::new(),
            audience: None,
            authorized_parties: Vec::new(),
            web: WebSignIn::Redirect("https://auth.test/sign-in?redirect={redirect}".into()),
            cookie: "__session".into(),
            oauth: Some(OAuth {
                issuer: ISSUER.into(),
                authorize_url: "https://auth.test/authorize".into(),
                token_url: "https://auth.test/token".into(),
                client_id: "its4tasks".into(),
            }),
        }
    }

    fn app() -> Router {
        let keys: JwkSet =
            serde_json::from_str(include_str!("../tests/fixtures/dev-jwks.json")).unwrap();
        router(
            Api::new(),
            Auth::Jwt(Box::new(Jwt::with_keys(config(), keys))),
        )
    }

    /// A token for `sub`, from `iss`, good for `ttl` seconds (negative: expired).
    fn token_from(iss: &str, sub: &str, ttl: i64) -> String {
        let key =
            EncodingKey::from_rsa_pem(include_bytes!("../tests/fixtures/dev-key.pem")).unwrap();
        let mut header = Header::new(jsonwebtoken::Algorithm::RS256);
        header.kid = Some("its4tasks-dev".into());
        let exp = chrono::Utc::now().timestamp() + ttl;
        encode(
            &header,
            &json!({ "iss": iss, "sub": sub, "exp": exp }),
            &key,
        )
        .unwrap()
    }

    fn token(sub: &str) -> String {
        token_from(ISSUER, sub, 3600)
    }

    async fn call(app: &Router, request: Request<Body>) -> (StatusCode, HeaderMap, String) {
        let response = app.clone().oneshot(request).await.unwrap();
        let (status, headers) = (response.status(), response.headers().clone());
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            headers,
            String::from_utf8_lossy(&bytes).into_owned(),
        )
    }

    async fn get(app: &Router, path: &str, token: Option<&str>) -> (StatusCode, String) {
        let mut request = Request::get(path);
        if let Some(token) = token {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let (status, _, body) = call(app, request.body(Body::empty()).unwrap()).await;
        (status, body)
    }

    async fn post(app: &Router, token: &str, change: &Change) -> (StatusCode, String) {
        post_to(app, "/api/changes", token, change).await
    }

    async fn post_to(
        app: &Router,
        path: &str,
        token: &str,
        change: &Change,
    ) -> (StatusCode, String) {
        let request = Request::post(path)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(change).unwrap()))
            .unwrap();
        let (status, _, body) = call(app, request).await;
        (status, body)
    }

    async fn view(app: &Router, token: &str) -> BoardView {
        let (status, body) = get(app, "/api/board", Some(token)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        serde_json::from_str(&body).unwrap()
    }

    #[tokio::test]
    async fn the_api_needs_a_valid_token() {
        let app = app();
        assert_eq!(
            get(&app, "/api/board", None).await.0,
            StatusCode::UNAUTHORIZED
        );
        for bad in [
            "not a token".to_owned(),
            token_from(ISSUER, "alice", -120),
            token_from("https://elsewhere.test", "alice", 3600),
        ] {
            assert_eq!(
                get(&app, "/api/board", Some(&bad)).await.0,
                StatusCode::UNAUTHORIZED,
                "{bad}"
            );
        }
        // Signed with a shared secret, under the right key id: refused.
        let mut header = Header::new(jsonwebtoken::Algorithm::HS256);
        header.kid = Some("its4tasks-dev".into());
        let exp = chrono::Utc::now().timestamp() + 3600;
        let forged = encode(
            &header,
            &json!({ "iss": ISSUER, "sub": "alice", "exp": exp }),
            &EncodingKey::from_secret(b"guess"),
        )
        .unwrap();
        assert_eq!(
            get(&app, "/api/board", Some(&forged)).await.0,
            StatusCode::UNAUTHORIZED
        );
        let change = Change::Unfocus { slot: 0 };
        let request = Request::post("/api/changes")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&change).unwrap()))
            .unwrap();
        assert_eq!(call(&app, request).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(get(&app, "/board", None).await.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_new_user_starts_a_board() {
        let app = app();
        let alice = token("alice");
        let first = view(&app, &alice).await;
        assert!(!first.started);
        assert!(first.board.tasks.is_empty());
        let welcome = its4tasks_core::import::fresh_ids(its4tasks_core::welcome::board(
            its4tasks_core::api::now(),
        ));
        let import = Change::Import { board: welcome };
        assert_eq!(post(&app, &alice, &import).await.0, StatusCode::OK);
        let now = view(&app, &alice).await;
        assert!(now.started);
        assert_eq!(now.board.tasks.len(), 2);
    }

    #[tokio::test]
    async fn users_only_reach_their_own_board() {
        let app = app();
        let (alice, bob) = (token("alice"), token("bob"));
        let add = Change::add_task("Alice's".into(), String::new(), None, true);
        let (status, body) = post(&app, &alice, &add).await;
        assert_eq!(status, StatusCode::OK);
        let applied: Applied = serde_json::from_str(&body).unwrap();
        let Outcome::Created(task) = applied.outcome else {
            panic!("no task")
        };
        let post_alice = Change::post(task, "hello".into(), MessageKind::Comment);
        post(&app, &alice, &post_alice).await;

        assert!(view(&app, &bob).await.board.tasks.is_empty());
        let rename = Change::Rename {
            task,
            title: "Bob's".into(),
        };
        let (status, body) = post(&app, &bob, &rename).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let rejected: Rejected = serde_json::from_str(&body).unwrap();
        assert_eq!(rejected.error, ApiError::Forbidden(task));
        assert!(rejected.board.tasks.is_empty(), "only bob's own board");
        let note = view(&app, &alice).await.board.tasks[0].notes[0].id;
        assert_eq!(
            post(&app, &bob, &Change::DeleteNote { note }).await.0,
            StatusCode::FORBIDDEN
        );
        let gone = Change::DeleteNote {
            note: Uuid::new_v4(),
        };
        assert_eq!(post(&app, &bob, &gone).await.0, StatusCode::NOT_FOUND);
        assert_eq!(view(&app, &alice).await.board.tasks[0].title, "Alice's");
    }

    #[tokio::test]
    async fn archived_tasks_are_sent_only_when_asked_for() {
        let app = app();
        let alice = token("alice");
        let (t0, long_ago) = (now(), now() - chrono::TimeDelta::days(40));
        let mut board = Board::default();
        let root = board.add_task("old".into(), "", None, long_ago);
        let sub = board.add_task("old sub".into(), "", Some(root), long_ago);
        let deep = board.add_task("old deep".into(), "", Some(sub), long_ago);
        let recent = board.add_task("recent".into(), "", None, long_ago);
        let open = board.add_task("open".into(), "", None, t0);
        for id in [root, sub, deep] {
            board.task_mut(id).unwrap().set_done(true, long_ago);
        }
        board.task_mut(recent).unwrap().set_done(true, t0);
        let import = Change::Import { board };
        assert_eq!(post(&app, &alice, &import).await.0, StatusCode::OK);
        let ids = |board: &Board| board.tasks.iter().map(|t| t.id).collect::<Vec<_>>();

        assert_eq!(ids(&view(&app, &alice).await.board), [recent, open]);
        let (status, body) = get(&app, "/api/board?archived=true", Some(&alice)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let all: BoardView = serde_json::from_str(&body).unwrap();
        assert_eq!(ids(&all.board), [root, sub, deep, recent, open]);

        // A change's answer too.
        let rename = Change::Rename {
            task: open,
            title: "still open".into(),
        };
        let (_, body) = post(&app, &alice, &rename).await;
        let applied: Applied = serde_json::from_str(&body).unwrap();
        assert_eq!(ids(&applied.board), [recent, open]);
        let (_, body) = post_to(&app, "/api/changes?archived=true", &alice, &rename).await;
        let applied: Applied = serde_json::from_str(&body).unwrap();
        assert_eq!(ids(&applied.board).len(), 5);

        // Reopening its deepest task brings the whole tree back.
        let reopen = Change::SetDone {
            task: deep,
            done: false,
        };
        assert_eq!(post(&app, &alice, &reopen).await.0, StatusCode::OK);
        assert_eq!(
            ids(&view(&app, &alice).await.board),
            [root, sub, deep, recent, open]
        );
    }

    #[tokio::test]
    async fn the_page_sends_strangers_to_sign_in() {
        let app = app();
        let request = Request::get("/?x=1")
            .header(header::HOST, "app.test:7979")
            .body(Body::empty())
            .unwrap();
        let (status, headers, _) = call(&app, request).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(
            headers[header::LOCATION],
            "https://auth.test/sign-in?redirect=http%3A%2F%2Fapp.test%3A7979%2F%3Fx%3D1"
        );
        // Signed in through the cookie, the startup page is served.
        let request = Request::get("/")
            .header(header::COOKIE, format!("__session={}", token("alice")))
            .body(Body::empty())
            .unwrap();
        let (status, _, page) = call(&app, request).await;
        assert_eq!(status, StatusCode::OK);
        assert!(page.contains("/its4tasks.js"));
        assert!(!page.contains("its4tasksAuth ="), "no Clerk here");
        // The page's code needs no sign-in.
        assert_eq!(
            get(&app, "/no/such/file.js", None).await.0,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn a_page_signed_in_by_clerk_loads_it() {
        let keys: JwkSet =
            serde_json::from_str(include_str!("../tests/fixtures/dev-jwks.json")).unwrap();
        let clerk =
            crate::clerk::Clerk::new("pk_test_ZXhhbXBsZS5jbGVyay5hY2NvdW50cy5kZXYk", None).unwrap();
        let config = JwtConfig {
            web: WebSignIn::Clerk(clerk),
            ..config()
        };
        let app = router(
            Api::new(),
            Auth::Jwt(Box::new(Jwt::with_keys(config, keys))),
        );
        // No redirect: Clerk's script signs in on the page.
        let (status, page) = get(&app, "/", None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(page.contains("window.its4tasksAuth"));
        assert!(page.contains("https://example.clerk.accounts.dev"));
        // Before the startup page's own script, which uses it.
        assert!(page.find("window.its4tasksAuth =") < page.find("/its4tasks.js"));
        // The API still wants a token.
        assert_eq!(
            get(&app, "/api/board", None).await.0,
            StatusCode::UNAUTHORIZED
        );
    }

    async fn post_text(
        app: &Router,
        path: &str,
        token: Option<&str>,
        text: &str,
    ) -> (StatusCode, String) {
        let mut request = Request::post(path).header(header::CONTENT_TYPE, "text/plain");
        if let Some(token) = token {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let (status, _, body) = call(app, request.body(Body::from(text.to_owned())).unwrap()).await;
        (status, body)
    }

    #[tokio::test]
    async fn new_users_start_a_board_once() {
        let app = app();
        let alice = token("alice");
        assert_eq!(
            post_text(&app, "/api/start", None, "").await.0,
            StatusCode::UNAUTHORIZED
        );
        let (status, body) = post_text(&app, "/api/start", Some(&alice), "").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let started: BoardView = serde_json::from_str(&body).unwrap();
        assert!(started.started);
        assert_eq!(started.board.tasks[0].title, "Welcome to its4tasks");
        // Whose it is, for work the browser keeps; someone else's differs.
        assert!(started.user.is_some());
        assert_eq!(view(&app, &alice).await.user, started.user);
        assert_ne!(view(&app, &token("bob")).await.user, started.user);
        // A second click doesn't make a second welcome board.
        let (status, _) = post_text(&app, "/api/start", Some(&alice), "").await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(view(&app, &alice).await.board, started.board);
    }

    #[tokio::test]
    async fn boards_are_imported_with_new_ids() {
        let app = app();
        let bob = token("bob");
        // An older board.json, with numbered tasks and comments without ids.
        let old = r#"{"tasks": [{"id": 0, "title": "From the old app",
            "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-02T00:00:00Z",
            "done": false, "notes": [{"at": "2026-01-02T00:00:00Z", "text": "still to do"}]}],
            "slots": [0, null, null, null], "next_id": 1}"#;
        assert_eq!(
            post_text(&app, "/api/import", None, old).await.0,
            StatusCode::UNAUTHORIZED
        );
        let (status, body) = post_text(&app, "/api/import", Some(&bob), old).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let first: BoardView = serde_json::from_str(&body).unwrap();
        assert!(first.started);
        assert_eq!(first.board.tasks[0].title, "From the old app");
        assert_eq!(first.board.tasks[0].notes[0].text, "still to do");
        assert_eq!(first.board.slots[0], Some(first.board.tasks[0].id));
        // Again: the same tasks once more, under new ids.
        let (_, body) = post_text(&app, "/api/import", Some(&bob), old).await;
        let twice: BoardView = serde_json::from_str(&body).unwrap();
        assert_eq!(twice.board.tasks.len(), 2);
        assert_ne!(twice.board.tasks[0].id, twice.board.tasks[1].id);
        let (status, body) = post_text(&app, "/api/import", Some(&bob), "not json").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("its4tasks.json"), "{body}");

        // What the app exports reads back in.
        let file = import::export(&first.board, now());
        let (status, body) = post_text(&app, "/api/import", Some(&token("carol")), &file).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let carol: BoardView = serde_json::from_str(&body).unwrap();
        assert_eq!(carol.board.tasks[0].title, "From the old app");
        assert_ne!(carol.board.tasks[0].id, first.board.tasks[0].id, "new ids");
    }

    #[tokio::test]
    async fn other_sites_pages_cant_change_boards() {
        let app = app();
        let alice = token("alice");
        let import = |headers: &[(&str, &str)]| {
            let mut request = Request::post("/api/start")
                .header(header::HOST, "app.test")
                .header(header::COOKIE, format!("__session={alice}"));
            for (name, value) in headers {
                request = request.header(*name, *value);
            }
            request.body(Body::empty()).unwrap()
        };
        for refused in [
            &[("sec-fetch-site", "cross-site")][..],
            &[("sec-fetch-site", "same-site")],
            &[("origin", "https://evil.test")],
            &[("origin", "null")],
        ] {
            let (status, _, _) = call(&app, import(refused)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{refused:?}");
        }
        assert!(!view(&app, &alice).await.started, "nothing started");
        // The board's own page, and apps that don't say.
        let (status, _, _) = call(&app, import(&[("origin", "http://app.test")])).await;
        assert_eq!(status, StatusCode::OK);
        for allowed in [&[("sec-fetch-site", "same-origin")][..], &[]] {
            let (status, _, _) = call(&app, import(allowed)).await;
            assert_eq!(status, StatusCode::CONFLICT, "let through: {allowed:?}");
        }
        // Reading is fine from anywhere: the browser keeps the answer from
        // other sites.
        let request = Request::get("/api/board")
            .header(header::AUTHORIZATION, format!("Bearer {alice}"))
            .header("sec-fetch-site", "cross-site")
            .body(Body::empty())
            .unwrap();
        assert_eq!(call(&app, request).await.0, StatusCode::OK);
    }

    #[tokio::test]
    async fn pages_cant_be_framed() {
        let app = app();
        for path in ["/", "/api/auth", "/favicon.svg"] {
            let (_, headers, _) = call(&app, Request::get(path).body(Body::empty()).unwrap()).await;
            assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY", "{path}");
            assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff", "{path}");
            assert_eq!(
                headers[header::CONTENT_SECURITY_POLICY],
                "frame-ancestors 'none'",
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn oversize_changes_are_refused() {
        let app = app();
        let alice = token("alice");
        let long = "x".repeat(its4tasks_core::api::MAX_TITLE + 1);
        let add = Change::add_task(long, String::new(), None, false);
        let (status, body) = post(&app, &alice, &add).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        let rejected: Rejected = serde_json::from_str(&body).unwrap();
        assert_eq!(rejected.error, ApiError::TooBig);
    }

    #[tokio::test]
    async fn the_page_has_an_icon() {
        let app = app();
        let (status, headers, svg) = call(
            &app,
            Request::get("/favicon.svg").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::CONTENT_TYPE], "image/svg+xml");
        assert!(svg.starts_with("<svg"));
        assert!(PAGE.contains(r#"href="/favicon.svg""#));
    }

    #[tokio::test]
    async fn app_files_are_revalidated() {
        // Only with the board app built (debug builds read it from disk).
        let Some(path) = Web::iter().find(|p| p != "index.html") else {
            return;
        };
        let app = app();
        let (status, headers, _) = call(
            &app,
            Request::get(format!("/{path}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::CACHE_CONTROL], "no-cache");
        let etag = headers[header::ETAG].clone();
        let request = Request::get(format!("/{path}"))
            .header(header::IF_NONE_MATCH, etag)
            .body(Body::empty())
            .unwrap();
        assert_eq!(call(&app, request).await.0, StatusCode::NOT_MODIFIED);
    }

    #[tokio::test]
    async fn apps_learn_how_to_sign_in() {
        let (status, body) = get(&app(), "/api/auth", None).await;
        assert_eq!(status, StatusCode::OK);
        let oauth: OAuth = serde_json::from_str(&body).unwrap();
        assert_eq!(oauth, config().oauth.unwrap());
    }

    #[tokio::test]
    async fn the_local_app_needs_its_secret() {
        let app = router(
            Api::new(),
            Auth::Local {
                secret: "s3cret".into(),
            },
        );
        assert_eq!(
            get(&app, "/api/board", None).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            get(&app, "/api/board", Some("guess")).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            get(&app, "/api/board", Some("s3cret")).await.0,
            StatusCode::OK
        );
        assert_eq!(get(&app, "/api/auth", None).await.0, StatusCode::NOT_FOUND);
    }
}
