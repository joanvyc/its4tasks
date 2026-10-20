# Agent notes for its4tasks

its4tasks is an egui/eframe desktop todo app: four focus cards plus a task list, with a daily/weekly log view. The code is in `src/`:
- `model.rs`: data and rules
- `log.rs`: log grouping
- `api.rs`: the only way the UI reaches the model. It reads a `&Board` and sends serializable `Change`s to `Api::apply`, which also saves. Tasks and comments have stable UUIDs (`Task::id` is a time-ordered v7, so sorting by it gives creation order; `Note::id` is a v4), and changes address comments by id, never by position
- `app.rs`: UI
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

2. **Run the app under XWayland so X11 tools can see it.** `import` and `tools/xt.py` only work with X11 windows.
   ```sh
   cargo build
   env -u WAYLAND_DISPLAY XDG_DATA_HOME="$DATA" ./target/debug/its4tasks &
   APP=$!
   sleep 2.5
   ```

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

5. **Rebuild before each look, and stop the app when done.** `cargo clippy` doesn't produce a new binary, so run `cargo build` before relaunching. When finished, run `kill $APP`. Don't use `pkill`: the user may have their real instance open.

Check for:
- overlapping or clipped widgets
- even gaps between cards
- items in a row sharing one center line
- layout that doesn't shift when toggles or hover buttons change
- glyphs that render (some emoji are missing from egui's bundled fonts)

Zoom in on any area that looks off. To find why something is the wrong size, temporarily log `response.rect` sizes with `eprintln!` and compare them with what was expected.
