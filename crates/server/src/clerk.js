// Signs the web GUI in with Clerk (https://clerk.com). Clerk's own script
// keeps the session going and hands out short-lived session tokens, which
// the GUI asks for before each request:
//   its4tasksAuth.token({ fresh }): a Promise of a token, or null if there's
//   no session; `fresh` skips Clerk's cache, for retrying after a refusal.
//   its4tasksAuth.signIn(): sends the browser to Clerk's sign-in page, and
//   back here afterwards. Only on the user's say-so: Clerk can briefly have
//   no session while it renews one.
// The server fills in the two constants below.
window.its4tasksAuth = (() => {
  const frontendApi = __FRONTEND_API__;
  const publishableKey = __PUBLISHABLE_KEY__;
  const ready = new Promise((resolve, reject) => {
    const script = document.createElement("script");
    script.src = `${frontendApi}/npm/@clerk/clerk-js@6/dist/clerk.browser.js`;
    script.crossOrigin = "anonymous";
    script.setAttribute("data-clerk-publishable-key", publishableKey);
    script.onload = () => window.Clerk.load().then(() => resolve(window.Clerk), reject);
    script.onerror = () => reject(new Error(`couldn't load Clerk from ${frontendApi}`));
    document.head.appendChild(script);
  });
  return {
    async token({ fresh = false } = {}) {
      const clerk = await ready;
      if (!clerk.session) {
        console.warn("its4tasks: Clerk has no session");
        return null;
      }
      try {
        const token = await clerk.session.getToken({ skipCache: fresh });
        if (!token) console.warn("its4tasks: Clerk gave no session token");
        return token || null;
      } catch (e) {
        console.warn("its4tasks: getting a session token from Clerk failed:", e);
        return null;
      }
    },
    async signIn() {
      const clerk = await ready;
      await clerk.redirectToSignIn({ signInForceRedirectUrl: window.location.href });
    },
  };
})();
