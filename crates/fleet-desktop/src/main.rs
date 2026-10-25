//! Fleet as a macOS app: one window on this machine's Fleet web server
//! (`fleet web serve`), which the app starts when nothing is listening.
//! The server's page gets no app commands (only the bundled start page, `start/`,
//! does) — just the window-dragging permission its top bar needs (see `shell`).
//! A bundled profile (see `profile`) makes the same binary a plain window on
//! another, remote web app instead.

mod links;
mod profile;
mod server;
mod shell;

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::ipc::CapabilityBuilder;
use tauri::menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::webview::{DownloadEvent, NewWindowResponse, PageLoadEvent};
use tauri::{
    AppHandle, Manager, RunEvent, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent,
};
use tauri_plugin_window_state::{StateFlags, WindowExt};

use links::Action;
use profile::Profile;

const START_PAGE: &str = "tauri://localhost/index.html";
/// How long a server the app started gets to answer before the start page gives up.
const START_GRACE: Duration = Duration::from_secs(30);
const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);
/// A TLS handshake over the tailnet included.
const REMOTE_PROBE_TIMEOUT: Duration = Duration::from_secs(4);

struct State {
    profile: Profile,
    target: Result<server::Target, String>,
    /// Computed on first use: asking the login shell can take a moment.
    path: OnceLock<String>,
    /// The `fleet web serve` this app started, and when.
    child: Mutex<Option<(Child, Instant)>>,
    downloads: Mutex<HashMap<String, PathBuf>>,
    /// The full-screen state the page was last told (see `sync_shell`).
    fullscreen: AtomicBool,
}

impl State {
    fn path(&self) -> &str {
        self.path.get_or_init(server::server_path)
    }

