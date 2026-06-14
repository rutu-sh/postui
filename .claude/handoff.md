# postui — Session Handoff

A terminal HTTP client TUI in Rust. **Inspired by** Postman — not a clone, not a port. Keyboard-driven, single-pane, designed for quick request iteration from the terminal.

## Stack
- Rust edition 2024
- `ratatui = "0.30"` — TUI framework
- `crossterm = "0.29"` — terminal IO
- `ureq = "2.10"` — blocking HTTP client (default features pull rustls + webpki-roots; no system TLS needed)
- `serde = "1"` + `serde_json = "1"` — collection persistence

## File layout
```
src/main.rs        entry point
src/tui/mod.rs     module
src/tui/app.rs     everything — state, key handling, rendering
src/tui/layout.rs  stub from initial scaffolding (dead-code warning)
```

`src/tui/app.rs` is monolithic (~1600 lines). Splitting into `buffer.rs`, `kv.rs`, `render.rs`, `persist.rs` would be a reasonable refactor; not done yet.

## Architecture

`App` owns all state. `App::run` loops:
1. `terminal.draw(|f| self.draw(f))`
2. `event::poll(50ms)` → `event::read()` → `handle_key`
3. `poll_response()` polls the worker `mpsc::Receiver` for any HTTP response that finished

### Focus model
Top-level `Focus`: `Method`, `Url`, `Send`, `Params`, `Response`, `Sidebar`.

Cycle (forward): `Sidebar → Method → URL → Send → Params(active_tab) → Response → Sidebar`. Inside `Focus::Params`:
- Sub-state `ParamsSubFocus::{ Tabs, Editor }`.
- `active_tab: RequestTab::{ Params, Headers, Body }` decides which editor shows.
- Tab within Params cycles `Params → Headers → Body → Response`; sub-focus is preserved, so you can Tab-walk between editors without leaving edit mode.

### Editing primitives
- `TextBuffer` — multi-line text editor (cursor row+col, vertical scroll). Used for the request body and as the design template.
- `KvEditor` — structured 3-column editor backed by `Vec<KvRow>` (`enabled: bool`, `key`, `value`). Columns: `Enabled` / `Key` / `Value`. Per-cell cursor, scroll, Tab/Shift+Tab walk cells, Enter advances Key→Value→next row (appends a new empty row at the end), Ctrl+D removes the row. Disabled rows render with strikethrough and are excluded from `entries()`.
- URL field — simple inline string + byte cursor.

### Networking
`send_request` snapshots the current state, opens an `mpsc::channel`, spawns a thread, builds a `ureq::AgentBuilder` with 10s connect / 30s read timeouts, applies params via `req.query(k, v)` and headers via `req.set(k, v)`, then calls either `req.call()` or `req.send_string(body)` based on `HttpMethod::allows_body()`. Result goes back through the channel; `poll_response()` consumes it on the next tick. Response body renders with word-wrap. Vertical scroll uses `wrapped_line_count()` (a char-based upper bound) since ratatui 0.30's `Paragraph::line_count` is `pub(crate)`.

### Persistence
`~/.postui_collection.json` — JSON array of `SavedRequest`. Loaded on startup in `App::default()`. `Ctrl+S` either overwrites the linked entry (via `current_request_idx`) or appends; `Ctrl+O` re-reads the file from disk. `state_file_path()` falls back to `.` if `$HOME` is unset.

`SavedRequest`:
- `name: Option<String>` (`#[serde(default)]` for back-compat)
- `method`, `url`, `params: Vec<KvRow>`, `headers: Vec<KvRow>`, `body: String`
- `last_response: Option<ResponseDisplay>` — captured only when current state is `ResponseState::Done`

