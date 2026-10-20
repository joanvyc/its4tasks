//! The GUI's side of the API: the last board the server sent, and changes
//! sent to it over HTTP (see `server`).
//!
//! Requests block. On localhost they take about a millisecond, and the board
//! changes in the same frame as the edit, so typing into a title doesn't
//! lose characters to a stale board.

use std::fmt;
use std::time::{Duration, Instant};

use ureq::Agent;

use crate::api::{ApiError, Change, Outcome};
use crate::model::Board;
use crate::server::{Applied, Rejected};

pub const DEFAULT_SERVER: &str = "http://127.0.0.1:7878";

/// How often `refresh` fetches the board, to show other clients' changes.
const REFRESH_EVERY: Duration = Duration::from_secs(2);

/// Long enough for a busy server, short enough that a dead one doesn't
/// freeze the UI for long.
const TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq)]
pub enum ClientError {
    /// The server turned the change down.
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

pub struct Client {
    url: String,
    agent: Agent,
    board: Board,
    /// Why the last request failed, until one succeeds.
    error: Option<String>,
    fetched: Instant,
}

impl Client {
    /// Connects to `$ITS4TASKS_SERVER`, or `DEFAULT_SERVER`.
    pub fn connect() -> Client {
        let url = std::env::var("ITS4TASKS_SERVER").unwrap_or_else(|_| DEFAULT_SERVER.to_owned());
        Client::new(&url)
    }

    /// Connects to the server at `url`. If it can't be reached the board
    /// starts empty and `error` says why; `refresh` keeps trying.
    pub fn new(url: &str) -> Client {
        let agent = Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .http_status_as_error(false)
            .build()
            .into();
        let mut client = Client {
            url: url.trim_end_matches('/').to_owned(),
            agent,
            board: Board::default(),
            error: None,
            fetched: Instant::now(),
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

    pub fn apply(&mut self, change: Change) -> Result<Outcome, ClientError> {
        let result = self.send(&change);
        self.error = match &result {
            Err(ClientError::Http(e)) => Some(e.clone()),
            _ => None,
        };
        result
    }

    /// Fetches the board if it hasn't been fetched for a while.
    pub fn refresh(&mut self) {
        if self.fetched.elapsed() >= REFRESH_EVERY {
            self.fetch();
        }
    }

    /// Fetches the board now, however recently it was fetched.
    pub fn fetch_now(&mut self) {
        self.fetch();
    }

    fn fetch(&mut self) {
        self.fetched = Instant::now();
        let board = self
            .agent
            .get(format!("{}/board", self.url))
            .call()
            .and_then(|mut r| r.body_mut().read_json::<Board>());
        match board {
            Ok(board) => {
                self.board = board;
                self.error = None;
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    fn send(&mut self, change: &Change) -> Result<Outcome, ClientError> {
        let mut response = self
            .agent
            .post(format!("{}/changes", self.url))
            .send_json(change)?;
        if response.status().is_success() {
            let applied: Applied = response.body_mut().read_json()?;
            self.board = applied.board;
            self.fetched = Instant::now();
            Ok(applied.outcome)
        } else if let Ok(rejected) = response.body_mut().read_json::<Rejected>() {
            Err(ClientError::Api(rejected.error))
        } else {
            Err(ClientError::Http(format!(
                "server answered {}",
                response.status()
            )))
        }
    }
}
