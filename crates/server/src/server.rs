//! The board and the web GUI over HTTP:
//! - `GET /api/board`: the `Board`, as JSON.
//! - `POST /api/changes` with a `Change`: an `Applied`, or a `Rejected` with
//!   404 (no such task or comment) or 409 (the board's rules refuse it, or
//!   a new id is taken).
//! - Anything else: the web GUI's files, built by `trunk build` in
//!   `crates/gui` and embedded in release builds. `/` is its `index.html`.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use its4tasks_core::api::{ApiError, Applied, Change, Rejected};
use rust_embed::Embed;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

use crate::api::{Api, Failure};
use crate::storage::LOCAL;

/// Changes are applied one at a time.
type Shared = Arc<Mutex<Api>>;

/// The web GUI (see build.rs). Debug builds read it from disk on each
/// request, so a new `trunk build` shows without rebuilding the server.
#[derive(Embed)]
#[folder = "../gui/dist"]
struct Web;

pub fn router(api: Api) -> Router {
    let api = Router::new()
        .route("/board", get(board))
        .route("/changes", post(change))
        .with_state(Arc::new(Mutex::new(api)));
    Router::new().nest("/api", api).fallback(web)
}

pub async fn serve(listener: TcpListener, api: Api) -> std::io::Result<()> {
    axum::serve(listener, router(api)).await
}

async fn board(State(api): State<Shared>) -> Response {
    let mut api = api.lock().await;
    let result = async {
        let user = api.sign_in(LOCAL, LOCAL).await?;
        Ok::<_, Failure>(api.board(&user).await?.1.clone())
    }
    .await;
    match result {
        Ok(board) => Json(board).into_response(),
        Err(e) => internal(e),
    }
}

async fn change(State(api): State<Shared>, Json(change): Json<Change>) -> Response {
    let mut api = api.lock().await;
    let user = match api.sign_in(LOCAL, LOCAL).await {
        Ok(user) => user,
        Err(e) => return internal(e.into()),
    };
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

/// A failure that isn't the client's doing.
fn internal(e: Failure) -> Response {
    eprintln!("its4tasks-server: {e}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Something went wrong on the server.",
    )
        .into_response()
}

async fn web(uri: Uri) -> Response {
    let path = match uri.path().trim_start_matches('/') {
        "" => "index.html",
        path => path,
    };
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

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use its4tasks_core::model::Board;
    use tower::ServiceExt;

    use super::*;

    /// Answers one request on a fresh, unsaved board.
    async fn get(path: &str) -> (StatusCode, String) {
        let request = Request::get(path).body(Body::empty()).unwrap();
        let response = router(Api::new()).oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    #[tokio::test]
    async fn the_api_lives_under_api() {
        let (status, body) = get("/api/board").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            serde_json::from_str::<Board>(&body).unwrap(),
            Board::default()
        );
        assert_eq!(get("/board").await.0, StatusCode::NOT_FOUND);
        assert_eq!(get("/no/such/file.js").await.0, StatusCode::NOT_FOUND);
    }
}
