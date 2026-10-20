// Signs the web GUI in with Clerk (https://clerk.com). Clerk's own script
// keeps the session going and hands out short-lived session tokens, which
// the GUI asks for before each request:
//   its4tasksAuth.token(): a Promise of a fresh token, or null if signed out.
//   its4tasksAuth.signIn(): sends the browser to Clerk's sign-in page, and
//   back here afterwards.
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
    async token() {
      const clerk = await ready;
      return clerk.session ? await clerk.session.getToken() : null;
    },
    async signIn() {
      const clerk = await ready;
      await clerk.redirectToSignIn({ signInForceRedirectUrl: window.location.href });
    },
  };
})();
