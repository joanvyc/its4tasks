//! `its4tasks --get-board`: the board the app would open, printed as an
//! `its4tasks.json` instead of shown.

use chrono::Utc;

use super::config::{Config, Source};
use super::{local, oauth};
use crate::client::{self, ClientError};
use crate::import;

/// The board chosen in the app, as an `its4tasks.json`'s text.
pub fn get_board() -> Result<String, String> {
    let mut config = Config::load();
    let view = match config.source.clone() {
        None => return Err("no board chosen yet: open its4tasks once and pick one".into()),
        Some(Source::Local) => {
            // Stops serving when dropped, after the fetch.
            let server = local::start()?;
            client::fetch_board(&server.url, &server.secret)
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
            client::fetch_board(&url, &tokens.access)
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

    #[test]
    fn a_local_board_is_fetched_with_its_secret() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("b.sqlite").display()
        );
        let server = local::start_at(url).unwrap();
        let view = client::fetch_board(&server.url, &server.secret).unwrap();
        assert!(!view.started);
        assert!(import::parse(&import::export(&view.board, Utc::now())).is_ok());
        assert_eq!(
            client::fetch_board(&server.url, "wrong").unwrap_err(),
            ClientError::Unauthorized
        );
    }
}
