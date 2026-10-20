//! Users' boards and the web GUI over HTTP. Everything about a board needs
//! a signed-in user (see `auth`), and only ever reaches their own board.
//! - `GET /api/board`: the user's `BoardView`.
//! - `POST /api/changes` with a `Change`: an `Applied`, or a `Rejected` with
//!   404 (no such task or comment), 403 (it's on someone else's board) or
//!   409 (the board's rules refuse it, or a new id is taken).
//! - `GET /api/auth`: how the native app signs in (OAuth), if it can.
//!   Answered to anyone.
//! - Anything else: the web GUI's files, built by `trunk build` in
//!   `crates/gui` and embedded in release builds. `/` is its `index.html`,
//!   which sends anyone not signed in to the authority's sign-in page first.
//!
//! Without a valid token the API answers 401.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use its4tasks_core::api::{ApiError, Applied, BoardView, Change, Rejected};
use rust_embed::Embed;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

use crate::api::{Api, Failure};
use crate::auth::{Auth, Unauthorized};
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
        .route("/auth", get(auth_info));
    Router::new()
        .nest("/api", api)
        .fallback(web)
        .with_state(state)
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

async fn board(State(state): State<Shared>, headers: HeaderMap) -> Response {
    let user = match user(&state, &headers).await {
        Ok(user) => user,
        Err(response) => return *response,
    };
    let mut api = state.api.lock().await;
    match api.board(&user).await {
        Ok((started, board)) => Json(BoardView {
            started,
            board: board.clone(),
        })
        .into_response(),
        Err(e) => internal(e),
    }
}

async fn change(
    State(state): State<Shared>,
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
        Ok((_, board)) => board.clone(),
        Err(e) => return internal(e),
    };
    match result {
        Ok(outcome) => Json(Applied { outcome, board }).into_response(),
        Err(Failure::Api(error)) => {
            let status = match error {
                ApiError::UnknownTask(_) | ApiError::UnknownNote(_) => StatusCode::NOT_FOUND,
                ApiError::Forbidden(_) => StatusCode::FORBIDDEN,
                ApiError::Move(_) | ApiError::Duplicate(_) | ApiError::Refused => {
                    StatusCode::CONFLICT
                }
            };
            (status, Json(Rejected { error, board })).into_response()
        }
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

async fn web(State(state): State<Shared>, uri: Uri, headers: HeaderMap) -> Response {
    let path = match uri.path().trim_start_matches('/') {
        "" => "index.html",
        path => path,
    };
    // The page needs someone signed in; its code and fonts don't.
    if path == "index.html"
        && let Auth::Jwt(jwt) = &state.auth
        && state.auth.identify(&headers).await.is_err()
    {
        let back = page_url(&state, &headers, &uri);
        return Redirect::to(&jwt.config.sign_in(&back)).into_response();
    }
    match Web::get(path) {
        Some(file) => {
            let mime = file.metadata.mimetype().to_owned();
            ([(header::CONTENT_TYPE, mime)], file.data).into_response()
        }
        None if path == "index.html" => (
            StatusCode::NOT_FOUND,
            "The web GUI isn't built: run `trunk build` in crates/gui.",
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
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
            sign_in_url: "https://auth.test/sign-in?redirect={redirect}".into(),
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
        let request = Request::post("/api/changes")
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
        // Signed in through the cookie, the page is served (or explains it
        // isn't built).
        let request = Request::get("/")
            .header(header::COOKIE, format!("__session={}", token("alice")))
            .body(Body::empty())
            .unwrap();
        let (status, _, _) = call(&app, request).await;
        assert!(matches!(status, StatusCode::OK | StatusCode::NOT_FOUND));
        // The page's code needs no sign-in.
        assert_eq!(
            get(&app, "/no/such/file.js", None).await.0,
            StatusCode::NOT_FOUND
        );
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
