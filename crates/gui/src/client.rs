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
//!
//! The server leaves archived tasks out of its boards (see
//! `Board::archived`) unless asked for them: `set_archived`.

use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use chrono::{DateTime, Utc};
use web_time::Instant;

use crate::api::{self, ApiError, Applied, BoardView, Change, Outcome, Rejected, apply_change};
use crate::model::Board;
use uuid::Uuid;

/// Where the native GUI looks for the server unless `$ITS4TASKS_SERVER`
/// says otherwise. In the browser the server is the page's own.
pub const DEFAULT_SERVER: &str = "http://127.0.0.1:7878";

/// How long after the server last sent the board `refresh` fetches it, to
/// show changes made elsewhere. Rarely needed: one person uses one device at
/// a time, and each change's answer brings the board anyway.
const REFRESH_EVERY: Duration = Duration::from_secs(60);

/// How often `refresh` retries while the server refuses, so held changes go
/// out soon after a token is renewed.
const RETRY_EVERY: Duration = Duration::from_secs(2);

/// How long the server must keep refusing before the user counts as signed
/// out. Shorter refusals happen while a token is renewed, and are retried.
const SIGNED_OUT_AFTER: Duration = Duration::from_secs(10);

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

/// How a request was sent, as its answer comes back.
struct Asked {
    token_gen: u64,
    /// With archived tasks.
    archived: bool,
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
    /// Answers, each with how its request was sent.
    answer_to: Sender<(Asked, Response)>,
    answers: Receiver<(Asked, Response)>,
    /// Whether to ask for archived tasks too.
    archived: bool,
    /// Whether `confirmed` has the archived tasks.
    has_archived: bool,
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
    /// Whose board it is, once the server has said.
    user: Option<Uuid>,
    /// The server's last answer was that nobody's signed in. Requests wait
    /// in `queue` meanwhile, and are retried with a fresh token.
    unauthorized: bool,
    /// Since when the server has been refusing, for `signed_out`.
    unauthorized_since: Option<Instant>,
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
            archived: false,
            has_archived: false,
            confirmed: Board::default(),
            pending: VecDeque::new(),
            board: Board::default(),
            started: true,
            loaded: false,
            user: None,
            unauthorized: false,
            unauthorized_since: None,
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

    /// Whose board it is, as the server knows them, once it has said.
    pub fn user(&self) -> Option<Uuid> {
        self.user
    }

    /// The changes held while the server refuses, which it hasn't applied:
    /// to keep them when the page goes to sign in again. Empty otherwise,
    /// as changes on their way may have been applied.
    pub fn held(&self) -> Vec<Change> {
        if !self.unauthorized {
            return Vec::new();
        }
        self.pending.iter().map(|p| p.change.clone()).collect()
    }

    /// Whether the server's last answer was that nobody's signed in.
    /// Requests are held until a new token (`set_token`, or in the browser,
    /// the page's) works.
    pub fn unauthorized(&self) -> bool {
        self.unauthorized
    }

    /// Whether the server has refused for long enough that the user must
    /// have been signed out, rather than a token being renewed.
    pub fn signed_out(&self) -> bool {
        self.unauthorized
            && self
                .unauthorized_since
                .is_some_and(|since| since.elapsed() >= SIGNED_OUT_AFTER)
    }

    /// Sends `token` as the bearer token from now on, say after it was
    /// refreshed, and sends what was held for want of one.
    pub fn set_token(&mut self, token: Option<String>) {
        self.token = token;
        self.token_gen += 1;
        // The server hasn't seen this one yet.
        self.unauthorized = false;
        self.next_request(false);
    }

    /// Whether archived tasks are asked for.
    pub fn archived(&self) -> bool {
        self.archived
    }

    /// Whether the board shown has the archived tasks: once the server has
    /// answered a request that asked for them.
    pub fn has_archived(&self) -> bool {
        self.has_archived
    }

