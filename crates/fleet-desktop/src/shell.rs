//! The window chrome: no macOS title bar, the traffic lights overlaid on the
//! page's own top bar. The page learns it is inside the app from an
//! initialization script (`data-shell="desktop"`, `data-attach="iterm"` when it opens
//! `fleet://attach` links — Fleet.app only, + CSS variables), not IPC. Used by Fleet and by
//! any profile with `"titleBar": "overlay"`.

use tauri::Url;

/// The traffic lights' `trafficLightPosition` (logical px): the close button's left edge,
/// and a y that puts the 14 px buttons at 21–35 px — centred on the web UI's 56 px top
/// bar (tao sizes the title-bar container to button + y; the buttons sit 9 px above its
/// bottom). The three buttons span x 16–76.
pub const TRAFFIC_LIGHTS: (f64, f64) = (16.0, 30.0);
/// Room the page leaves for the traffic lights at the left of its top bar (76 px + a gap).
pub const INSET_LEFT_PX: u32 = 88;
/// The height of the web UI's top bar row (the traffic lights are centred on it).
pub const TITLEBAR_HEIGHT_PX: u32 = 56;

/// Where the traffic lights go in a window `window_height` tall (AppKit points, origin
/// bottom-left): the title-bar container's y and height, and each button's x. The same
/// layout tao applies for `trafficLightPosition`: the container is button + y tall,
/// pinned to the window's top, the buttons `spacing` apart from x.
pub fn traffic_light_layout(
    window_height: f64,
    button_height: f64,
    spacing: f64,
) -> (f64, f64, [f64; 3]) {
    let (x, y) = TRAFFIC_LIGHTS;
    let height = button_height + y;
    (
        window_height - height,
        height,
        [x, x + spacing, x + 2.0 * spacing],
    )
}

/// Put the traffic lights back at [`TRAFFIC_LIGHTS`]. `setTitle:` makes AppKit lay the
/// title bar out again on its next pass, buttons back in the top-left corner, and the
/// page's `<title>` changes often (the waiting count, the selected session). tao
/// re-applies the position right after its `setTitle:` — before that pass — and
/// otherwise only in its view's `drawRect:` (rare under the webview) and on leaving full
/// screen, so the lights escape until the next resize. This queues a block on the main
/// dispatch queue, after tao's `setTitle:` block, that runs the pending layout first and
/// then moves the buttons. Also called on resize, focus, appearance and display changes,
/// which may lay the title bar out too. A no-op in full screen (the buttons live in
/// another window there).
#[cfg(target_os = "macos")]
pub fn place_traffic_lights(w: &tauri::WebviewWindow) {
    let Ok(ns_window) = w.ns_window() else { return };
    let ns_window = ns_window as usize;
    dispatch2::DispatchQueue::main().exec_async(move || {
        // SAFETY: the main window is never closed (⌘W hides it), so the pointer stays
        // valid; the main queue runs on the main thread.
        unsafe { macos::place(&*(ns_window as *const objc2_app_kit::NSWindow)) }
    });
}

#[cfg(target_os = "macos")]
mod macos {
    use objc2_app_kit::{NSWindow, NSWindowButton, NSWindowStyleMask};

    pub fn place(window: &NSWindow) {
        if window.styleMask().contains(NSWindowStyleMask::FullScreen) {
            return;
        }
        let buttons = [
            NSWindowButton::CloseButton,
            NSWindowButton::MiniaturizeButton,
            NSWindowButton::ZoomButton,
        ]
        .map(|b| window.standardWindowButton(b));
        let [Some(close), Some(mini), Some(zoom)] = buttons else {
            return;
        };
        // SAFETY: plain view-hierarchy getters.
        let Some(container) = (unsafe { close.superview().and_then(|v| v.superview()) }) else {
            return;
        };
        // Run any title-bar layout AppKit has pending now, or it would undo this later.
        if let Some(frame_view) = unsafe { container.superview() } {
            frame_view.layoutSubtreeIfNeeded();
        }
        let spacing = mini.frame().origin.x - close.frame().origin.x;
        let (y, height, xs) = super::traffic_light_layout(
            window.frame().size.height,
            close.frame().size.height,
            spacing,
        );
        let mut frame = container.frame();
        if frame.origin.y != y || frame.size.height != height {
            frame.origin.y = y;
            frame.size.height = height;
            container.setFrame(frame);
        }
        for (button, x) in [close, mini, zoom].iter().zip(xs) {
            let mut origin = button.frame().origin;
            if origin.x != x {
                origin.x = x;
                button.setFrameOrigin(origin);
            }
        }
    }
}

