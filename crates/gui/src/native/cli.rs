//! `its4tasks --get-board`: the board the app would open, printed as an
//! `its4tasks.json` instead of shown. Without its archived tasks, unless
//! `--get-board-archived`.

use chrono::Utc;

use super::config::{Config, Source};
use super::{local, oauth};
use crate::client::{self, ClientError};
use crate::import;

/// The board chosen in the app, as an `its4tasks.json`'s text: without its
/// archived tasks (see `Board::archived`), unless `archived`.
pub fn get_board(archived: bool) -> Result<String, String> {
    let mut config = Config::load();
    let view = match config.source.clone() {
        None => return Err("no board chosen yet: open its4tasks once and pick one".into()),
        Some(Source::Local) => {
            // Stops serving when dropped, after the fetch.
            let server = local::start()?;
            client::fetch_board(&server.url, &server.secret, archived)
        }
        Some(Source::Server {
            refresh_token: None,
            ..
        }) => return Err("signed out: sign in with the app first".into()),
        Some(Source::Server {
            url,
            refresh_token: Some(token),
        }) => {
            let tokens =
                oauth::refresh(&url, &token).map_err(|e| format!("signing in to {url}: {e}"))?;
            // Authorities may hand out a new refresh token in place of the old.
            if tokens.refresh.as_ref() != Some(&token) {
                config.source = Some(Source::Server {
                    url: url.clone(),
                    refresh_token: tokens.refresh.clone(),
                });
                config.save();
            }
            client::fetch_board(&url, &tokens.access, archived)
        }
    };
    let view = view.map_err(|e| match e {
        ClientError::Unauthorized => "the server refused the sign-in".to_owned(),
        e => e.to_string(),
    })?;
    Ok(import::export(&view.board, Utc::now()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{self, Change};
    use crate::client::Client;
    use crate::model::Board;

    #[test]
    fn a_local_board_is_fetched_with_its_secret() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("b.sqlite").display()
        );
        let server = local::start_at(url).unwrap();
        let view = client::fetch_board(&server.url, &server.secret, true).unwrap();
        assert!(!view.started);
        assert!(import::parse(&import::export(&view.board, Utc::now())).is_ok());
        assert_eq!(
            client::fetch_board(&server.url, "wrong", true).unwrap_err(),
            ClientError::Unauthorized
        );
    }

    #[test]
    fn archived_tasks_only_when_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("b.sqlite").display()
        );
        let server = local::start_at(url).unwrap();
        let long_ago = api::now() - chrono::TimeDelta::days(40);
        let mut board = Board::default();
        let old = board.add_task("old".into(), "", None, long_ago);
        board.task_mut(old).unwrap().set_done(true, long_ago);
        let open = board.add_task("open".into(), "", None, long_ago);
        let mut client = Client::new(
            &server.url,
            Some(server.secret.clone()),
            client::TIMEOUT,
            || {},
        );
        client.apply(Change::Import { board }).unwrap();
        assert!(client.flush(client::TIMEOUT));
        let ids = |archived| {
            let view = client::fetch_board(&server.url, &server.secret, archived).unwrap();
            view.board.tasks.iter().map(|t| t.id).collect::<Vec<_>>()
        };
        assert_eq!(ids(false), [open]);
        assert_eq!(ids(true), [old, open]);
    }
}
