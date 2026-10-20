//! `Api` over HTTP, with JSON bodies:
//! - `GET /board`: the `Board`.
//! - `POST /changes` with a `Change`: an `Applied`, or a `Rejected` with
//!   404 (no such task or comment) or 409 (the board's rules refuse it).

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

use crate::api::{Api, ApiError, Change, Outcome};
use crate::model::Board;

/// Changes are applied one at a time.
type Shared = Arc<Mutex<Api>>;

/// What a change did, and the board after it.
#[derive(Debug, Serialize, Deserialize)]
pub struct Applied {
    pub outcome: Outcome,
    pub board: Board,
}

/// Why a change was turned down.
#[derive(Debug, Serialize, Deserialize)]
pub struct Rejected {
    pub error: ApiError,
}

pub fn router(api: Api) -> Router {
    Router::new()
        .route("/board", get(board))
        .route("/changes", post(change))
        .with_state(Arc::new(Mutex::new(api)))
}

pub async fn serve(listener: TcpListener, api: Api) -> std::io::Result<()> {
    axum::serve(listener, router(api)).await
}

async fn board(State(api): State<Shared>) -> Json<Board> {
    Json(api.lock().await.board().clone())
}

async fn change(State(api): State<Shared>, Json(change): Json<Change>) -> Response {
    let mut api = api.lock().await;
    match api.apply(change).await {
        Ok(outcome) => Json(Applied {
            outcome,
            board: api.board().clone(),
        })
        .into_response(),
        Err(error) => {
            let status = match error {
                ApiError::UnknownTask(_) | ApiError::UnknownNote(_) => StatusCode::NOT_FOUND,
                ApiError::Move(_) | ApiError::Refused => StatusCode::CONFLICT,
            };
            (status, Json(Rejected { error })).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::client::{Client, ClientError};
    use crate::model::MessageKind;

    /// Serves an unsaved board on a free port, for as long as the test runs.
    fn start() -> String {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                tx.send(listener.local_addr().unwrap()).unwrap();
                serve(listener, Api::new(Board::default())).await.unwrap();
            });
        });
        format!("http://{}", rx.recv().unwrap())
    }

    #[test]
    fn clients_change_the_board_over_http() {
        let url = start();
        let mut a = Client::new(&url);
        let mut b = Client::new(&url);
        assert_eq!((a.error(), b.error()), (None, None));

        let add = Change::AddTask {
            title: "Write report".into(),
            notes: String::new(),
            parent: None,
            focus: true,
        };
        let Ok(Outcome::Created(id)) = a.apply(add) else {
            panic!("no task created");
        };
        let post = Change::Post {
            task: id,
            text: "first draft done".into(),
            kind: MessageKind::Comment,
        };
        assert_eq!(a.apply(post), Ok(Outcome::Done));
        // The answer carries the new board.
        let task = a.board().task(id).unwrap();
        assert_eq!(task.notes[0].text, "first draft done");
        assert_eq!(a.board().slots[0], Some(id));

        // The server's rules still apply.
        let gone = Uuid::new_v4();
        assert_eq!(
            a.apply(Change::ToggleStar { note: gone }),
            Err(ClientError::Api(ApiError::UnknownNote(gone)))
        );

        // Another client sees the change once it fetches.
        assert!(b.board().task(id).is_none());
        b.fetch_now();
        assert_eq!(b.board(), a.board());
    }

    #[test]
    fn a_missing_server_is_reported() {
        // Nothing listens on a port that was just freed.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut client = Client::new(&format!("http://127.0.0.1:{port}"));
        assert!(client.error().is_some());
        assert!(matches!(
            client.apply(Change::Unfocus { slot: 0 }),
            Err(ClientError::Http(_))
        ));
        assert!(client.error().is_some());
    }
}
