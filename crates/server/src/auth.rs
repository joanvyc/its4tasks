//! Who's asking. Signing in happens at an outside authority (Clerk or
//! similar), which hands out JWTs it signs. A request carries one as
//! `Authorization: Bearer <jwt>` (the native app) or in a cookie (the
//! browser); the user is the token's `sub` at its `iss`.
//!
//! The native app's local mode has no authority: its in-process server
//! trusts one random secret instead, which stands for the local user.

use std::time::{Duration, Instant};

use axum::http::{HeaderMap, header};
use jsonwebtoken::jwk::{AlgorithmParameters, JwkSet};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::clerk::Clerk;
use crate::storage::LOCAL;

/// Someone the authority vouches for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub iss: String,
    pub sub: String,
}

/// Why a request isn't signed in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unauthorized(pub String);

pub enum Auth {
    /// Tokens signed by an authority.
    Jwt(Box<Jwt>),
    /// The native app's local mode: this bearer secret is the local user.
    Local { secret: String },
}

impl Auth {
    pub async fn identify(&self, headers: &HeaderMap) -> Result<Identity, Unauthorized> {
        match self {
            Auth::Local { secret } => match bearer(headers) {
                Some(token) if token == secret => Ok(Identity {
                    iss: LOCAL.to_owned(),
                    sub: LOCAL.to_owned(),
                }),
                _ => Err(Unauthorized("not the local app".into())),
            },
            Auth::Jwt(jwt) => {
                let token = bearer(headers)
                    .or_else(|| cookie(headers, &jwt.config.cookie))
                    .ok_or_else(|| Unauthorized("not signed in".into()))?;
                jwt.verify(token).await
            }
        }
    }
}

/// Where to trust tokens from, and where to send people to sign in.
#[derive(Debug, Clone)]
pub struct JwtConfig {
    /// The `iss` tokens must have.
    pub issuer: String,
    /// Where the authority publishes its keys.
    pub jwks_url: String,
    /// The `aud` tokens must have, if any.
    pub audience: Option<String>,
    /// How the browser signs in.
    pub web: WebSignIn,
    /// The cookie the browser's token is in.
    pub cookie: String,
    /// How the native app signs in, if it can.
    pub oauth: Option<OAuth>,
}

/// How the browser signs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebSignIn {
    /// The server sends people without a valid token to this page, where
    /// `{redirect}` becomes the page to come back to. The authority sets the
    /// token's cookie for the server's domain.
    Redirect(String),
    /// Clerk's script on the page signs people in and keeps their token
    /// fresh, and the page sends it along.
    Clerk(Clerk),
}

/// How a native app signs in: OAuth 2 authorization code with PKCE, for a
/// public client, redirecting to a loopback address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuth {
    pub issuer: String,
    pub authorize_url: String,
    pub token_url: String,
    pub client_id: String,
}

impl JwtConfig {
    /// From `ITS4TASKS_AUTH_*`, `ITS4TASKS_CLERK_*` and `ITS4TASKS_OAUTH_*`;
    /// `None` without an issuer or Clerk key. Errors name what's missing.
    pub fn from_env() -> Result<Option<JwtConfig>, String> {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let clerk = match var("ITS4TASKS_CLERK_PUBLISHABLE_KEY") {
            Some(key) => Some(Clerk::new(
                &key,
                var("ITS4TASKS_CLERK_FRONTEND_API").as_deref(),
            )?),
            None => None,
        };
        // Clerk's tokens come from its Frontend API.
        let issuer = match (var("ITS4TASKS_AUTH_ISSUER"), &clerk) {
            (Some(issuer), _) => issuer,
            (None, Some(clerk)) => clerk.frontend_api.clone(),
            (None, None) => return Ok(None),
        };
        let issuer = issuer.trim_end_matches('/').to_owned();
        let web = match (clerk, var("ITS4TASKS_AUTH_SIGN_IN_URL")) {
            (Some(clerk), _) => WebSignIn::Clerk(clerk),
            (None, Some(url)) => WebSignIn::Redirect(url),
            (None, None) => {
                return Err("set ITS4TASKS_AUTH_SIGN_IN_URL, or \
                            ITS4TASKS_CLERK_PUBLISHABLE_KEY, for browsers to sign in"
                    .into());
            }
        };
        let oauth = match (
            var("ITS4TASKS_OAUTH_AUTHORIZE_URL"),
            var("ITS4TASKS_OAUTH_TOKEN_URL"),
            var("ITS4TASKS_OAUTH_CLIENT_ID"),
        ) {
            (Some(authorize_url), Some(token_url), Some(client_id)) => Some(OAuth {
                issuer: issuer.clone(),
                authorize_url,
                token_url,
                client_id,
            }),
            (None, None, None) => None,
            _ => {
                return Err("set all of ITS4TASKS_OAUTH_AUTHORIZE_URL, _TOKEN_URL and \
                            _CLIENT_ID, or none"
                    .into());
            }
        };
        Ok(Some(JwtConfig {
            jwks_url: var("ITS4TASKS_AUTH_JWKS_URL")
                .unwrap_or_else(|| format!("{issuer}/.well-known/jwks.json")),
            issuer,
            audience: var("ITS4TASKS_AUTH_AUDIENCE"),
            web,
            cookie: var("ITS4TASKS_AUTH_COOKIE").unwrap_or_else(|| "__session".to_owned()),
            oauth,
        }))
    }

    /// The page to send browsers without a valid token to, coming back to
    /// `back` afterwards; `None` if the page signs in by itself.
    pub fn sign_in(&self, back: &str) -> Option<String> {
        match &self.web {
            WebSignIn::Redirect(url) => Some(url.replace("{redirect}", &percent_encode(back))),
            WebSignIn::Clerk(_) => None,
        }
    }
}