/// The script that tells the page it is in the desktop shell and whether the
/// traffic lights are showing (they hide in full screen, so the inset collapses).
/// Runs as the initialization script of every page and again on full-screen changes.
/// `attach`: the app opens `fleet://attach` and `fleet://shell` links (Fleet.app).
pub fn script(fullscreen: bool, attach: bool) -> String {
    let inset = if fullscreen { 0 } else { INSET_LEFT_PX };
    let attach = if attach {
        "d.dataset.attach='iterm';d.dataset.hostShell='iterm';"
    } else {
        ""
    };
    format!(
        "(function(){{var d=document.documentElement;if(!d)return;\
d.dataset.shell='desktop';d.classList.add('shell-desktop');\
{attach}\
d.dataset.fullscreen='{fullscreen}';\
d.style.setProperty('--titlebar-inset-left','{inset}px');\
d.style.setProperty('--titlebar-height','{TITLEBAR_HEIGHT_PX}px');}})();"
    )
}

/// The script a native menu item runs to ask the web UI for an action (`fleet:command`, the
/// UI's `APP_COMMAND_EVENT`); the page decides whether it applies (not over a dialog, …).
pub fn command(action: &'static str) -> String {
    assert!(
        action.bytes().all(|b| b.is_ascii_alphanumeric()),
        "action {action:?}"
    );
    format!("window.dispatchEvent(new CustomEvent('fleet:command',{{detail:'{action}'}}));")
}

/// The `remote.urls` pattern granting window dragging to the server's pages:
/// the server's origin, any path. `None` for an opaque origin.
pub fn drag_pattern(server: &Url) -> Option<String> {
    let origin = server.origin();
    origin
        .is_tuple()
        .then(|| format!("{}/*", origin.ascii_serialization()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_marks_the_shell_and_sets_the_inset() {
        let s = script(false, true);
        assert!(s.contains("d.dataset.shell='desktop'"));
        assert!(s.contains("classList.add('shell-desktop')"));
        assert!(s.contains("d.dataset.attach='iterm'"));
        assert!(s.contains("d.dataset.hostShell='iterm'"));
        assert!(s.contains("'--titlebar-inset-left','88px'"));
        assert!(s.contains("'--titlebar-height','56px'"));
        assert!(s.contains("fullscreen='false'"));
    }

    #[test]
    fn attach_is_fleet_only() {
        assert!(!script(false, false).contains("attach"));
        assert!(script(false, false).contains("d.dataset.shell='desktop'"));
    }

    #[test]
    fn full_screen_collapses_the_inset() {
        let s = script(true, true);
        assert!(s.contains("'--titlebar-inset-left','0px'"));
        assert!(s.contains("fullscreen='true'"));
    }

    #[test]
    fn traffic_lights_sit_on_the_top_bar() {
        // An 820 pt window, 14 pt buttons 23 pt apart (AppKit's defaults).
        let (y, height, xs) = traffic_light_layout(820.0, 14.0, 23.0);
        assert_eq!((y, height), (776.0, 44.0));
        assert_eq!(xs, [16.0, 39.0, 62.0]);
        // Centred on the page's top bar: 21 pt above, 21 pt below.
        let top = height - 9.0 - 14.0;
        assert_eq!(top, (TITLEBAR_HEIGHT_PX as f64 - 14.0) / 2.0);
    }

    #[test]
    fn command_dispatches_a_dom_event() {
        assert_eq!(
            command("new"),
            "window.dispatchEvent(new CustomEvent('fleet:command',{detail:'new'}));"
        );
    }

    #[test]
    #[should_panic(expected = "action")]
    fn command_rejects_script_text() {
        command("new');alert(1);('");
    }

    #[test]
    fn drag_pattern_is_the_server_origin() {
        let u = |s: &str| Url::parse(s).unwrap();
        assert_eq!(
            drag_pattern(&u("http://127.0.0.1:7777/")).as_deref(),
            Some("http://127.0.0.1:7777/*")
        );
        assert_eq!(
            drag_pattern(&u("http://192.0.2.10:7777/some/path?x=1")).as_deref(),
            Some("http://192.0.2.10:7777/*")
        );
        assert_eq!(
            drag_pattern(&u("https://fleet.example.com")).as_deref(),
            Some("https://fleet.example.com/*")
        );
        assert_eq!(drag_pattern(&u("data:text/plain,x")), None);
    }
}
