//! The GUI's side of the API (`GET /api/board`, `POST /api/changes`).
//!
//! An edit shows at once. `apply` applies the change to a local copy of the
//! board with the server's own rules (`api::apply_change`) and queues it.
//! Requests go out one at a time, in order, without the UI waiting on them:
//! through a thread per request natively, and `fetch()` in the browser
//! (`call`). Each
//! answer carries the server's board, which becomes the copy's base. Changes
//! still on their way are replayed on top of it. A change the server
//! refuses, or can't be told about, is dropped from the copy.

use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use chrono::{DateTime, Utc};
use web_time::Instant;

use crate::api::{self, ApiError, Applied, Change, Outcome, Rejected, apply_change};
use crate::model::Board;

/// Where the native GUI looks for the server unless `$ITS4TASKS_SERVER`
/// says otherwise. In the browser the server is the page's own.
pub const DEFAULT_SERVER: &str = "http://127.0.0.1:7878";

/// How often `refresh` fetches the board, to show changes made elsewhere
/// (say, the same board open in a browser).
const REFRESH_EVERY: Duration = Duration::from_secs(2);

/// How long a request may take before the server counts as unreachable.
/// Native only: in the browser a request takes as long as it takes.
const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq)]
pub enum ClientError {
    /// The board's rules refuse the change.
    Api(ApiError),
    /// The server couldn't be reached or answered nonsense.
    Http(String),
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ClientError::Api(e) => e.fmt(f),
            ClientError::Http(e) => f.write_str(e),
        }
    }
}

enum Request {
    Send(u64, Change),
    Fetch,
}

enum Response {
    /// The server's answer to a change, or why there was none.
    Sent(u64, Result<Answer, String>),
    Fetched(Result<Board, ClientError>),
}

enum Answer {
    Applied(Applied),
    Refused(Rejected),
}

/// A change applied to the copy but not yet answered by the server.
struct Pending {
    seq: u64,
    change: Change,
    /// When it was made, so replaying it gives the same result.
    at: DateTime<Utc>,
}

pub struct Client {
    /// Where the API is: the server's address, or "" for the page's own.
    url: String,
    timeout: Duration,
    /// Called when an answer arrives, for `poll`.
    wake: Arc<dyn Fn() + Send + Sync>,
    /// Requests not sent yet. One is on its way at a time, so answers come
    /// back in order.
    queue: VecDeque<Request>,
    in_flight: bool,
    answer_to: Sender<Response>,
    answers: Receiver<Response>,
    /// The last board the server sent.
    confirmed: Board,
    pending: VecDeque<Pending>,
    /// `confirmed` with `pending` applied: what the GUI shows.
    board: Board,
    next_seq: u64,
    fetching: bool,
    fetched: Instant,
    /// Why the server can't be reached, until it answers again.
    error: Option<String>,
    /// Why the server refused the last change it refused.
    notice: Option<String>,
}

impl Client {
    /// Connects to the page's own server in the browser, and natively to
    /// `$ITS4TASKS_SERVER` or `DEFAULT_SERVER`. `wake` is called, possibly
    /// from another thread, whenever an answer arrives, for `poll`.
    pub fn connect(wake: impl Fn() + Send + Sync + 'static) -> Client {
        let url = if cfg!(target_arch = "wasm32") {
            String::new()
        } else {
            std::env::var("ITS4TASKS_SERVER").unwrap_or_else(|_| DEFAULT_SERVER.to_owned())
        };
        Client::new(&url, TIMEOUT, wake)
    }

    /// Connects to the server at `url` and starts fetching the board, which
    /// is empty until it arrives.
    pub fn new(url: &str, timeout: Duration, wake: impl Fn() + Send + Sync + 'static) -> Client {
        let (answer_to, answers) = mpsc::channel();
        let mut client = Client {
            url: url.trim_end_matches('/').to_owned(),
            timeout,
            wake: Arc::new(wake),
            queue: VecDeque::new(),
            in_flight: false,
            answer_to,
            answers,
            confirmed: Board::default(),
            pending: VecDeque::new(),
            board: Board::default(),
            next_seq: 0,
            fetching: false,
            fetched: Instant::now(),
            error: None,
            notice: None,
        };
        client.fetch();
        client
    }

