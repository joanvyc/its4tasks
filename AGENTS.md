# Agent notes for its4tasks

its4tasks is an egui/eframe todo app: four focus cards plus a task list, with a daily/weekly log view. Boards are personal: each user sees only their own, and nothing is shared. For now there is one board and no login; multi-user support (authentication plus a private board per user) is a planned step.

It runs as a server, `its4tasks-server`, which keeps the board and serves both the API and the web GUI. The GUI runs in the browser (WASM) or natively, and is a client of the server. It's a Cargo workspace:
- `crates/core` (`its4tasks-core`): what both sides share. Must keep building for `wasm32-unknown-unknown`.
  - `model.rs`: data and rules.
  - `log.rs`: log grouping.
  - `api.rs`: the only way to change the board. Clients read a `Board` and send serializable `Change`s, and `apply_change` holds the rules for them. Tasks and comments have stable UUIDs (`Task::id` is a time-ordered v7, so sorting by it gives creation order; `Note::id` is a v4). Changes address comments by id, never by position, and changes that create things carry client-picked ids.
- `crates/server` (`its4tasks-server`):
  - `api.rs`: `Api`, the board plus saving after each change.
  - `storage/`: SQLite through sea-orm (entities, migrations, and a diff so a save writes only what changed), plus a one-time import of the old `board.json`.
  - `server.rs`: `GET /api/board`, `POST /api/changes`, and the web GUI at `/`.
  - `main.rs`: listens on `$ITS4TASKS_ADDR`, by default `127.0.0.1:7878`.
  - The web GUI is `crates/gui/dist`, embedded by rust-embed. Debug builds read it from disk, release builds bake it in (see `build.rs`).
- `crates/gui` (`its4tasks`):
  - `app.rs`: the UI.
  - `main.rs`: the native window, or the browser canvas in `index.html`.
  - `client.rs`: the GUI's side of the API. An edit shows at once: `apply` applies the change to a local copy with `apply_change`, and requests go out one at a time in the background (a thread per request natively, `fetch()` in the browser). Server answers replace the copy's base, with unanswered changes replayed on top. A refused change, or one sent while the server is unreachable, is undone. The native GUI talks to `$ITS4TASKS_SERVER` (by default `http://127.0.0.1:7878`), the web GUI to the server that served it.

## Checks

Before finishing a change, run these. All must pass with no warnings.
```sh
cargo fmt --all && cargo clippy --workspace --all-targets \
  && cargo clippy -p its4tasks --target wasm32-unknown-unknown && cargo test --workspace
```
Keep logic in `model.rs`/`log.rs`, where it can be unit-tested, rather than in `app.rs`. `app.rs` never mutates the model: a new kind of edit gets a `Change` variant in `api.rs`.

## Building the web GUI

```sh
(cd crates/gui && trunk build)            # writes crates/gui/dist; a debug server picks it up as is
(cd crates/gui && trunk build --release)  # then: cargo build --release -p its4tasks-server
```
A release `its4tasks-server` binary contains the GUI and needs nothing else.

## Testing visual changes

Any change that affects what's on screen (layout, colors, new widgets) must be checked by running the app and looking at screenshots. Passing tests is not enough. egui layout bugs only show up on screen, for example a row that grows past its card's edge or widgets that aren't centered. Use the helpers in `tools/`.

1. **Never touch the user's real data.** It lives in `~/.local/share/its4tasks/board.sqlite`. An older `board.json` there (or in `~/.local/share/focal/`) is imported into it on the first launch and kept as `board.json.imported`. Point `XDG_DATA_HOME` at a temporary directory and seed it with sample data:
   ```sh
   DATA=$(mktemp -d)
   python3 tools/seed_board.py "$DATA"
   ```
   The seed writes a `board.json`, which the app imports on launch. It covers about 12 days of history: open and resolved blockers, starred comments, a done task, and fresh and stale tasks. Extend it when a change needs other cases.

2. **Start the server, then the GUI under XWayland so X11 tools can see it.** `import` and `tools/xt.py` only work with X11 windows. Use a port other than the default 7878, so a real server or GUI the user has open never meets the test data.
   ```sh
   cargo build
   ITS4TASKS_ADDR=127.0.0.1:7979 XDG_DATA_HOME="$DATA" ./target/debug/its4tasks-server &
   SERVER=$!
   sleep 1
   ITS4TASKS_SERVER=http://127.0.0.1:7979 env -u WAYLAND_DISPLAY ./target/debug/its4tasks &
   APP=$!
   sleep 2.5
   ```
   `curl -s 127.0.0.1:7979/api/board` shows what the server holds. A change POSTed with curl, as another client would, shows up in the GUI within about 2 s. To check behaviour on a slow network, start the server with `ITS4TASKS_DELAY_MS=800`, which holds back every answer that long. Edits should still show at once.

3. **Screenshot and look at the result:**
   ```sh
   python3 tools/xt.py shot shot.png
   magick shot.png -crop 440x200+30+460 -filter point -resize 400% zoom.png   # zoom in on a region
   ```
   Coordinates in the screenshot are the same ones `xt.py` uses for input.

4. **Drive the UI to check interactive states** like hover, toggles, menus and typing:
   ```sh
   python3 tools/xt.py move X Y            # hover
   python3 tools/xt.py click X Y [3]       # left click, or right click with 3
   python3 tools/xt.py type "some text"
   python3 tools/xt.py key Return [shift]  # e.g. Shift+Enter
   python3 tools/xt.py key Escape
   ```
   The first pointer event after launch only tells egui where the pointer is. Move the pointer once before testing hover.

   **The web GUI** is tested the same way, in a Firefox with its own throwaway profile, so the user's browser is never touched. Run `trunk build` first. Then point `xt.py` at the Firefox window with `XT_WINDOW`:
   ```sh
   FF=$(mktemp -d)
   env -u WAYLAND_DISPLAY MOZ_ENABLE_WAYLAND=0 firefox --no-remote --profile "$FF" --kiosk http://127.0.0.1:7979/ &
   BROWSER=$!
   sleep 10
   XT_WINDOW="its4tasks " python3 tools/xt.py shot web.png
   ```
   Kiosk mode fills the screen, so the browser canvas starts at (0, 0). Key presses go to the focused window, and a native GUI open on top of the browser takes the clicks meant for the browser where they overlap.

5. **Rebuild before each look, and stop the app when done.** `cargo clippy` doesn't produce a new binary, so run `cargo build` before relaunching. When finished, run `kill $APP $SERVER` (and `$BROWSER`). Don't use `pkill`: the user may have their real instance open.

Check for:
- overlapping or clipped widgets
- even gaps between cards
- items in a row sharing one center line
- layout that doesn't shift when toggles or hover buttons change
- glyphs that render (some emoji are missing from egui's bundled fonts)

Zoom in on any area that looks off. To find why something is the wrong size, temporarily log `response.rect` sizes with `eprintln!` and compare them with what was expected.