    /// Asks for archived tasks from now on, fetching them now; or leaves
    /// them out, at once.
    pub fn set_archived(&mut self, on: bool) {
        if on == self.archived {
            return;
        }
        self.archived = on;
        if on {
            // Even if a fetch is on its way: it may not have asked.
            self.fetching = true;
            self.fetched = Instant::now();
            self.send(Request::Fetch);
        } else {
            self.has_archived = false;
            self.confirmed = self.confirmed.without_archived(api::now());
            self.rebuild();
        }
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

    /// Fetches the board if it hasn't been fetched for a while. While the
    /// server refuses, retries instead, with a fresh token in the browser.
    pub fn refresh(&mut self) {
        let every = if self.unauthorized {
            RETRY_EVERY
        } else {
            REFRESH_EVERY
        };
        if self.fetched.elapsed() < every {
            return;
        }
        if self.unauthorized {
            if !self.in_flight {
                self.fetched = Instant::now();
                if self.queue.is_empty() {
                    self.fetching = true;
                    self.queue.push_back(Request::Fetch);
                }
                self.next_request(true);
            }
        } else if !self.fetching {
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
        self.next_request(false);
    }

    /// Sends the next queued request, unless one is still on its way or the
    /// server is refusing. `retry` sends it anyway, as a retry after a
    /// refusal, which in the browser asks the page for a fresh token.
    fn next_request(&mut self, retry: bool) {
        if self.in_flight || (self.unauthorized && !retry) {
            return;
        }
        let Some(request) = self.queue.pop_front() else {
            return;
        };
        self.in_flight = true;
        let (answer_to, wake) = (self.answer_to.clone(), self.wake.clone());
        let asked = Asked {
            token_gen: self.token_gen,
            archived: self.archived,
        };
        let query = if self.archived { "?archived=true" } else { "" };
        let answer = move |response| {
            // The client may be gone, say while the GUI closes.
            let _ = answer_to.send((asked, response));
            wake();
        };
        let (timeout, token) = (self.timeout, self.token.clone());
        match request {
            Request::Fetch => {
                let url = format!("{}/api/board{query}", self.url);
                call(url, None, token, retry, timeout, move |reply| {
                    answer(Response::Fetched(board_from(reply)));
                });
            }
            Request::Send(seq, change) => {
                let url = format!("{}/api/changes{query}", self.url);
                call(url, Some(change), token, retry, timeout, move |reply| {
                    answer(Response::Sent(seq, answer_from(reply)));
                });
            }
        }
    }

    /// Records one answer and sends the next request. Answers come in the
    /// order the requests were made, so an answer's board includes every
    /// change sent before it.
    fn take(&mut self, (asked, response): (Asked, Response)) {
        let token_gen = asked.token_gen;
        self.in_flight = false;
        match response {
            // The server didn't apply it: hold it, and send it again once
            // signed in.
            Response::Sent(seq, Err(ClientError::Unauthorized)) => {
                if let Some(p) = self.pending.iter().find(|p| p.seq == seq) {
                    self.queue.push_front(Request::Send(seq, p.change.clone()));
                }
                self.failed(ClientError::Unauthorized, token_gen);
            }
            Response::Sent(seq, result) => {
                self.pending.retain(|p| p.seq != seq);
                match result {
                    Ok(Answer::Applied(applied)) => {
                        self.confirm(applied.board, asked.archived);
                        self.answered();
                        self.fetched = Instant::now();
                    }
                    Ok(Answer::Refused(rejected)) => {
                        self.notice = Some(format!("Change refused: {}", rejected.error));
                        self.confirm(rejected.board, asked.archived);
                        self.answered();
                        self.fetched = Instant::now();
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
                        self.user = view.user.or(self.user);
                        self.confirm(view.board, asked.archived);
                        self.started = view.started || !self.pending.is_empty();
                        self.answered();
                    }
                    Err(e) => self.failed(e, token_gen),
                }
            }
        }
        self.next_request(false);
    }

    /// Takes the server's board, sent with archived tasks or not. Archived
    /// tasks asked for before `set_archived(false)` are left out.
    fn confirm(&mut self, board: Board, archived: bool) {
        self.has_archived = archived && self.archived;
        self.confirmed = if archived && !self.archived {
            board.without_archived(api::now())
        } else {
            board
        };
    }

    fn answered(&mut self) {
        self.loaded = true;
        self.error = None;
        self.unauthorized = false;
        self.unauthorized_since = None;
    }

    fn failed(&mut self, e: ClientError, token_gen: u64) {
        if e != ClientError::Unauthorized {
            self.error = Some(e.to_string());
        } else if token_gen == self.token_gen {
            // A refusal of an earlier token says nothing of this one.
            self.unauthorized = true;
            self.unauthorized_since.get_or_insert_with(Instant::now);
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

/// The board at `url`, signed in with `token`, with its archived tasks if
/// `archived`, waiting for it here: for `its4tasks --get-board` and
/// `--get-board-archived`, which have no window to keep responsive.
#[cfg(not(target_arch = "wasm32"))]
pub fn fetch_board(url: &str, token: &str, archived: bool) -> Result<BoardView, ClientError> {
    let (tx, rx) = mpsc::channel();
    let query = if archived { "?archived=true" } else { "" };
    let url = format!("{url}/api/board{query}");
    call(
        url,
        None,
        Some(token.to_owned()),
        false,
        TIMEOUT,
        move |reply| {
            let _ = tx.send(board_from(reply));
        },
    );
    rx.recv()
        .unwrap_or_else(|_| Err(ClientError::Http("the request went nowhere".into())))
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
    _retry: bool,
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
/// `page_token`; a fresh one for a `retry`), and otherwise with the session
/// cookie.
#[cfg(target_arch = "wasm32")]
fn call(
    url: String,
    change: Option<Change>,
    _token: Option<String>,
    retry: bool,
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
        match page_token(retry).await {
            Err(e) => return done(Err(e)),
            Ok(PageToken::Cookie) => {}
            Ok(PageToken::Bearer(token)) => {
                request
                    .headers
                    .insert("Authorization", format!("Bearer {token}"));
            }
            // Held like a refusal, and retried.
            Ok(PageToken::SignedOut) => {
                return done(Ok(Reply {
                    status: 401,
                    body: Vec::new(),
                }));
            }
        }
        let response = ehttp::fetch_async(request).await;
        if let Ok(r) = &response
            && r.status == 401
        {
            warn("its4tasks: the server refused the token; retrying with a fresh one");
        }
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
    /// Nobody is, for now.
    SignedOut,
}

#[cfg(target_arch = "wasm32")]
fn warn(message: &str) {
    web_sys::console::warn_1(&message.into());
}

/// The page's `window.its4tasksAuth`, which the server adds for sign-in
/// services that keep tokens fresh in the page, such as Clerk: `token({
/// fresh })` gives a promise of a token (a new one if `fresh`), or of null
/// when signed out, and `signIn()` goes to sign in.
#[cfg(target_arch = "wasm32")]
fn page_auth() -> Option<eframe::wasm_bindgen::JsValue> {
    let window = web_sys::window()?;
    let auth = js_sys::Reflect::get(&window, &"its4tasksAuth".into()).ok()?;
    (!auth.is_undefined() && !auth.is_null()).then_some(auth)
}

/// A token from the page's `its4tasksAuth`, if it has one. Never sends the
/// browser away: signing in again is the user's call (`sign_in_again`).
#[cfg(target_arch = "wasm32")]
async fn page_token(fresh: bool) -> Result<PageToken, String> {
    use eframe::wasm_bindgen::{JsCast, JsValue};
    use js_sys::{Function, Object, Promise, Reflect};

    let failed = |e: JsValue| format!("signing in: {e:?}");
    let Some(auth) = page_auth() else {
        return Ok(PageToken::Cookie);
    };
    let token: Function = Reflect::get(&auth, &"token".into())
        .map_err(failed)?
        .dyn_into()
        .map_err(failed)?;
    let options = Object::new();
    Reflect::set(&options, &"fresh".into(), &fresh.into()).map_err(failed)?;
    let promise: Promise = token
        .call1(&auth, &options)
        .map_err(failed)?
        .dyn_into()
        .map_err(failed)?;
    let token = wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .map_err(failed)?;
    match token.as_string() {
        Some(token) => Ok(PageToken::Bearer(token)),
        None => {
            warn("its4tasks: no session token from the page; retrying");
            Ok(PageToken::SignedOut)
        }
    }
}

/// Sends the browser to sign in again: through the page's `its4tasksAuth`,
/// or by reloading, for the server to send it there.
#[cfg(target_arch = "wasm32")]
pub fn sign_in_again() {
    use eframe::wasm_bindgen::JsCast;
    use js_sys::{Function, Reflect};

    let sign_in = page_auth().and_then(|auth| {
        let method: Function = Reflect::get(&auth, &"signIn".into())
            .ok()?
            .dyn_into()
            .ok()?;
        method.call0(&auth).ok()
    });
    if sign_in.is_none()
        && let Some(window) = web_sys::window()
    {
        let _ = window.location().reload();
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::native::local;

    #[test]
    fn archived_tasks_come_only_when_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("b.sqlite").display()
        );
        let server = local::start_at(url).unwrap();
        let mut client = Client::new(&server.url, Some(server.secret.clone()), TIMEOUT, || {});
        let long_ago = api::now() - chrono::TimeDelta::days(40);
        let mut board = Board::default();
        let old = board.add_task("old".into(), "", None, long_ago);
        board.task_mut(old).unwrap().set_done(true, long_ago);
        let open = board.add_task("open".into(), "", None, long_ago);
        client.apply(Change::Import { board }).unwrap();
        assert!(client.flush(TIMEOUT));
        let ids = |c: &Client| c.board().tasks.iter().map(|t| t.id).collect::<Vec<_>>();
        assert_eq!(ids(&client), [open]);
        assert!(!client.has_archived());

        client.set_archived(true);
        assert!(client.flush(TIMEOUT));
        assert!(client.has_archived());
        assert_eq!(ids(&client), [old, open]);
        // Changes' answers keep them.
        let rename = Change::Rename {
            task: open,
            title: "still open".into(),
        };
        client.apply(rename).unwrap();
        assert!(client.flush(TIMEOUT));
        assert_eq!(ids(&client), [old, open]);

        client.set_archived(false);
        assert!(!client.has_archived());
        assert_eq!(ids(&client), [open]);
    }
}
