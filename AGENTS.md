# Agent notes for its4tasks

its4tasks is an egui/eframe todo app: four focus cards plus a task list, with a daily/weekly log view. Boards are personal: each user sees only their own, and nothing is shared. `README.md` covers running it; this file is about working on it.

It runs as a server, `its4tasks-server`, which keeps every user's board and serves both the API and the web GUI. The GUI runs in the browser (WASM) or natively, and is a client of the server. It's a Cargo workspace:
- `crates/core` (`its4tasks-core`): what both sides share. Must keep building for `wasm32-unknown-unknown`.
  - `model.rs`: data and rules.
  - `log.rs`: log grouping.
  - `api.rs`: the only way to change a board. Clients read a `Board` and send serializable `Change`s, and `apply_change` holds the rules for them. Tasks and comments have stable UUIDs (`Task::id` is a time-ordered v7, so sorting by it gives creation order; `Note::id` is a v4). Changes address comments by id, never by position, and changes that create things carry client-picked ids. `Change::Import` adds a whole board's tasks (given fresh ids by `import::fresh_ids` first).
  - `import.rs`: reading `board.json` files, including the older formats. `welcome.rs`: the board a new user starts with.
  - `examples/import_change.rs`: prints the `Import` change for a `board.json`, for seeding test users.
