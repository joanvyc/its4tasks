// A stand-in for Clerk's clerk-js, FOR LOCAL TESTING ONLY, served by
// its4tasks-dev-auth. It has just what its4tasks-server's clerk.js uses:
// Clerk.load(), Clerk.session.getToken() and Clerk.redirectToSignIn(). Like
// Clerk, it keeps a session (the `__client` cookie dev-auth's sign-in sets)
// and hands out short-lived tokens for it, the same one until it's close to
// expiring; a session that ends is checked for again every 5 s.
(() => {
  const origin = new URL(document.currentScript.src).origin;

  // A token for the session, or null without one.
  async function fetchToken() {
    const response = await fetch(`${origin}/v1/client/token`, { credentials: "include" });
    if (response.status === 401) return null;
    if (!response.ok) throw new Error(`dev-auth answered ${response.status}`);
    return (await response.json()).jwt;
  }

  function expiry(jwt) {
    const payload = jwt.split(".")[1].replace(/-/g, "+").replace(/_/g, "/");
    return JSON.parse(atob(payload)).exp;
  }

  function session(first) {
    let jwt = first;
    return {
      async getToken() {
        if (expiry(jwt) - Date.now() / 1000 < 10) {
          const fresh = await fetchToken();
          if (!fresh) {
            clerk.session = null;
            return null;
          }
          jwt = fresh;
        }
        return jwt;
      },
    };
  }

  const clerk = {
    session: undefined,
    async load() {
      const jwt = await fetchToken();
      clerk.session = jwt ? session(jwt) : null;
    },
    async redirectToSignIn(options) {
      const back = (options && options.signInForceRedirectUrl) || window.location.href;
      window.location.assign(`${origin}/sign-in?redirect=${encodeURIComponent(back)}`);
    },
  };
  // Like Clerk keeping its client in sync (say, after signing in again in
  // another tab): a session that comes back is picked up.
  setInterval(async () => {
    if (clerk.session !== null) return;
    const jwt = await fetchToken().catch(() => null);
    if (jwt) clerk.session = session(jwt);
  }, 5000);
  window.Clerk = clerk;
})();
