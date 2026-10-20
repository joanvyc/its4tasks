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

use crate::api::{self, ApiError, Applied, BoardView, Change, Outcome, Rejected, apply_change};
use crate::model::Board;

/// Where the native GUI looks for the server unless `$ITS4TASKS_SERVER`
/// says otherwise. In the browser the server is the page's own.
pub const DEFAULT_SERVER: &str = "http://127.0.0.1:7878";

/// How often `refresh` fetches the board, to show changes made elsewhere
/// (say, the same board open in a browser).
const REFRESH_EVERY: Duration = Duration::from_secs(2);

/// How long a request may take before the server counts as unreachable.
/// Native only: in the browser a request takes as long as it takes.
pub const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq)]
pub enum ClientError {
    /// The board's rules refuse the change.
    Api(ApiError),
    /// The server couldn't be reached or answered nonsense.
    Http(String),
    /// The server wants someone signed in: the token is missing, expired
    /// or not trusted.
    Unauthorized,
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ClientError::Api(e) => e.fmt(f),
            ClientError::Http(e) => f.write_str(e),
            ClientError::Unauthorized => f.write_str("not signed in"),
        }
    }
}

enum Request {
    Send(u64, Change),
    Fetch,
}

enum Response {
    /// The server's answer to a change, or why there was none.
    Sent(u64, Result<Answer, ClientError>),
    Fetched(Result<BoardView, ClientError>),
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
    /// Sent as `Authorization: Bearer`. The browser sends its session
    /// cookie instead.
    token: Option<String>,
    timeout: Duration,
    /// Called when an answer arrives, for `poll`.
    wake: Arc<dyn Fn() + Send + Sync>,
    /// Requests not sent yet. One is on its way at a time, so answers come
    /// back in order.
    queue: VecDeque<Request>,
    in_flight: bool,
    /// Answers, each with the `token_gen` its request was sent with.
    answer_to: Sender<(u64, Response)>,
    answers: Receiver<(u64, Response)>,
    /// The last board the server sent.
    confirmed: Board,
    pending: VecDeque<Pending>,
    /// `confirmed` with `pending` applied: what the GUI shows.
    board: Board,
    /// Whether the user has a board yet: new users choose to start one or
    /// import one.
    started: bool,
    /// Whether the server has answered at all, so `board` and `started`
    /// are its.
    loaded: bool,
    /// The server's last answer was that nobody's signed in.
    unauthorized: bool,
    /// Counts the tokens set, so that a refusal of an earlier one isn't
    /// taken for a refusal of this one.
    token_gen: u64,
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
        Client::new(&url, None, TIMEOUT, wake)
    }

    /// Connects to the server at `url`, signed in with the bearer `token`
    /// (the browser signs in with its cookie instead), and starts fetching
    /// the board, which is empty until it arrives.
    pub fn new(
        url: &str,
        token: Option<String>,
        timeout: Duration,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Client {
        let (answer_to, answers) = mpsc::channel();
        let mut client = Client {
            url: url.trim_end_matches('/').to_owned(),
            token,
            timeout,
            wake: Arc::new(wake),
            queue: VecDeque::new(),
            in_flight: false,
            answer_to,
            answers,
            confirmed: Board::default(),
            pending: VecDeque::new(),
            board: Board::default(),
            started: true,
            loaded: false,
            unauthorized: false,
            token_gen: 0,
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

    /// False for a new user, until they start a board or import one.
    pub fn started(&self) -> bool {
        self.started
    }

    /// Whether the server has answered yet.
    pub fn loaded(&self) -> bool {
        self.loaded
    }

    /// Whether the server's last answer was that nobody's signed in. Sign
    /// in again (`set_token`, or in the browser, reload the page).
    pub fn unauthorized(&self) -> bool {
        self.unauthorized
    }

    /// Sends `token` as the bearer token from now on, say after it was
    /// refreshed.
    pub fn set_token(&mut self, token: Option<String>) {
        self.token = token;
        self.token_gen += 1;
        // The server hasn't seen this one yet.
        self.unauthorized = false;
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
        // The server counts any change as starting the board.
        self.started = true;
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
        let token_gen = self.token_gen;
        let answer = move |response| {
            // The client may be gone, say while the GUI closes.
            let _ = answer_to.send((token_gen, response));
            wake();
        };
        let (timeout, token) = (self.timeout, self.token.clone());
        match request {
            Request::Fetch => {
                let url = format!("{}/api/board", self.url);
                call(url, None, token, timeout, move |reply| {
                    answer(Response::Fetched(board_from(reply)));
                });
            }
            Request::Send(seq, change) => {
                let url = format!("{}/api/changes", self.url);
                call(url, Some(change), token, timeout, move |reply| {
                    answer(Response::Sent(seq, answer_from(reply)));
                });
            }
        }
    }

    /// Records one answer and sends the next request. Answers come in the
    /// order the requests were made, so an answer's board includes every
    /// change sent before it.
    fn take(&mut self, (token_gen, response): (u64, Response)) {
        self.in_flight = false;
        match response {
            Response::Sent(seq, result) => {
                self.pending.retain(|p| p.seq != seq);
                match result {
                    Ok(Answer::Applied(applied)) => {
                        self.confirmed = applied.board;
                        self.answered();
                    }
                    Ok(Answer::Refused(rejected)) => {
                        self.notice = Some(format!("Change refused: {}", rejected.error));
                        self.confirmed = rejected.board;
                        self.answered();
                    }
                    Err(e) => {
                        // Whether the server saw these is unknown, and some
                        // changes (the toggles) can't safely be sent twice,
                        // so drop them. Answers to them still update
                        // `confirmed`, the server's word.
                        self.pending.clear();
                        self.failed(e, token_gen);
                    }
                }
            }
            Response::Fetched(result) => {
                self.fetching = false;
                match result {
                    Ok(view) => {
                        self.confirmed = view.board;
                        self.started = view.started || !self.pending.is_empty();
                        self.answered();
                    }
                    Err(e) => self.failed(e, token_gen),
                }
            }
        }
        self.next_request();
    }

    fn answered(&mut self) {
        self.loaded = true;
        self.error = None;
        self.unauthorized = false;
    }

    fn failed(&mut self, e: ClientError, token_gen: u64) {
        self.unauthorized = e == ClientError::Unauthorized && token_gen == self.token_gen;
        self.error = Some(e.to_string());
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

/// GETs `url`, or POSTs `change` to it as JSON, with `token` as the bearer
/// token, and hands the reply to `done` on another thread.
#[cfg(not(target_arch = "wasm32"))]
fn call(
    url: String,
    change: Option<Change>,
    token: Option<String>,
    timeout: Duration,
    done: impl FnOnce(Result<Reply, String>) + Send + 'static,
) {
    std::thread::spawn(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .build()
            .into();
        let bearer = token.map(|t| format!("Bearer {t}"));
        let response = match change {
            None => {
                let request = agent.get(&url);
                match &bearer {
                    Some(b) => request.header("Authorization", b).call(),
                    None => request.call(),
                }
            }
            Some(change) => {
                let request = agent.post(&url);
                match &bearer {
                    Some(b) => request.header("Authorization", b).send_json(&change),
                    None => request.send_json(&change),
                }
            }
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
/// `fetch()`, and hands the reply to `done` when it arrives. It's signed in
/// with the page's token if the page has a way to get one (see
/// `page_token`), and otherwise with the session cookie.
#[cfg(target_arch = "wasm32")]
fn call(
    url: String,
    change: Option<Change>,
    _token: Option<String>,
    _timeout: Duration,
    done: impl FnOnce(Result<Reply, String>) + Send + 'static,
) {
    let request = match change {
        None => ehttp::Request::get(url),
        Some(change) => ehttp::Request::post_json(url, &change).expect("a Change serializes"),
    };
    // ehttp leaves cookies out unless told, unlike the browser's fetch().
    let mut request = request.with_credentials(ehttp::Credentials::SameOrigin);
    wasm_bindgen_futures::spawn_local(async move {
        match page_token().await {
            Err(e) => return done(Err(e)),
            Ok(PageToken::Cookie) => {}
            Ok(PageToken::Bearer(token)) => {
                request
                    .headers
                    .insert("Authorization", format!("Bearer {token}"));
            }
            // On its way to sign in.
            Ok(PageToken::SignedOut) => {
                return done(Ok(Reply {
                    status: 401,
                    body: Vec::new(),
                }));
            }
        }
        let response = ehttp::fetch_async(request).await;
        done(response.map(|r| Reply {
            status: r.status,
            body: r.bytes,
        }));
    });
}

/// How a request from the page is signed in.
#[cfg(target_arch = "wasm32")]
enum PageToken {
    /// With the session cookie: the page has no `its4tasksAuth`.
    Cookie,
    Bearer(String),
    /// Nobody is: the browser has been sent to sign in.
    SignedOut,
}

/// A token from the page's `window.its4tasksAuth`, which the server adds
/// for sign-in services that keep tokens fresh in the page, such as Clerk:
/// `token()` gives a promise of a fresh token, or of null when signed out,
/// and `signIn()` goes to sign in.
#[cfg(target_arch = "wasm32")]
async fn page_token() -> Result<PageToken, String> {
    use eframe::wasm_bindgen::{JsCast, JsValue};
    use js_sys::{Function, Promise, Reflect};

    let failed = |e: JsValue| format!("signing in: {e:?}");
    let window = web_sys::window().ok_or("no window")?;
    let auth = Reflect::get(&window, &"its4tasksAuth".into()).map_err(failed)?;
    if auth.is_undefined() || auth.is_null() {
        return Ok(PageToken::Cookie);
    }
    let call = |name: &str| -> Result<JsValue, String> {
        let method: Function = Reflect::get(&auth, &name.into())
            .map_err(failed)?
            .dyn_into()
            .map_err(failed)?;
        method.call0(&auth).map_err(failed)
    };
    let promise: Promise = call("token")?.dyn_into().map_err(failed)?;
    let token = wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .map_err(failed)?;
    match token.as_string() {
        Some(token) => Ok(PageToken::Bearer(token)),
        None => {
            call("signIn")?;
            Ok(PageToken::SignedOut)
        }
    }
}

fn board_from(reply: Result<Reply, String>) -> Result<BoardView, ClientError> {
    let reply = reply.map_err(ClientError::Http)?;
    if reply.status == 401 {
        return Err(ClientError::Unauthorized);
    }
    if !reply.ok() {
        return Err(ClientError::Http(format!(
            "server answered {}",
            reply.status
        )));
    }
    serde_json::from_slice(&reply.body)
        .map_err(|e| ClientError::Http(format!("unreadable board: {e}")))
}

fn answer_from(reply: Result<Reply, String>) -> Result<Answer, ClientError> {
    let reply = reply.map_err(ClientError::Http)?;
    if reply.status == 401 {
        return Err(ClientError::Unauthorized);
    }
    let answer = if reply.ok() {
        serde_json::from_slice(&reply.body).map(Answer::Applied)
    } else {
        serde_json::from_slice(&reply.body).map(Answer::Refused)
    };
    answer.map_err(|e| ClientError::Http(format!("server answered {}: {e}", reply.status)))
}