    pub fn board(&self) -> &Board {
        &self.board
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Applies the change to the board shown and sends it to the server.
    /// Fails, without sending, if the board's rules refuse it here.
    pub fn apply(&mut self, change: Change) -> Result<Outcome, ClientError> {
        let at = api::now();
        let outcome =
            apply_change(&mut self.board, change.clone(), at).map_err(ClientError::Api)?;
        let seq = self.next_seq;
        self.next_seq += 1;
        self.pending.push_back(Pending {
            seq,
            change: change.clone(),
            at,
        });
        self.send(Request::Send(seq, change));
        Ok(outcome)
    }

    /// Takes in the server's answers. Call it every frame.
    pub fn poll(&mut self) {
        let mut changed = false;
        while let Ok(response) = self.answers.try_recv() {
            self.take(response);
            changed = true;
        }
        if changed {
            self.rebuild();
        }
    }

    /// Fetches the board if it hasn't been fetched for a while.
    pub fn refresh(&mut self) {
        if !self.fetching && self.fetched.elapsed() >= REFRESH_EVERY {
            self.fetch();
        }
    }

    /// Fetches the board now, unless a fetch is already on its way.
    pub fn fetch(&mut self) {
        if !self.fetching {
            self.fetching = true;
            self.fetched = Instant::now();
            self.send(Request::Fetch);
        }
    }

    /// Waits until every change and fetch sent so far is answered, or until
    /// `timeout`. True if everything was answered. Not in the browser, which
    /// can't wait.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn flush(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while !self.pending.is_empty() || self.fetching {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.answers.recv_timeout(left) {
                Ok(response) => self.take(response),
                Err(_) => break,
            }
        }
        self.rebuild();
        self.pending.is_empty() && !self.fetching
    }

    fn send(&mut self, request: Request) {
        self.queue.push_back(request);
        self.next_request();
    }

    /// Sends the next queued request, unless one is still on its way.
    fn next_request(&mut self) {
        if self.in_flight {
            return;
        }
        let Some(request) = self.queue.pop_front() else {
            return;
        };
        self.in_flight = true;
        let (answer_to, wake) = (self.answer_to.clone(), self.wake.clone());
        let answer = move |response| {
            // The client may be gone, say while the GUI closes.
            let _ = answer_to.send(response);
            wake();
        };
        let timeout = self.timeout;
        match request {
            Request::Fetch => {
                let url = format!("{}/api/board", self.url);
                call(url, None, timeout, move |reply| {
                    answer(Response::Fetched(board_from(reply)));
                });
            }
            Request::Send(seq, change) => {
                let url = format!("{}/api/changes", self.url);
                call(url, Some(change), timeout, move |reply| {
                    answer(Response::Sent(seq, answer_from(reply)));
                });
            }
        }
    }

    /// Records one answer and sends the next request. Answers come in the
    /// order the requests were made, so an answer's board includes every
    /// change sent before it.
    fn take(&mut self, response: Response) {
        self.in_flight = false;
        match response {
            Response::Sent(seq, result) => {
                self.pending.retain(|p| p.seq != seq);
                match result {
                    Ok(Answer::Applied(applied)) => {
                        self.confirmed = applied.board;
                        self.error = None;
                    }
                    Ok(Answer::Refused(rejected)) => {
                        self.notice = Some(format!("Change refused: {}", rejected.error));
                        self.confirmed = rejected.board;
                        self.error = None;
                    }
                    Err(e) => {
                        // Whether the server saw these is unknown, and some
                        // changes (the toggles) can't safely be sent twice,
                        // so drop them. Answers to them still update
                        // `confirmed`, the server's word.
                        self.pending.clear();
                        self.error = Some(e);
                    }
                }
            }
            Response::Fetched(result) => {
                self.fetching = false;
                match result {
                    Ok(board) => {
                        self.confirmed = board;
                        self.error = None;
                    }
                    Err(e) => self.error = Some(e.to_string()),
                }
            }
        }
        self.next_request();
    }

    /// The server's board with the changes still on their way on top.
    /// Changes that no longer apply to it are skipped.
    fn rebuild(&mut self) {
        let mut board = self.confirmed.clone();
        for p in &self.pending {
            let _ = apply_change(&mut board, p.change.clone(), p.at);
        }
        self.board = board;
    }
}

/// What the server sent back.
struct Reply {
    status: u16,
    body: Vec<u8>,
}

impl Reply {
    fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// GETs `url`, or POSTs `change` to it as JSON, and hands the reply to
/// `done` on another thread.
#[cfg(not(target_arch = "wasm32"))]
fn call(
    url: String,
    change: Option<Change>,
    timeout: Duration,
    done: impl FnOnce(Result<Reply, String>) + Send + 'static,
) {
    std::thread::spawn(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .build()
            .into();
        let response = match change {
            None => agent.get(&url).call(),
            Some(change) => agent.post(&url).send_json(&change),
        };
        let reply = response.and_then(|mut response| {
            let status = response.status().as_u16();
            let body = response.body_mut().read_to_vec()?;
            Ok(Reply { status, body })
        });
        done(reply.map_err(|e| e.to_string()));
    });
}

/// GETs `url`, or POSTs `change` to it as JSON, with the browser's
/// `fetch()`, and hands the reply to `done` when it arrives.
#[cfg(target_arch = "wasm32")]
fn call(
    url: String,
    change: Option<Change>,
    _timeout: Duration,
    done: impl FnOnce(Result<Reply, String>) + Send + 'static,
) {
    let request = match change {
        None => ehttp::Request::get(url),
        Some(change) => ehttp::Request::post_json(url, &change).expect("a Change serializes"),
    };
    ehttp::fetch(request, move |response| {
        done(response.map(|r| Reply {
            status: r.status,
            body: r.bytes,
        }));
    });
}

fn board_from(reply: Result<Reply, String>) -> Result<Board, ClientError> {
    let reply = reply.map_err(ClientError::Http)?;
    if !reply.ok() {
        return Err(ClientError::Http(format!(
            "server answered {}",
            reply.status
        )));
    }
    serde_json::from_slice(&reply.body)
        .map_err(|e| ClientError::Http(format!("unreadable board: {e}")))
}

fn answer_from(reply: Result<Reply, String>) -> Result<Answer, String> {
    let reply = reply?;
    let answer = if reply.ok() {
        serde_json::from_slice(&reply.body).map(Answer::Applied)
    } else {
        serde_json::from_slice(&reply.body).map(Answer::Refused)
    };
    answer.map_err(|e| format!("server answered {}: {e}", reply.status))
}