/// Checks tokens against the authority's published keys.
pub struct Jwt {
    pub config: JwtConfig,
    keys: RwLock<Keys>,
}

struct Keys {
    set: JwkSet,
    /// When `set` was last fetched; `None` if never.
    fetched: Option<Instant>,
}

/// Don't ask the authority for its keys more often than this, however many
/// tokens with unknown key ids turn up.
const REFETCH_AFTER: Duration = Duration::from_secs(60);

impl Jwt {
    /// Fetches the keys when first needed.
    pub fn new(config: JwtConfig) -> Jwt {
        Jwt {
            config,
            keys: RwLock::new(Keys {
                set: JwkSet { keys: Vec::new() },
                fetched: None,
            }),
        }
    }

    /// Trusts exactly these keys, and never fetches any (for tests).
    pub fn with_keys(config: JwtConfig, set: JwkSet) -> Jwt {
        Jwt {
            config,
            keys: RwLock::new(Keys {
                set,
                fetched: Some(Instant::now() + Duration::from_secs(365 * 24 * 3600)),
            }),
        }
    }

    pub async fn verify(&self, token: &str) -> Result<Identity, Unauthorized> {
        let fail = |e: &dyn std::fmt::Display| Unauthorized(format!("invalid token: {e}"));
        let header = decode_header(token).map_err(|e| fail(&e))?;
        let kid = header.kid.ok_or_else(|| fail(&"no key id"))?;
        let jwk = self.key(&kid).await?;
        // The algorithm comes from the key, never from the token alone, and
        // only public-key ones count.
        let algorithm = match &jwk.algorithm {
            AlgorithmParameters::RSA(_)
                if matches!(
                    header.alg,
                    Algorithm::RS256
                        | Algorithm::RS384
                        | Algorithm::RS512
                        | Algorithm::PS256
                        | Algorithm::PS384
                        | Algorithm::PS512
                ) =>
            {
                header.alg
            }
            AlgorithmParameters::EllipticCurve(_)
                if matches!(header.alg, Algorithm::ES256 | Algorithm::ES384) =>
            {
                header.alg
            }
            AlgorithmParameters::OctetKeyPair(_) if header.alg == Algorithm::EdDSA => header.alg,
            _ => return Err(fail(&"algorithm doesn't match the key")),
        };
        let key = DecodingKey::from_jwk(&jwk).map_err(|e| fail(&e))?;
        let mut validation = Validation::new(algorithm);
        validation.set_issuer(&[&self.config.issuer]);
        validation.set_required_spec_claims(&["exp", "iss", "sub"]);
        match &self.config.audience {
            Some(aud) => validation.set_audience(&[aud]),
            None => validation.validate_aud = false,
        }
        let claims = decode::<Claims>(token, &key, &validation).map_err(|e| fail(&e))?;
        Ok(Identity {
            iss: claims.claims.iss,
            sub: claims.claims.sub,
        })
    }

    /// The key with this id, fetching the authority's keys if it's new.
    async fn key(&self, kid: &str) -> Result<jsonwebtoken::jwk::Jwk, Unauthorized> {
        let find = |keys: &Keys| keys.set.find(kid).cloned();
        let stale = {
            let keys = self.keys.read().await;
            if let Some(jwk) = find(&keys) {
                return Ok(jwk);
            }
            keys.fetched.is_none_or(|at| at.elapsed() >= REFETCH_AFTER)
        };
        if stale {
            let mut keys = self.keys.write().await;
            // Someone else may have fetched them while we waited.
            if find(&keys).is_none() && keys.fetched.is_none_or(|at| at.elapsed() >= REFETCH_AFTER)
            {
                keys.fetched = Some(Instant::now());
                match fetch_keys(self.config.jwks_url.clone()).await {
                    Ok(set) => keys.set = set,
                    Err(e) => eprintln!("its4tasks-server: fetching the authority's keys: {e}"),
                }
            }
            if let Some(jwk) = find(&keys) {
                return Ok(jwk);
            }
        }
        Err(Unauthorized(format!("invalid token: unknown key {kid}")))
    }
}

async fn fetch_keys(url: String) -> Result<JwkSet, String> {
    tokio::task::spawn_blocking(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .build()
            .into();
        agent
            .get(&url)
            .call()
            .and_then(|mut r| r.body_mut().read_json::<JwkSet>())
            .map_err(|e| format!("{url}: {e}"))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Deserialize)]
struct Claims {
    iss: String,
    sub: String,
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
}

fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|pair| {
            let (k, v) = pair.trim().split_once('=')?;
            (k == name).then_some(v)
        })
}

/// Escapes everything but unreserved characters, for a URL's query.
fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_token() {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, "a=1; __session=tok; b=2".parse().unwrap());
        assert_eq!(cookie(&headers, "__session"), Some("tok"));
        assert_eq!(cookie(&headers, "c"), None);
        headers.insert(header::AUTHORIZATION, "Bearer abc".parse().unwrap());
        assert_eq!(bearer(&headers), Some("abc"));
    }

    #[test]
    fn sign_in_comes_back() {
        let config = JwtConfig {
            issuer: "https://auth.test".into(),
            jwks_url: String::new(),
            audience: None,
            web: WebSignIn::Redirect("https://auth.test/sign-in?redirect_url={redirect}".into()),
            cookie: "__session".into(),
            oauth: None,
        };
        assert_eq!(
            config.sign_in("http://127.0.0.1:7979/?a=b").as_deref(),
            Some("https://auth.test/sign-in?redirect_url=http%3A%2F%2F127.0.0.1%3A7979%2F%3Fa%3Db")
        );
    }
}
