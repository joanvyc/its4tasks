//! `Api` over HTTP, with JSON bodies:
//! - `GET /board`: the `Board`.
//! - `POST /changes` with a `Change`: an `Applied`, or a `Rejected` with
//!   404 (no such task or comment) or 409 (the board's rules refuse it, or
//!   a new id is taken).

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

/// Why a change was turned down, and the board it was turned down by:
/// often the client's copy was out of date.
#[derive(Debug, Serialize, Deserialize)]
pub struct Rejected {
    pub error: ApiError,
    pub board: Board,
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
                ApiError::Move(_) | ApiError::Duplicate(_) | ApiError::Refused => {
                    StatusCode::CONFLICT
                }
            };
            let board = api.board().clone();
            (status, Json(Rejected { error, board })).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use uuid::Uuid;

    use super::*;
    use crate::client::{Client, ClientError};
    use crate::model::MessageKind;

    const WAIT: Duration = Duration::from_secs(5);

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

    /// A client that has the server's board.
    fn client(url: &str) -> Client {
        let mut client = Client::new(url, WAIT, || {});
        assert!(client.flush(WAIT));
        client
    }

    fn add(client: &mut Client, title: &str) -> Uuid {
        match client.apply(Change::add_task(title.into(), String::new(), None, false)) {
            Ok(Outcome::Created(id)) => id,
            other => panic!("no task created: {other:?}"),
        }
    }

    #[test]
    fn changes_show_at_once_and_reach_the_server() {
        let url = start();
        let mut a = client(&url);
        let id = add(&mut a, "Write report");
        let post = Change::post(id, "first draft done".into(), MessageKind::Comment);
        assert_eq!(a.apply(post), Ok(Outcome::Done));
        // Shown before the server has answered either change.
        assert_eq!(
            a.board().task(id).unwrap().notes[0].text,
            "first draft done"
        );

        assert!(a.flush(WAIT));
        assert_eq!(a.error(), None);
        let b = client(&url);
        assert_eq!(b.board(), a.board(), "same ids on the server");
    }

    #[test]
    fn a_refused_change_is_undone() {
        let url = start();
        let mut a = client(&url);
        let id = add(&mut a, "Write report");
        assert!(a.flush(WAIT));
        let mut b = client(&url);
        b.apply(Change::DeleteTask { task: id }).unwrap();
        assert!(b.flush(WAIT));

        // `a` hasn't heard, so the comment shows until the server refuses it.
        let post = Change::post(id, "too late".into(), MessageKind::Comment);
        assert_eq!(a.apply(post), Ok(Outcome::Done));
        assert!(a.board().task(id).is_some());
        assert!(a.flush(WAIT));
        assert!(a.board().task(id).is_none());
        assert!(a.notice().is_some_and(|n| n.contains("no task")));
        assert_eq!(a.error(), None);
    }

    #[test]
    fn rules_broken_here_are_not_sent() {
        let url = start();
        let mut a = client(&url);
        let gone = Uuid::new_v4();
        assert_eq!(
            a.apply(Change::ToggleStar { note: gone }),
            Err(ClientError::Api(ApiError::UnknownNote(gone)))
        );
        assert!(a.flush(WAIT));
        assert_eq!(a.notice(), None);
    }

    #[test]
    fn quick_renames_never_go_backwards() {
        let url = start();
        let mut a = client(&url);
        let id = add(&mut a, "t");
        let mut title = String::from("t");
        for c in "abcdefghijklmnopqrst".chars() {
            title.push(c);
            let rename = Change::Rename {
                task: id,
                title: title.clone(),
            };
            a.apply(rename).unwrap();
            // Answers to earlier renames arrive while typing.
            a.poll();
            assert_eq!(a.board().task(id).unwrap().title, title);
        }
        assert!(a.flush(WAIT));
        assert_eq!(a.board().task(id).unwrap().title, title);
        assert_eq!(client(&url).board().task(id).unwrap().title, title);
    }

    #[test]
    fn a_missing_server_undoes_changes() {
        // Nothing listens on a port that was just freed.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut a = Client::new(&format!("http://127.0.0.1:{port}"), WAIT, || {});
        assert!(a.flush(WAIT));
        assert!(a.error().is_some());
        let id = add(&mut a, "offline");
        assert!(a.board().task(id).is_some());
        assert!(a.flush(WAIT));
        assert!(a.board().task(id).is_none());
        assert!(a.error().is_some());
    }

    #[test]
    fn a_hanging_server_times_out() {
        // Accepts connections and never answers.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let held: Vec<_> = listener.incoming().collect();
            drop(held);
        });
        let mut a = Client::new(&url, Duration::from_millis(300), || {});
        let id = add(&mut a, "slow");
        assert!(a.board().task(id).is_some(), "shown without waiting");
        assert!(a.flush(WAIT));
        assert!(a.board().task(id).is_none());
        assert!(a.error().is_some());
    }
}
