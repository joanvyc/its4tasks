//! The GUI's client against a real its4tasks-server, over HTTP on a free port.

use std::time::Duration;

use its4tasks::api::{ApiError, Change, Outcome};
use its4tasks::client::{Client, ClientError};
use its4tasks::model::MessageKind;
use its4tasks_server::api::Api;
use its4tasks_server::auth::Auth;
use its4tasks_server::server::serve;
use tokio::net::TcpListener;
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(5);

/// The local app's secret, which the test servers trust.
const SECRET: &str = "test-secret";

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
            serve(
                listener,
                Api::new(),
                Auth::Local {
                    secret: SECRET.into(),
                },
            )
            .await
            .unwrap();
        });
    });
    format!("http://{}", rx.recv().unwrap())
}

/// A client that has the server's board.
fn client(url: &str) -> Client {
    let mut client = Client::new(url, Some(SECRET.into()), WAIT, || {});
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
fn the_board_is_not_fetched_again_at_once() {
    let url = start();
    let mut a = client(&url);
    let mut b = client(&url);
    let id = add(&mut b, "Made elsewhere");
    assert!(b.flush(WAIT));
    // Just loaded: no fetch for a while.
    a.refresh();
    a.flush(Duration::from_millis(300));
    assert!(a.board().task(id).is_none());
}

#[test]
fn a_change_brings_the_board() {
    let url = start();
    let mut a = client(&url);
    let mut b = client(&url);
    let elsewhere = add(&mut b, "Made elsewhere");
    assert!(b.flush(WAIT));
    add(&mut a, "Made here");
    assert!(a.flush(WAIT));
    assert!(a.board().task(elsewhere).is_some(), "without fetching");
}

#[test]
fn changes_wait_for_a_working_token() {
    let url = start();
    let mut a = Client::new(&url, Some("expired".into()), WAIT, || {});
    a.flush(WAIT);
    assert!(a.unauthorized());
    // Briefly refused, as while a token is renewed: not signed out yet.
    assert!(!a.signed_out());
    let id = add(&mut a, "Written while refused");
    assert!(!a.flush(Duration::from_millis(300)), "held, not sent");
    assert!(a.board().task(id).is_some(), "still shown");
    assert!(client(&url).board().task(id).is_none());

    a.set_token(Some(SECRET.into()));
    assert!(a.flush(WAIT));
    assert!(!a.unauthorized());
    let server = client(&url);
    assert_eq!(
        server.board().tasks.iter().filter(|t| t.id == id).count(),
        1
    );
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
    let mut a = Client::new(&format!("http://127.0.0.1:{port}"), None, WAIT, || {});
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
    let mut a = Client::new(&url, None, Duration::from_millis(300), || {});
    let id = add(&mut a, "slow");
    assert!(a.board().task(id).is_some(), "shown without waiting");
    assert!(a.flush(WAIT));
    assert!(a.board().task(id).is_none());
    assert!(a.error().is_some());
}
