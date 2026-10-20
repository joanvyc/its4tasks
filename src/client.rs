//! The GUI's side of the API (see `server`).
//!
//! An edit shows at once. `apply` applies the change to a local copy of the
//! board with the server's own rules (`api::apply_change`) and queues it.
//! A worker thread sends queued requests to the server one at a time, in
//! order, so the UI never waits on the network. Each answer carries the
//! server's board, which becomes the copy's base. Changes still on their way
//! are replayed on top of it. A change the server refuses, or can't be told
//! about, is dropped from the copy.

use std::collections::VecDeque;
use std::fmt;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use ureq::Agent;

use crate::api::{ApiError, Change, Outcome, apply_change};
use crate::model::Board;
use crate::server::{Applied, Rejected};

pub const DEFAULT_SERVER: &str = "http://127.0.0.1:7878";

/// How often `refresh` fetches the board, to show other clients' changes.
const REFRESH_EVERY: Duration = Duration::from_secs(2);

/// How long a request may take before the server counts as unreachable.
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

impl From<ureq::Error> for ClientError {
    fn from(e: ureq::Error) -> Self {
        ClientError::Http(e.to_string())
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
    requests: Sender<Request>,
    responses: Receiver<Response>,
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
    /// Connects to `$ITS4TASKS_SERVER`, or `DEFAULT_SERVER`. `wake` is called
    /// from the worker thread whenever an answer arrives, for `poll`.
    pub fn connect(wake: impl Fn() + Send + 'static) -> Client {
        let url = std::env::var("ITS4TASKS_SERVER").unwrap_or_else(|_| DEFAULT_SERVER.to_owned());
        Client::new(&url, TIMEOUT, wake)
    }

    /// Connects to the server at `url` and starts fetching the board, which
    /// is empty until it arrives.
    pub fn new(url: &str, timeout: Duration, wake: impl Fn() + Send + 'static) -> Client {
        let (requests, worker_requests) = mpsc::channel();
        let (worker_responses, responses) = mpsc::channel();
        let url = url.trim_end_matches('/').to_owned();
        thread::spawn(move || work(&url, timeout, worker_requests, worker_responses, wake));
        let mut client = Client {
            requests,
            responses,
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
        let at = Utc::now();
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
        while let Ok(response) = self.responses.try_recv() {
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
    /// `timeout`. True if everything was answered.
    pub fn flush(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while !self.pending.is_empty() || self.fetching {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.responses.recv_timeout(left) {
                Ok(response) => self.take(response),
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
            }
        }
        self.rebuild();
        self.pending.is_empty() && !self.fetching
    }

    fn send(&mut self, request: Request) {
        // The worker only stops once the client is dropped.
        let _ = self.requests.send(request);
    }

    /// Records one answer. The worker answers in the order it was asked, so
    /// an answer's board includes every change sent before it.
    fn take(&mut self, response: Response) {
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

/// The worker thread: answers requests one at a time, in order, until the
/// client is dropped.
fn work(
    url: &str,
    timeout: Duration,
    requests: Receiver<Request>,
    responses: Sender<Response>,
    wake: impl Fn(),
) {
    let agent: Agent = Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .build()
        .into();
    for request in requests {
        let response = match request {
            Request::Send(seq, change) => Response::Sent(seq, send(&agent, url, &change)),
            Request::Fetch => Response::Fetched(fetch(&agent, url)),
        };
        if responses.send(response).is_err() {
            return;
        }
        wake();
    }
}

fn fetch(agent: &Agent, url: &str) -> Result<Board, ClientError> {
    let mut response = agent.get(format!("{url}/board")).call()?;
    Ok(response.body_mut().read_json()?)
}

fn send(agent: &Agent, url: &str, change: &Change) -> Result<Answer, String> {
    let mut response = agent
        .post(format!("{url}/changes"))
        .send_json(change)
        .map_err(|e| e.to_string())?;
    let status = response.status();
    let body = response.body_mut();
    let answer = if status.is_success() {
        body.read_json().map(Answer::Applied)
    } else {
        body.read_json().map(Answer::Refused)
    };
    answer.map_err(|e| format!("server answered {status}: {e}"))
}