    /// Does the server answer? Another profile's https server is checked end to end
    /// (Fleet's is not, as before: a reachable port counts).
    fn probe(&self, url: &Url) -> Result<(), String> {
        if self.profile.is_fleet() {
            server::probe(url, PROBE_TIMEOUT)
        } else {
            server::probe_strict(url, REMOTE_PROBE_TIMEOUT)
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Status {
    /// `ok` (navigating to the server), `starting`, `down` or `error` (bad config).
    state: &'static str,
    url: Option<String>,
    source: Option<String>,
    error: Option<String>,
    log: Option<String>,
    log_path: Option<String>,
    /// The app's name (the profile's), for the start page's titles.
    app: String,
    /// Fleet.app: the start page shows Fleet's help and keeps its drag region.
    fleet: bool,
}

impl Status {
    fn new(state: &'static str, t: Option<&server::Target>) -> Self {
        Status {
            app: String::new(),
            fleet: false,
            state,
            url: t.map(|t| t.url.to_string()),
            source: t.map(|t| t.source.clone()),
            error: None,
            log: None,
            log_path: None,
        }
    }
    fn error(mut self, e: impl Into<String>) -> Self {
        self.error = Some(e.into());
        self
    }
    fn with_log(mut self) -> Self {
        let p = server::log_path();
        self.log = Some(server::tail(&p, 20)).filter(|l| !l.is_empty());
        self.log_path = Some(p.display().to_string());
        self
    }
}

/// Start page → is the server up? Navigates the window there when it is;
/// otherwise starts `fleet web serve` once (for this machine's server).
#[tauri::command]
async fn connect(window: WebviewWindow) -> Status {
    let profile = window.state::<State>().profile.clone();
    let mut s = tauri::async_runtime::spawn_blocking(move || connect_blocking(&window))
        .await
        .unwrap_or_else(|e| Status::new("error", None).error(e.to_string()));
    s.app = profile.name().to_string();
    s.fleet = profile.is_fleet();
    s
}

fn connect_blocking(window: &WebviewWindow) -> Status {
    let st = window.state::<State>();
    let t = match &st.target {
        Ok(t) => t,
        Err(e) => return Status::new("error", None).error(e.clone()),
    };
    let err = match st.probe(&t.url) {
        Ok(()) => {
            return match window.navigate(t.url.clone()) {
                Ok(()) => Status::new("ok", Some(t)),
                Err(e) => Status::new("error", Some(t)).error(e.to_string()),
            };
        }
        Err(e) => e,
    };
    let down = Status::new("down", Some(t));
    if !t.local {
        return down.error(err);
    }
    let mut child = st.child.lock().unwrap();
    if let Some((c, since)) = child.as_mut() {
        return match c.try_wait() {
            Ok(Some(exit)) => {
                *child = None;
                down.error(format!("fleet web serve exited ({exit})"))
                    .with_log()
            }
            _ if since.elapsed() < START_GRACE => Status::new("starting", Some(t)).with_log(),
            _ => down
                .error(format!("started fleet web serve, but {err}"))
                .with_log(),
        };
    }
    let Some(fleet) = server::fleet_bin(fleet::core::config::get(), st.path()) else {
        return down.error(format!(
            "{err}\n\n`fleet` was not found on the login shell's PATH, ~/.local/bin or ~/.cargo/bin — set `fleetBin` in the config"
        ));
    };
    match server::spawn(
        &fleet,
        &server::serve_args(t),
        st.path(),
        &server::log_path(),
    ) {
        Ok(c) => {
            *child = Some((c, Instant::now()));
            Status::new("starting", Some(t)).with_log()
        }
        Err(e) => down.error(e),
    }
}

#[tauri::command]
fn open_log() {
    open_external(&server::log_path().display().to_string());
}

fn open_external(target: &str) {
    if let Err(e) = Command::new("/usr/bin/open").arg(target).spawn() {
        eprintln!("fleet-desktop: open {target}: {e}");
    }
}

/// Open an iTerm tab running `fleet -H <host> enter <session>`.
fn attach(host: &str, session: &str) {
    run_in_iterm(fleet::core::backend::attach_command(host, session));
}

/// Open an iTerm tab in the host's tmux: `fleet -H <host> new` (attach to, or create, `main`).
/// This machine gets a plain iTerm tab.
fn shell(host: &str) {
    use fleet::core::hosts::{Scope, Target, resolve};
    let cfg = fleet::core::config::get();
    if let Ok(Target::Local { .. }) = resolve(cfg, Some(host), false, Scope::DefaultHost) {
        std::thread::spawn(|| {
            if let Err(e) = fleet::core::backend::iterm_open() {
                eprintln!("fleet-desktop: iTerm: {e}");
            }
        });
        return;
    }
    run_in_iterm(fleet::core::backend::host_shell_command(host));
}

/// Run `line` in a new iTerm tab, off the UI thread (the AppleScript waits for the tab's shell).
fn run_in_iterm(line: String) {
    std::thread::spawn(move || {
        if let Err(e) = fleet::core::backend::iterm_run(&line, false) {
            eprintln!("fleet-desktop: iTerm ({line}): {e}");
        }
    });
}

/// The menu bar. File → New Session (a `fleet:command`) is Fleet's only.
fn menu(app: &AppHandle, profile: &Profile) -> tauri::Result<Menu<tauri::Wry>> {
    let item =
        |id: &str, label: &str, accel: &str| MenuItem::with_id(app, id, label, true, Some(accel));
    let sep = || PredefinedMenuItem::separator(app);
    let name = profile.name();
    let app_menu = Submenu::with_items(
        app,
        name,
        true,
        &[
            &PredefinedMenuItem::about(app, Some(&format!("About {name}")), None)?,
            &sep()?,
            &PredefinedMenuItem::services(app, None)?,
            &sep()?,
            &PredefinedMenuItem::hide(app, None)?,
            &PredefinedMenuItem::hide_others(app, None)?,
            &PredefinedMenuItem::show_all(app, None)?,
            &sep()?,
            &PredefinedMenuItem::quit(app, None)?,
        ],
    )?;
    let file = Submenu::with_items(
        app,
        "File",
        true,
        &[&item("new-session", "New Session", "CmdOrCtrl+N")?],
    )?;
    let mut menus: Vec<&dyn IsMenuItem<tauri::Wry>> = vec![&app_menu];
    if profile.is_fleet() {
        menus.push(&file);
    }
    let rest = [
        Submenu::with_items(
            app,
            "Edit",
            true,
            &[
                &PredefinedMenuItem::undo(app, None)?,
                &PredefinedMenuItem::redo(app, None)?,
                &sep()?,
                &PredefinedMenuItem::cut(app, None)?,
                &PredefinedMenuItem::copy(app, None)?,
                &PredefinedMenuItem::paste(app, None)?,
                &PredefinedMenuItem::select_all(app, None)?,
            ],
        )?,
        Submenu::with_items(
            app,
            "View",
            true,
            &[
                &item("reload", "Reload", "CmdOrCtrl+R")?,
                &item("reconnect", "Reconnect to Server", "CmdOrCtrl+Shift+R")?,
                &sep()?,
                &item("back", "Back", "CmdOrCtrl+[")?,
                &item("forward", "Forward", "CmdOrCtrl+]")?,
                &sep()?,
                &PredefinedMenuItem::fullscreen(app, None)?,
            ],
        )?,
        Submenu::with_items(
            app,
            "Window",
            true,
            &[
                &PredefinedMenuItem::minimize(app, None)?,
                &PredefinedMenuItem::maximize(app, None)?,
                &sep()?,
                &PredefinedMenuItem::close_window(app, None)?,
            ],
        )?,
    ];
    menus.extend(rest.iter().map(|m| m as &dyn IsMenuItem<tauri::Wry>));
    Menu::with_items(app, &menus)
}

fn start_page() -> Url {
    Url::parse(START_PAGE).expect("static URL")
}

fn on_menu(app: &AppHandle, id: &str) {
    let Some(w) = app.get_webview_window("main") else {
        return;
    };
    match id {
        // Browsers keep ⌘N for themselves, so the page binds it only through this menu.
        "new-session" => drop(w.eval(shell::command("new"))),
        "back" => drop(w.eval("history.back()")),
        "forward" => drop(w.eval("history.forward()")),
        "reconnect" => drop(w.navigate(start_page())),
        // Reloading a server that is gone would leave a blank error page.
        "reload" => {
            std::thread::spawn(move || {
                let on_server = w.url().is_ok_and(|u| u.scheme().starts_with("http"));
                let st = w.state::<State>();
                let up = st.target.as_ref().is_ok_and(|t| st.probe(&t.url).is_ok());
                if on_server && !up {
                    let _ = w.navigate(start_page());
                } else {
                    let _ = w.eval("location.reload()");
                }
            });
        }
        _ => {}
    }
}

fn build_window(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let st = app.state::<State>();
    let fleet = st.profile.is_fleet();
    let overlay = st.profile.overlay();
    let name = st.profile.name().to_string();
    let server = st
        .target
        .as_ref()
        .map(|t| t.url.clone())
        .unwrap_or_else(|_| start_page());
    let frames = st.profile.frame_origins();
    let dl = app.clone();
    let builder = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title(&name)
        .inner_size(1280.0, 820.0)
        .min_inner_size(420.0, 480.0)
        .visible(false)
        // Let the page get file drops (composer attachments) instead of Tauri.
        .disable_drag_drop_handler()
        .on_navigation(move |url| match links::navigation(url, &server, &frames) {
            Action::Allow => true,
            Action::Open => {
                open_external(url.as_str());
                false
            }
            // `fleet://` links are Fleet.app's only.
            Action::Attach { host, session } => {
                if fleet {
                    attach(&host, &session);
                }
                false
            }
            Action::Shell { host } => {
                if fleet {
                    shell(&host);
                }
                false
            }
            Action::Deny => false,
        })
        .on_new_window(|url, _| {
            if links::new_window(&url) == Action::Open {
                open_external(url.as_str());
            }
            NewWindowResponse::Deny
        })
        .on_document_title_changed(move |w, title| {
            let _ = w.set_title(if title.trim().is_empty() {
                &name
            } else {
                &title
            });
            // A new title lays the title bar out again (see `shell::place_traffic_lights`).
            place_traffic_lights(&w);
        })
        .on_download(move |_, ev| {
            let st = dl.state::<State>();
            match ev {
                DownloadEvent::Requested { url, destination } => {
                    st.downloads
                        .lock()
                        .unwrap()
                        .insert(url.to_string(), destination.clone());
                }
                // macOS reports no path on finish: reveal the one chosen at request time.
                DownloadEvent::Finished { url, success, .. } => {
                    let dest = st.downloads.lock().unwrap().remove(url.as_str());
                    if let (true, Some(d)) = (success, dest) {
                        let _ = Command::new("/usr/bin/open").arg("-R").arg(d).spawn();
                    }
                }
                _ => {}
            }
            true
        });
    // Fleet's chrome: no title bar, the traffic lights over the UI's own top bar, and
    // the page script telling the UI so. Other profiles opt in (`"titleBar": "overlay"`).
    let builder = if overlay {
        let b = builder
            .initialization_script(shell::script(false, fleet))
            .on_page_load(|w, p| {
                if p.event() == PageLoadEvent::Finished {
                    sync_shell(&w, true);
                }
            });
        #[cfg(target_os = "macos")]
        let b = b
            .title_bar_style(tauri::TitleBarStyle::Overlay)
            .hidden_title(true)
            .traffic_light_position(tauri::LogicalPosition::new(
                shell::TRAFFIC_LIGHTS.0,
                shell::TRAFFIC_LIGHTS.1,
            ));
        b
    } else {
        builder
    };
    let w = builder.build()?;
    let _ = w.restore_state(StateFlags::all() & !StateFlags::VISIBLE);
    w.show()?;
    Ok(w)
}

/// Tell the page whether the window is in full screen (the traffic lights hide
/// there, so its top-bar inset collapses) — when it changed, or always (`force`,
/// after a page load, which starts from the initialization script's default).
fn sync_shell(w: &WebviewWindow, force: bool) {
    let st = w.state::<State>();
    if !st.profile.overlay() {
        return;
    }
    let Ok(fs) = w.is_fullscreen() else { return };
    let last = st.fullscreen.swap(fs, Ordering::Relaxed);
    if force || last != fs {
        let _ = w.eval(shell::script(fs, st.profile.is_fleet()));
    }
}

/// Put the traffic lights back where the overlay chrome wants them (overlay windows only).
fn place_traffic_lights(w: &WebviewWindow) {
    #[cfg(target_os = "macos")]
    if w.state::<State>().profile.overlay() {
        shell::place_traffic_lights(w);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = w;
}

/// Window dragging for the page's top bar (`data-tauri-drag-region`): Tauri's
/// drag script calls these two window commands. Granted to the start page and
/// the server's origin, in this window only — nothing else is exposed to the page.
fn allow_window_drag(app: &AppHandle) -> tauri::Result<()> {
    let mut cap = CapabilityBuilder::new("window-drag")
        .window("main")
        .permission("core:window:allow-start-dragging")
        .permission("core:window:allow-internal-toggle-maximize");
    let st = app.state::<State>();
    if let Some(p) = st
        .target
        .as_ref()
        .ok()
        .and_then(|t| shell::drag_pattern(&t.url))
    {
        cap = cap.remote(p);
    }
    app.add_capability(cap)
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

fn main() {
    let profile = profile::load().unwrap_or_else(|e| {
        eprintln!("fleet-desktop: profile: {e}");
        std::process::exit(1);
    });
    let target = match &profile {
        Profile::Fleet => {
            let loaded = fleet::core::config::load();
            let env_url = std::env::var("FLEET_DESKTOP_URL").ok();
            match &loaded.problem {
                Some(p) => Err(p.clone()),
                None => server::resolve(env_url.as_deref(), loaded),
            }
        }
        Profile::Remote(r) => {
            let env_url = r.url_env.as_deref().and_then(|k| std::env::var(k).ok());
            server::resolve_remote(env_url.as_deref(), r)
        }
    };
    let mut context = tauri::generate_context!();
    if let Profile::Remote(r) = &profile {
        // Its own identifier: its own single-instance socket, window state and config dir.
        context.config_mut().identifier = r.identifier.clone();
        context.config_mut().product_name = Some(r.name.clone());
        context.package_info_mut().name = r.name.clone();
    }
    let menu_profile = profile.clone();
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            show_main(app)
        }))
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(StateFlags::all() & !StateFlags::VISIBLE)
                .build(),
        )
        .manage(State {
            profile,
            target,
            path: OnceLock::new(),
            child: Mutex::new(None),
            downloads: Mutex::new(HashMap::new()),
            fullscreen: AtomicBool::new(false),
        })
        .invoke_handler(tauri::generate_handler![connect, open_log])
        .menu(move |app| menu(app, &menu_profile))
        .on_menu_event(|app, ev| on_menu(app, ev.id().as_ref()))
        .setup(|app| {
            if app.state::<State>().profile.overlay() {
                allow_window_drag(app.handle())?;
            }
            build_window(app.handle())?;
            Ok(())
        })
        .on_window_event(|w, ev| {
            // ⌘W / the close button hide the window; ⌘Q quits.
            match ev {
                WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    let _ = w.hide();
                }
                // Entering / leaving full screen resizes the window.
                WindowEvent::Resized(_) => {
                    if let Some(wv) = w.app_handle().get_webview_window(w.label()) {
                        sync_shell(&wv, false);
                        place_traffic_lights(&wv);
                    }
                }
                // AppKit may lay the title bar out again on these (see `shell`).
                WindowEvent::Focused(_)
                | WindowEvent::ThemeChanged(_)
                | WindowEvent::ScaleFactorChanged { .. } => {
                    if let Some(wv) = w.app_handle().get_webview_window(w.label()) {
                        place_traffic_lights(&wv);
                    }
                }
                _ => {}
            }
        })
        .build(context)
        .expect("error while building the app");
    app.run(|app, ev| match ev {
        RunEvent::Reopen { .. } => show_main(app),
        RunEvent::Exit => {
            if let Some((mut c, _)) = app.state::<State>().child.lock().unwrap().take() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
        _ => {}
    });
}
