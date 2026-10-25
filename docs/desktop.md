# Desktop app (macOS)

`Fleet.app` is the web UI in its own window: a Dock icon, ⌘-Tab, a remembered window, native
menus — instead of a browser tab. It is a thin [Tauri 2](https://tauri.app) shell
(`crates/fleet-desktop`) around this machine's `fleet web serve`; the UI is exactly the one the
server serves, so it never needs its own release.

Why Tauri: it uses the system WebKit (a ~10 MB app, not a bundled Chromium like Electron), it is
Rust, so it lives in this workspace and reads the config through `fleet::core::config`, and it has
first-class hooks for everything a wrapper has to decide (navigation, new windows, downloads, file
drops). A plain WKWebView app in Swift would be smaller still but a second language and toolchain.

## Build and install

On a Mac with the repo's Rust toolchain (arm64 builds an arm64 app):

```sh
crates/fleet-desktop/bundle.sh             # → target/release/bundle/Fleet.app and Fleet.zip
crates/fleet-desktop/bundle.sh --install   # ...and copy it to ~/Applications
```

The script runs `cargo build --release -p fleet-desktop`, writes the bundle (Info.plist, an
`.icns` made from `crates/fleet-desktop/icons/icon.png` with `sips`/`iconutil`) and signs it
ad hoc. No Tauri CLI, Node or Xcode app is needed (the Command Line Tools are). The crate is not a
default workspace member, so a plain `cargo build` / `cargo test` does not build Tauri; build or
test it with `-p fleet-desktop`.

**Another Mac** (e.g. build on the workstation, run on the laptop — same architecture): copy the
bundle over and put it in Applications.

```sh
rsync -a target/release/bundle/Fleet.app laptop:Applications/   # or scp the Fleet.zip and unzip it
```

The app is not notarized. Copied with rsync/scp it has no quarantine flag and just opens; if it
came through a browser, AirDrop or Mail, macOS refuses it once — right-click → Open, or
`xattr -dr com.apple.quarantine ~/Applications/Fleet.app`. The target Mac needs `fleet` itself
(`fleet install --host laptop` from the other machine, or `cargo install`) and a config.

## Which server it opens

The first of:

1. `FLEET_DESKTOP_URL` (env, for testing: run `Fleet.app/Contents/MacOS/fleet-desktop` from a shell);
2. `desktop.url` in `~/.config/fleet/config.json` — e.g. the workstation's URL, to use it without a
   local server (`fleet config set desktop.url http://100.x.y.z:7777`);
3. `hosts.<self>.web` — this machine's own server, the normal case;
4. `http://127.0.0.1:<web.port>` (7777 without a config).

Since every server merges its peers, the local server shows the whole fleet.

**Voice input needs a secure origin.** WebKit only exposes the microphone to HTTPS pages and
`localhost`, so with a remote server over plain `http://100.x.y.z:7777` the composer's mic just
says so. Put the server behind Tailscale HTTPS on that machine (tailnet only, a real certificate):

```sh
tailscale serve --bg --https=7443 http://localhost:7777       # on the workstation
fleet config set desktop.url https://workstation.<tailnet>.ts.net:7443   # on the laptop
```

The server takes the browser's address from `X-Forwarded-For` when the request comes from this
machine's proxy, so "Open in editor" links still know which machine you are on. The first
recording asks for microphone access (`NSMicrophoneUsageDescription`); the server needs a
transcription key (see `POST /api/transcribe` in [architecture](architecture.md)). The same
microphone drives the [voice operator](architecture.md#voice-operator) (`#/operator`).

On launch a small start page checks `GET /api/health`. If the server answers, the window loads
it. If not and the URL is this machine's (3, 4, or a loopback 1/2), the app starts
`fleet --local web serve --port <that port>` (plus `--bind 127.0.0.1` for a loopback URL) with the
login shell's `PATH`, logs to `~/Library/Logs/Fleet/web-serve.log`, and opens the UI once it is
up. A server the app started stops when the app quits; to keep one running (and reachable from the
phone) with the lid open and the app closed, use `fleet web install-service` — the app then just
connects. Anything else — a remote URL that is down, a server that exits, one that does not come up
within 30 s — shows "Fleet server is not running" with the error, the log tail and Retry.
`fleet` is found as config `fleetBin`, else on the login shell's `PATH`, `~/.local/bin`,
`~/.cargo/bin`, `/opt/homebrew/bin`, `/usr/local/bin`.

## Behaviour

| | |
| --- | --- |
| ⌘N | File → New Session: the web UI's New session dialog (the menu dispatches a `fleet:command` DOM event; browsers keep ⌘N, so in a browser tab it is ⌘⇧O) — the other shortcuts are the web UI's own ([web/ui/README.md](../web/ui/README.md#shortcuts)) |
| ⌘R | reload (back to the start page when the server is gone) |
| ⌘⇧R | reconnect: the start page again (re-checks, restarts a server the app owns) |
| ⌘[ / ⌘] | back / forward (the app's hash routes) |
| ⌘W, close button | hide the window; the Dock icon brings it back; ⌘Q quits |
| links | the server's own pages stay in the window; other http(s) pages and every `target="_blank"` link open in the default browser; `vscode://`, `vscode-insiders://`, `cursor://`, `mailto:` go to macOS (**Open in VS Code / Cursor** works); other schemes are dropped |
| attach | `fleet://attach?host=<host>&session=<tmux session>` opens an iTerm tab (a window when none is open) running `fleet -H <host> enter -- <session>`, both words shell-quoted; any other `fleet://` link is dropped. The session details' **Attach** button and the palette's *Attach in iTerm* use it (shown only when the init script sets `data-attach="iterm"`, so an older app keeps just **Copy**). The first use asks for permission to control iTerm (Info.plist `NSAppleEventsUsageDescription`) |
| downloads | the preview's Download saves to `~/Downloads` (no overwrite: `name (1).ext`) and reveals the file in Finder |
| files | dropping files on a session / the New session form, pasting images and the paperclip picker work as in the browser (Tauri's own drop handler is off, so the page gets the drop) |
| clipboard | Copy path and ⌘C/⌘V/⌘X/⌘A work (Edit menu); on a plain-http tailnet URL the UI uses its non-secure-context copy fallback |
| title bar | none: the traffic lights sit over the UI's own top bar (vertically centred on its 56 px row), the page's headers leave room for them and collapse that room in full screen, where macOS hides them |
| dragging | drag the window by the empty parts of any top bar (the top bar, session / settings / notes header, the empty pane); double-click there zooms. Buttons, fields, toggles and links don't drag |
| window | size, position and maximized/fullscreen state are remembered; the title follows the page (`(2) session · Fleet`, shown in the Window menu and Mission Control) |
| single instance | opening the app again focuses the running one |

The server's page gets no app commands: only the bundled start page may call the app's two
commands (`connect`, `open_log`); the other way, a menu item only evaluates a fixed
`window.dispatchEvent(new CustomEvent('fleet:command', …))` in the page (`shell::command`). The web app has no notifications today, so the app asks for no
notification permission.

### Window chrome and the one IPC grant

The window uses Tauri's `titleBarStyle: Overlay` with a hidden title, so the page draws under the
traffic lights. The page learns it is inside the app without IPC: the window's initialization
script (it runs on every page, the server's too) sets `data-shell="desktop"` (and the class
`shell-desktop`), `data-attach="iterm"`, `data-fullscreen`, `--titlebar-inset-left` (88px; 0 in full screen) and
`--titlebar-height` (56px) on `<html>`. On a page load and whenever the window resizes into or
out of full screen, the app evaluates the same script again with the current state. The web UI's
rules for these live in `web/ui/src/index.css` (→ "Desktop app"); without `data-shell` — any
browser — nothing changes.

Dragging uses Tauri's `data-tauri-drag-region` (`"deep"` on the top bars: any non-interactive
part drags; buttons, inputs, links, `tabindex` and `role=button`-like elements don't). Tauri's
drag script turns a mousedown there into the window's `start_dragging` command (and a
double-click into `internal_toggle_maximize`), which is IPC. So the app adds one runtime
capability, `window-drag`: exactly `core:window:allow-start-dragging` and
`core:window:allow-internal-toggle-maximize`, for the `main` window, on the start page and on the
resolved server's origin only (`remote.urls: ["<scheme>://<host>:<port>/*"]`). Every other
command stays denied to the server's page (other window commands, every plugin, `connect`,
`open_log`).

Trade-off: any script running in the server's page — the UI itself, or injected content if the
page were ever compromised — can start a window drag or zoom/unzoom the window. That is all it
can do: no file, shell, navigation or settings access, and a drag follows the mouse until the
button is released. Native dragging without IPC would mean hit-testing the page's layout from Rust,
duplicating the UI's layout knowledge in the app; the narrow grant is the smaller risk.

## Other apps (profiles)

The same crate bundles other web apps as their own Mac app — one binary, no second Tauri
shell. `bundle.sh --profile <name>` writes `Contents/Resources/profile.json`; the binary reads
it at launch, and without one it is Fleet.app, unchanged. A profile app gets:

- its own name, icon and bundle identifier (Info.plist, and Tauri's identifier is overridden at
  launch), so its single-instance lock, remembered window, WebKit storage and macOS prefs are
  separate from Fleet.app's;
- a fixed remote URL: the profile's env var when set (for a run from a shell), else the URL
  baked in at bundle time. It never reads Fleet's config and never starts a server; when
  `GET /api/health` does not answer 200 (an https URL is checked end to end with `curl`, since
  a proxy such as `tailscale serve` accepts connections while the app behind it is down), the
  start page says "<App> server is not running" with Retry;
- the native title bar, or with `"titleBar": "overlay"` Fleet's chrome (Overlay title bar, the
  page script without `data-attach`, the `window-drag` grant on its URL's origin — the page must
  handle `data-shell="desktop"` and `--titlebar-inset-left` itself, see
  [Window chrome](#window-chrome-and-the-one-ipc-grant)); no `fleet://` links, no File →
  New Session. The rest is shared: the link policy, downloads, the Edit menu, ⌘R / ⌘⇧R,
  ⌘[ / ⌘], ⌘W hides, the remembered window, single instance.

Profiles live in `bundle.sh` (name, identifier, icon, category, URL env var, title bar). Today:

| profile | app | identifier | title bar | URL |
| --- | --- | --- | --- | --- |
| `fin` | `Fin.app` | `dev.fleet.desktop.fin` | overlay | `--url`, else `FIN_DESKTOP_URL` at build time; `FIN_DESKTOP_URL` at run time overrides it |
| `brain` | `Brain.app` | `dev.fleet.desktop.brain` | overlay | `--url`, else `BRAIN_DESKTOP_URL` at build time; `BRAIN_DESKTOP_URL` at run time overrides it |

```sh
crates/fleet-desktop/bundle.sh --profile fin --url https://workstation.<tailnet>.ts.net:4443 --install
crates/fleet-desktop/bundle.sh --profile brain --url https://workstation.<tailnet>.ts.net:5443 --install
```

The URL is passed at build time, not committed: it is a private tailnet address.

**Frames.** A navigation inside the page's frames goes through the same link policy, so an
`<iframe>` on another origin would open in the browser instead. `--frame-origin <origin>`
(repeatable) bundles `"frameOrigins": […]` into the profile: those origins load in the window,
frames included. Fin uses it for its assistant panel, which frames Fleet's
[embed view](architecture.md#embed-view) — pass the Fleet web server's origin (same reason as
the URL: not committed). Brain does the same for its assistant panel. Both profiles also ask for the microphone (voice input in that panel).

```sh
crates/fleet-desktop/bundle.sh --profile fin --url https://workstation.<tailnet>.ts.net:4443 \
  --frame-origin https://workstation.<tailnet>.ts.net:7443 --install
```

## Develop

```sh
cargo run -p fleet-desktop                                   # debug build, the config's server
FLEET_DESKTOP_URL=http://127.0.0.1:7796 cargo run -p fleet-desktop   # a test server of your own
cargo test -p fleet-desktop
cargo clippy -p fleet-desktop --all-targets -- -D warnings
# another profile, from a profile file instead of the bundle's
FLEET_DESKTOP_PROFILE=./profile.json cargo run -p fleet-desktop
```

`profile.json`: `{"name": "…", "identifier": "…", "url": "https://…", "urlEnv": "…"}`.

Layout: `src/profile.rs` (Fleet or another app), `src/server.rs` (URL resolution, health probe, starting the server), `src/links.rs` (the
navigation / new-window policy), `src/shell.rs` (traffic-light position, the page's
initialization script, the drag capability's URL pattern), `src/main.rs` (window, menu, plugins), `start/index.html` (the
start page), `tauri.conf.json`, `icons/` (`icon.svg` is the web icon on the macOS icon grid,
`icon.png` its 1024px render; `fin.*` and `brain.*` likewise for `Fin.app` and `Brain.app`. All
of them are generated, with the web icons and the Suite palette, from a shared design system kept
outside this repo — replace them as a set, don't edit by hand).