### Layout
- Row 0: 1-line text header (app name, no border)
- Sidebar (when `show_sidebar`): `TOP|LEFT|BOTTOM` borders, width 30
- Body: borders are `TOP|RIGHT|BOTTOM` when the sidebar is shown (no `LEFT`, so its horizontals join the sidebar's) or all four otherwise
- Body interior: URL row at the top (Method ¦ URL ¦ Send in one rounded box, with manual `┬`/`┴` junctions), then a configurable split (`split_ratio`, default 50) between Params/Headers/Body editor (top) and Response (bottom), separated by a single `├─ RESPONSE … ─┤` divider line with title

The URL row's top overlaps the body's top border via manual junction overrides at `(area.x, area.y + 2)` and `(area.x + area.width - 1, area.y + 2)`. The left junction is skipped when the body has no `LEFT` border.

## Keybindings

### Global
- `Ctrl+C` — quit
- `Ctrl+S` — save (overwrite linked entry, else append + link)
- `Ctrl+O` — reload collection from disk
- `Ctrl+B` — toggle focus to/from sidebar (auto-shows it if hidden)
- `Shift+↑/↓` — resize params/response split (±5%, clamped to [15, 85])

### Method / Send
- `q` — quit
- `h/s/f` — toggle header / sidebar / footer panels
- `Tab` / `Shift+Tab` — cycle focus
- `Enter`/`↓` on Method — open method dropdown
- `Enter`/`Space` on Send — send request

### URL field
- Type, arrows, Home, End, Backspace, Delete
- `Enter` — send

### Params/Headers (KV editor)
- `Tab` / `Shift+Tab` — next / previous cell (across Enabled / Key / Value)
- `Enter` — advances Key→Value, Value→next row Key (appends a row at the end)
- `Space` or `Enter` on Enabled — toggle checkbox
- `←/→` — within-cell cursor, jumps to adjacent cell at edges
- `↑/↓` — change row (`↑` at row 0 returns to tab strip)
- `PgUp/PgDn` — page-sized row jump
- `Ctrl+D` — delete current row

### Body editor
- Plain text editing; `Enter` → newline; `Tab` → two spaces (indent); `Shift+Tab` → cycle focus back; `↑` at row 0 → tab strip

### Response (focused)
- `↑/↓` or `j/k` — scroll one line
- `PgUp/PgDn` — half page
- `Home`/`g`, `End`/`G` — top/bottom

### Sidebar
- `↑/↓` or `j/k` — navigate (`+ New Request` row + saved entries)
- `Enter` on `+ New Request` — clear all state, jump focus to URL
- `Enter` on saved entry — load it, link `current_request_idx`
- `r` on saved entry — start inline rename (`Enter` commits + persists, `Esc` cancels)
- `d` on saved entry — delete + persist

### Method dropdown
- `↑/↓`, `Enter`, `Esc`

### Esc semantics
`Esc` never quits (intentional). In the Params editor it returns to the tab strip; in the method dropdown or rename it cancels.

## Known issues / open questions
- **Save during `InFlight` clobbers `last_response`.** If a saved request is loaded, the user clicks Send again, then `Ctrl+S` before the response arrives, the overwrite snapshot has `last_response = None` and stomps the previously saved response. User flagged this; design decision pending. Fix: preserve `last_response` from the existing entry when the current state isn't `Done`.
- **URL field doesn't horizontally scroll.** Long URLs visually clip at the cell's right edge; cursor clamps.
- **KV cells don't horizontally scroll within a cell.** Same issue.
- **Body editor: no syntax highlighting, no soft wrap.**
- **Wrapped-line counting** in the response is char-based, so scroll bounds are an upper estimate — you can scroll slightly past the actual end and see blank space.
- **`src/tui/layout.rs`** is a stub from the initial scaffold; it emits a `dead_code` warning. Either delete it or build it out.
- **`app.rs` size.** ~1600 lines, monolithic. Splitting would help readability.

## Storage details
Path defined in `collection_file_path()`:
```rust
let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
std::path::PathBuf::from(home).join(".postui_collection.json")
```

Format: a top-level JSON array. Each element is one `SavedRequest`. Hand-edit then `Ctrl+O` to reload.

## Running
```bash
cargo run
```
No CLI args. URL starts empty; user types one in.

## Debugging
TUIs are painful to debug in-process. `eprintln!` corrupts the terminal in raw mode. For state issues, append to a file:
```rust
use std::io::Write;
let mut f = std::fs::OpenOptions::new().append(true).create(true).open("/tmp/postui.log").unwrap();
writeln!(f, "focus={:?} sub={:?}", self.focus, self.params_sub_focus).ok();
```

## Style notes from this session
- User prefers terse responses; cite `file_path:line` for references.
- No emojis anywhere.
- No comments unless the *why* is non-obvious.
- Direct fixes, no premature abstraction.
- On macOS: `Ctrl+arrow` conflicts with Mission Control, so resize is bound to `Shift+arrow`.