- `crates/server` (`its4tasks-server`, library and binary):
  - `auth.rs`: who's asking. Users sign in at an outside authority (Clerk or similar) that signs JWTs; the server checks them against the authority's JWKS and knows a user by the token's `(iss, sub)`. The token comes as `Authorization: Bearer` (native, and the browser with Clerk) or in the `__session` cookie (browser). `Auth::Local` is the native app's local mode instead: one random bearer secret, standing for the local user. Tokens are read by `Jwt::verify` itself (header, claims, times with 60 s leeway, issuer, audience), and jsonwebtoken only checks the signature (`crypto::verify`). `jsonwebtoken::decode` refuses any token whose header has a non-string extra field, and Clerk's renewed tokens have numeric ones. Refused tokens are logged with the reason, at most every 10 s.
  - `clerk.rs`, `clerk.js`: with Clerk, the page loads Clerk's script and defines `window.its4tasksAuth`: `token({ fresh })` and `signIn()`. The startup page and the web GUI ask it for a token before each request. The server then serves the page without checking for a session, and the startup page sends people to sign in itself.
  - `api.rs`: `Api`, a board per user plus saving after each change. An unknown `(iss, sub)` becomes a new user, whose board is empty with `started: false` until they start one or import one. Touching another user's task or comment is `Forbidden`.
  - `storage/`: sea-orm over SQLite (feature `sqlite`, default) or Postgres (feature `postgres`): entities, migrations, and a per-user diff so a save writes only what changed. Every row carries its `owner_id`.
  - `server.rs`: `GET /api/board`, `POST /api/changes` (401 without a valid token, 403 for others' things), `POST /api/start` (a new user's welcome board; 409 once started), `POST /api/import` (a `board.json`'s text; the server parses it with `core::import`), and `GET /api/auth` (how native apps sign in). It also serves the web GUI's files, with an ETag and `no-cache`, since their names don't change between builds.
  - `page/index.html`: the startup page at `/`. Before downloading the board app, it checks sign-in and whether the user has a board. Signed-out visitors go straight to sign-in, with nothing rendered. New users get a plain HTML landing page (start, or import a `board.json`). Everyone else gets the board app, which it loads by importing `/its4tasks.js`. In cookie mode the server sends strangers to the sign-in page before serving it.
  - `main.rs`: configuration from the environment; `README.md` lists it.
  - The web GUI is `crates/gui/dist`, embedded by rust-embed. Debug builds read it from disk, release builds bake it in (see `build.rs`).
- `crates/gui` (`its4tasks`):
  - `app.rs`: the UI, including the landing page for new users and the "⋯" menu (import, switch board).
  - `client.rs`: the GUI's side of the API. An edit shows at once: `apply` applies the change to a local copy with `apply_change`, and requests go out one at a time in the background (a thread per request natively, `fetch()` in the browser). Server answers replace the copy's base, with unanswered changes replayed on top. A refused change, or one sent while the server is unreachable, is undone. A change refused with 401 is kept and held instead, as the server didn't apply it.
    - In the browser, requests carry the token from the page's `its4tasksAuth` if it has one (Clerk), and the session cookie otherwise.
    - A 401, or no token from the page, sets `unauthorized()`. Requests are held and retried every 2 s, with a fresh token in the browser. The native shell refreshes its token and calls `set_token`.
    - `signed_out()` is true after 10 s of refusals, and the view bar then says so (with "Sign in again" in the browser).
    - **The app never reloads the page or redirects on its own.** Doing so loses whatever the user was doing, and Clerk can briefly have no token while it renews one.
  - `files.rs`: picking a `board.json` to import, natively or in the browser.
  - `unsent.rs`: in the browser, work not on the server yet is kept in `localStorage` (`its4tasks.unsent.<user id>`) about once a second, and right before "Sign in again". That means changes held while refused (`Client::held`, empty unless refused, so nothing is sent twice), comment drafts by task, a comment being edited, and the new-task form. The app puts it back after the next load for the same user (`BoardView::user`).
  - `native/`: the native app's shell. The first time, it asks where the board is (`shell.rs`): a local board, served in-process from `board.sqlite` in the data directory (`local.rs`), or a server, signed in through the browser with OAuth code + PKCE and a loopback redirect (`oauth.rs`). The choice and the refresh token are kept in `config.json` in the config directory (`config.rs`).
  - `main.rs`: the native window, or the browser canvas in `index.html`.
- `crates/dev-auth` (`its4tasks-dev-auth`): a fake authority for local testing. Anyone signs in as anyone by typing a name; tokens are signed with the test key in `crates/server/tests/fixtures`. It also serves a mock of Clerk's script (`clerk.browser.js`) for trying the Clerk setup; `README.md` has the settings. Its renewed tokens are shaped like Clerk's: v2 claims, and a number in the header. `POST /v1/client/revoke?sub=user_<name>` cuts that user's mock session off, as signing out elsewhere would, and `POST /v1/client/restore?sub=…` brings it back. The user is in the `__client` cookie, which is `user_` plus the lowercased letters and digits of the name typed. Never use it, or that key, for real.

## Checks

Before finishing a change, run these. All must pass with no warnings.
```sh
cargo fmt --all && cargo clippy --workspace --all-targets \
  && cargo clippy -p its4tasks --target wasm32-unknown-unknown \
  && cargo clippy -p its4tasks-server --no-default-features --features postgres --all-targets \
  && cargo test --workspace
```
Keep logic in `model.rs`/`log.rs`, where it can be unit-tested, rather than in `app.rs`. `app.rs` never mutates the model: a new kind of edit gets a `Change` variant in `api.rs`.

The storage tests also run against Postgres when `TEST_DATABASE_URL` points at a server they may create databases on, with the `postgres` feature on (the default build has no Postgres driver). A throwaway cluster will do:
```sh
PG=$(mktemp -d)
initdb -D "$PG/data" -U its4tasks --auth=trust
pg_ctl -D "$PG/data" -o "-p 55432 -c unix_socket_directories='' -c listen_addresses=127.0.0.1" -l "$PG/log" start
TEST_DATABASE_URL=postgres://its4tasks@127.0.0.1:55432/postgres cargo test -p its4tasks-server --features postgres storage
pg_ctl -D "$PG/data" stop
```

## Building the web GUI

```sh
(cd crates/gui && trunk build)            # writes crates/gui/dist (its4tasks.js, its4tasks_bg.wasm); a debug server picks it up as is
(cd crates/gui && trunk build --release)  # then: cargo build --release -p its4tasks-server
```
A release `its4tasks-server` binary contains the GUI and needs nothing else.

## Testing visual changes

Any change that affects what's on screen (layout, colors, new widgets) must be checked by running the app and looking at screenshots. Passing tests is not enough. egui layout bugs only show up on screen, for example a row that grows past its card's edge or widgets that aren't centered. Use the helpers in `tools/`.

1. **Never touch the user's real data or settings.** The native app keeps its local board in `~/.local/share/its4tasks/board.sqlite` and its settings, including a refresh token, in `~/.config/its4tasks/config.json`. Always point both `XDG_DATA_HOME` and `XDG_CONFIG_HOME` at temporary directories, for the app and for the server. Use ports other than the default 7878, so a real server or GUI the user has open never meets the test data: 7979 for the server and 7980 for dev-auth below.

2. **Start the fake authority and a server that trusts it**, then seed a test user's board. `tools/seed_board.py` writes a `board.json` with about 12 days of history: open and resolved blockers, starred comments, a done task, and fresh and stale tasks. Extend it when a change needs other cases.
   ```sh
   cargo build
   T=$(mktemp -d)
   ./target/debug/its4tasks-dev-auth &
   AUTH=$!
   ITS4TASKS_ADDR=127.0.0.1:7979 \
   ITS4TASKS_AUTH_ISSUER=http://127.0.0.1:7980 \
   ITS4TASKS_AUTH_SIGN_IN_URL='http://127.0.0.1:7980/sign-in?redirect={redirect}' \
   ITS4TASKS_OAUTH_AUTHORIZE_URL=http://127.0.0.1:7980/authorize \
   ITS4TASKS_OAUTH_TOKEN_URL=http://127.0.0.1:7980/token \
   ITS4TASKS_OAUTH_CLIENT_ID=its4tasks \
   DATABASE_URL="sqlite://$T/server.sqlite?mode=rwc" ./target/debug/its4tasks-server &
   SERVER=$!
   sleep 1
   # A token for alice, and her board seeded:
   TOKEN=$(curl -s -D - -o /dev/null -d name=alice -d redirect=http://127.0.0.1:7979/ \
     http://127.0.0.1:7980/sign-in | sed -n 's/^set-cookie: __session=\([^;]*\).*/\1/ip')
   python3 tools/seed_board.py "$T/seed"
   cargo run -q -p its4tasks-core --example import_change -- "$T/seed/its4tasks/board.json" \
     | curl -s -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' --data-binary @- \
       127.0.0.1:7979/api/changes > /dev/null
   ```
   `curl -s -H "Authorization: Bearer $TOKEN" 127.0.0.1:7979/api/board` shows alice's board. A change POSTed with curl, as another client would, shows up in the GUI within about 2 s. To check behaviour on a slow network, start the server with `ITS4TASKS_DELAY_MS=800`, which holds back every answer that long. Edits should still show at once. dev-auth's tokens last an hour; `ITS4TASKS_DEV_AUTH_TTL=20` makes them expire quickly, to test renewal (the server allows 60 s of clock leeway on top).

3. **Run the native GUI under XWayland so X11 tools can see it.** `import` and `tools/xt.py` only work with X11 windows. Signing in opens a browser, so point `BROWSER` at a stand-in that never opens the user's: a script that signs in with curl and follows the redirect back to the app works.
   ```sh
   cat > "$T/browser.sh" <<'EOF'
   #!/bin/bash
   # Signs in to dev-auth as alice and follows the redirect back to the app.
   (loc=$(curl -s -o /dev/null -w '%{redirect_url}' --data "${1#*\?}&name=alice" http://127.0.0.1:7980/authorize)
    curl -s "$loc") > /dev/null 2>&1 &
   EOF
   chmod +x "$T/browser.sh"
   env -u WAYLAND_DISPLAY BROWSER="$T/browser.sh" XDG_DATA_HOME="$T/data" XDG_CONFIG_HOME="$T/config" \
     ./target/debug/its4tasks &
   APP=$!
   sleep 2.5
   ```
   The first launch shows where-is-your-board: type `http://127.0.0.1:7979` in the server field and press Enter to sign in as alice. Later launches with the same `XDG_CONFIG_HOME` go straight to the board. "Use a local board" serves `$T/data/its4tasks/board.sqlite` in-process instead. The native file dialog for importing goes through the desktop portal, which `xt.py` can't see or drive, so seed boards through the API instead.

4. **Screenshot and look at the result:**
   ```sh
   python3 tools/xt.py shot shot.png
   magick shot.png -crop 440x200+30+460 -filter point -resize 400% zoom.png   # zoom in on a region
   ```
   Coordinates in the screenshot are the same ones `xt.py` uses for input.

5. **Drive the UI to check interactive states** like hover, toggles, menus and typing:
   ```sh
   python3 tools/xt.py move X Y            # hover
   python3 tools/xt.py click X Y [3]       # left click, or right click with 3
   python3 tools/xt.py type "some text"
   python3 tools/xt.py key Return [shift]  # e.g. Shift+Enter
   python3 tools/xt.py key Escape
   ```
   The first pointer event after launch only tells egui where the pointer is. Move the pointer once before testing hover. Clicking into a text field puts the cursor where you click: press `End` before deleting what's there. `click`, `type` and `key` raise the window and give it X input focus first. Even so, take a screenshot after typing to check the text arrived. Synthetic X input only ever reaches X11 windows, never the user's Wayland ones.

   **The web GUI** is tested the same way, in a Firefox with its own throwaway profile, so the user's browser is never touched. Run `trunk build` first. The server sends the new profile to dev-auth's sign-in page; sign in there, then point `xt.py` at the app with `XT_WINDOW`:
   ```sh
   FF=$(mktemp -d)
   env -u WAYLAND_DISPLAY MOZ_ENABLE_WAYLAND=0 firefox --no-remote --profile "$FF" --kiosk http://127.0.0.1:7979/ &
   BROWSER=$!
   sleep 10
   XT_WINDOW="Sign in" python3 tools/xt.py click 950 551   # the name field
   XT_WINDOW="Sign in" python3 tools/xt.py type alice
   XT_WINDOW="Sign in" python3 tools/xt.py key Return
   sleep 10
   XT_WINDOW="its4tasks" python3 tools/xt.py shot web.png
   ```
   Kiosk mode fills the screen, so the browser canvas starts at (0, 0). Key presses go to the focused window, and a native GUI open on top of the browser takes the clicks meant for the browser where they overlap. A second profile signed in as someone else checks that boards stay apart. The session cookie lasts only as long as that Firefox: a restarted one signs in again. To see the page's console, add `user_pref("devtools.console.stdout.content", true);` to `$FF/user.js` and read Firefox's stdout.

6. **Rebuild before each look, and stop everything when done.** `cargo clippy` doesn't produce a new binary, so run `cargo build` before relaunching. When finished, run `kill $APP $SERVER $AUTH` (and `$BROWSER`). Don't use `pkill`: the user may have their real instance open.

Check for:
- overlapping or clipped widgets
- even gaps between cards
- items in a row sharing one center line
- layout that doesn't shift when toggles or hover buttons change
- glyphs that render (some emoji are missing from egui's bundled fonts)

Zoom in on any area that looks off. To find why something is the wrong size, temporarily log `response.rect` sizes with `eprintln!` and compare them with what was expected.
