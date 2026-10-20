//! Clerk (https://clerk.com) in the browser. Its session tokens last a
//! minute and are kept fresh by Clerk's script, so with Clerk the page
//! loads that script (through `clerk.js`) and the web GUI asks it for a
//! token before each request, sending it as `Authorization: Bearer`.
//!
//! The server can't check the session before serving the page, then: right
//! after signing in there's no fresh token until Clerk's script has run on
//! the page. The page sends people to sign in instead.

use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clerk {
    pub publishable_key: String,
    /// Clerk's Frontend API, which serves its script and issues its tokens,
    /// e.g. `https://example.clerk.accounts.dev`.
    pub frontend_api: String,
}

impl Clerk {
    /// From a publishable key (`pk_test_…` or `pk_live_…`), which holds the
    /// Frontend API's domain. `frontend_api` overrides that, as a URL.
    pub fn new(publishable_key: &str, frontend_api: Option<&str>) -> Result<Clerk, String> {
        let frontend_api = match frontend_api {
            Some(url) => url.trim_end_matches('/').to_owned(),
            None => format!("https://{}", domain(publishable_key)?),
        };
        Ok(Clerk {
            publishable_key: publishable_key.to_owned(),
            frontend_api,
        })
    }

    /// The `<script>` for the page's `<head>`.
    pub fn script(&self) -> String {
        let js = include_str!("clerk.js")
            .replace("__FRONTEND_API__", &js_string(&self.frontend_api))
            .replace("__PUBLISHABLE_KEY__", &js_string(&self.publishable_key));
        format!("<script>\n{js}</script>\n")
    }
}

/// The Frontend API's domain in a publishable key: base64 of `domain$`.
fn domain(publishable_key: &str) -> Result<String, String> {
    let bad = || format!("{publishable_key:?} isn't a Clerk publishable key");
    let encoded = publishable_key
        .strip_prefix("pk_test_")
        .or_else(|| publishable_key.strip_prefix("pk_live_"))
        .ok_or_else(bad)?;
    let bytes = STANDARD_NO_PAD
        .decode(encoded.trim_end_matches('='))
        .map_err(|_| bad())?;
    let text = String::from_utf8(bytes).map_err(|_| bad())?;
    let domain = text.strip_suffix('$').ok_or_else(bad)?;
    if domain.is_empty() || domain.contains(['/', '"', '<', ' ']) {
        return Err(bad());
    }
    Ok(domain.to_owned())
}

/// `s` as a JavaScript string literal that can't end the `<script>`.
fn js_string(s: &str) -> String {
    serde_json::to_string(s)
        .expect("a string serializes")
        .replace("</", "<\\/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publishable_keys_name_the_frontend_api() {
        // base64("example.clerk.accounts.dev$")
        let key = "pk_test_ZXhhbXBsZS5jbGVyay5hY2NvdW50cy5kZXYk";
        let clerk = Clerk::new(key, None).unwrap();
        assert_eq!(clerk.frontend_api, "https://example.clerk.accounts.dev");
        let local = Clerk::new(key, Some("http://127.0.0.1:7980/")).unwrap();
        assert_eq!(local.frontend_api, "http://127.0.0.1:7980");
        assert!(Clerk::new("sk_test_abc", None).is_err());
        assert!(Clerk::new("pk_test_!!!", None).is_err());
    }

    #[test]
    fn the_script_carries_the_key() {
        let clerk = Clerk {
            publishable_key: "pk_test_x</script>".into(),
            frontend_api: "https://clerk.example.com".into(),
        };
        let script = clerk.script();
        assert!(script.contains(r#"const frontendApi = "https://clerk.example.com";"#));
        assert!(script.contains(r#""pk_test_x<\/script>""#));
        assert_eq!(script.matches("</script>").count(), 1);
    }
}
