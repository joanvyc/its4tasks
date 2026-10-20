# Agent notes for its4tasks

its4tasks is an egui/eframe desktop todo app: four focus cards plus a task list, with a daily/weekly log view. It runs as two binaries: `its4tasks-server` keeps the board and serves it over HTTP, and the `its4tasks` GUI is a client of it. The code is in `src/`:
- `model.rs`: data and rules
- `log.rs`: log grouping
- `api.rs`: the only way to change the board. Clients read a `Board` and send serializable `Change`s, which `Api::apply` applies and saves. Tasks and comments have stable UUIDs (`Task::id` is a time-ordered v7, so sorting by it gives creation order; `Note::id` is a v4), and changes address comments by id, never by position
- `server.rs` and `bin/its4tasks-server.rs`: `Api` over HTTP (`GET /board`, `POST /changes`). Listens on `$ITS4TASKS_ADDR`, by default `127.0.0.1:7878`
- `client.rs`: the GUI's side. An edit shows at once: `apply` applies the change to a local copy with the server's rules (`api::apply_change`), and a worker thread sends it, so the UI never waits on the network. Server answers replace the copy's base, with unanswered changes replayed on top. A refused change, or one sent while the server is unreachable, is undone. Changes that create tasks or comments carry client-picked ids so both sides agree. Talks to `$ITS4TASKS_SERVER`, by default `http://127.0.0.1:7878`
- `main.rs` and `app.rs`: the GUI
- `storage/`: SQLite persistence through sea-orm (entities, migrations, and a diff so a save writes only what changed), plus a one-time import of the old `board.json`

## Checks

Before finishing a change, run these. Both must pass with no warnings.
```sh
cargo fmt && cargo clippy --all-targets && cargo test
```
Keep logic in `model.rs`/`log.rs`, where it can be unit-tested, rather than in `app.rs`. `app.rs` never mutates the model: a new kind of edit gets a `Change` variant in `api.rs`.

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
   `curl -s 127.0.0.1:7979/board` shows what the server holds. A change POSTed with curl, as another client would, shows up in the GUI within about 2 s. To check behaviour on a slow network, start the server with `ITS4TASKS_DELAY_MS=800`, which holds back every answer that long. Edits should still show at once.

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

5. **Rebuild before each look, and stop the app when done.** `cargo clippy` doesn't produce a new binary, so run `cargo build` before relaunching. When finished, run `kill $APP $SERVER`. Don't use `pkill`: the user may have their real instance open.

Check for:
- overlapping or clipped widgets
- even gaps between cards
- items in a row sharing one center line
- layout that doesn't shift when toggles or hover buttons change
- glyphs that render (some emoji are missing from egui's bundled fonts)

Zoom in on any area that looks off. To find why something is the wrong size, temporarily log `response.rect` sizes with `eprintln!` and compare them with what was expected.
