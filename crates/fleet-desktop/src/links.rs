//! What happens to a URL the page navigates to or opens in a new window.

use tauri::Url;

/// Schemes handed to macOS (`open`) — editor links and mail. Anything else
/// that is not the app's own origin or http(s) is dropped.
const OS_SCHEMES: &[&str] = &["vscode", "vscode-insiders", "cursor", "mailto"];

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Load it in the app's window.
    Allow,
    /// Hand it to the OS: the default browser for http(s), the registered app otherwise.
    Open,
    /// `fleet://attach?host=<h>&session=<tmux session>`: open an iTerm tab attached to it.
    Attach {
        host: String,
        session: String,
    },
    /// `fleet://shell?host=<h>`: open an iTerm tab in the host's tmux (`fleet -H <h> new`).
    Shell {
        host: String,
    },
    Deny,
}

/// The app's own `fleet://` links: `attach` (a host and a session) and `shell` (a host).
fn fleet_link(url: &Url) -> Action {
    let q = |k: &str| {
        url.query_pairs()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.into_owned())
            .filter(|v| !v.is_empty())
    };
    match (url.host_str(), q("host"), q("session")) {
        (Some("attach"), Some(host), Some(session)) => Action::Attach { host, session },
        (Some("shell"), Some(host), _) => Action::Shell { host },
        _ => Action::Deny,
    }
}

fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host() == b.host()
        && a.port_or_known_default() == b.port_or_known_default()
}

/// A navigation of the window (or of a frame in it). The app's own pages, the
/// Fleet server and the profile's frame origins (`frames`) stay in the window; other
/// web pages open in the browser.
pub fn navigation(url: &Url, server: &Url, frames: &[Url]) -> Action {
    match url.scheme() {
        "tauri" | "about" | "blob" | "data" => Action::Allow,
        "http" | "https" if url.host_str() == Some("tauri.localhost") => Action::Allow,
        "http" | "https" if same_origin(url, server) => Action::Allow,
        "http" | "https" if frames.iter().any(|f| same_origin(url, f)) => Action::Allow,
        "http" | "https" => Action::Open,
        s if OS_SCHEMES.contains(&s) => Action::Open,
        "fleet" => fleet_link(url),
        _ => Action::Deny,
    }
}

/// A `target="_blank"` link or `window.open`: never a second app window.
pub fn new_window(url: &Url) -> Action {
    match url.scheme() {
        "http" | "https" => Action::Open,
        s if OS_SCHEMES.contains(&s) => Action::Open,
        _ => Action::Deny,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn server_origin_stays_in_the_window() {
        let server = u("http://127.0.0.1:7777/");
        assert_eq!(
            navigation(&u("http://127.0.0.1:7777/#/s/laptop/abc"), &server, &[]),
            Action::Allow
        );
        assert_eq!(
            navigation(&u("http://127.0.0.1:7777/api/x"), &server, &[]),
            Action::Allow
        );
        assert_eq!(
            navigation(&u("tauri://localhost/index.html"), &server, &[]),
            Action::Allow
        );
        assert_eq!(navigation(&u("about:blank"), &server, &[]), Action::Allow);
    }

    #[test]
    fn other_origins_go_to_the_browser() {
        let server = u("http://127.0.0.1:7777/");
        assert_eq!(
            navigation(&u("http://127.0.0.1:7778/"), &server, &[]),
            Action::Open
        );
        assert_eq!(
            navigation(&u("https://127.0.0.1:7777/"), &server, &[]),
            Action::Open
        );
        assert_eq!(
            navigation(&u("https://github.com/owner/repo/pull/1"), &server, &[]),
            Action::Open
        );
    }

    #[test]
    fn editor_links_go_to_the_os_and_the_rest_is_dropped() {
        let server = u("http://127.0.0.1:7777/");
        assert_eq!(
            navigation(&u("vscode://file/tmp/x"), &server, &[]),
            Action::Open
        );
        assert_eq!(
            navigation(&u("cursor://file/tmp/x"), &server, &[]),
            Action::Open
        );
        assert_eq!(
            navigation(&u("file:///etc/passwd"), &server, &[]),
            Action::Deny
        );
        assert_eq!(
            navigation(&u("javascript:alert(1)"), &server, &[]),
            Action::Deny
        );
        assert_eq!(
            navigation(&u("x-apple-systempreferences:foo"), &server, &[]),
            Action::Deny
        );
    }

    #[test]
    fn frame_origins_stay_in_the_window() {
        let server = u("https://app.example.com:4443/");
        let frames = [u("https://app.example.com:7443")];
        assert_eq!(
            navigation(
                &u("https://app.example.com:7443/?host=h&dir=x#/embed"),
                &server,
                &frames
            ),
            Action::Allow
        );
        assert_eq!(
            navigation(&u("https://app.example.com:7444/"), &server, &frames),
            Action::Open
        );
        assert_eq!(
            navigation(&u("https://app.example.com:7443/"), &server, &[]),
            Action::Open
        );
    }

    #[test]
    fn new_windows_open_outside() {
        assert_eq!(new_window(&u("http://127.0.0.1:7777/api/x")), Action::Open);
        assert_eq!(new_window(&u("https://example.com/")), Action::Open);
        assert_eq!(new_window(&u("vscode://file/tmp")), Action::Open);
        assert_eq!(new_window(&u("file:///tmp")), Action::Deny);
    }

    #[test]
    fn fleet_attach_links_carry_host_and_session() {
        let server = u("http://127.0.0.1:7777/");
        assert_eq!(
            navigation(
                &u("fleet://attach?host=workstation&session=a%20b"),
                &server,
                &[]
            ),
            Action::Attach {
                host: "workstation".into(),
                session: "a b".into()
            }
        );
        assert_eq!(
            navigation(&u("fleet://shell?host=workstation"), &server, &[]),
            Action::Shell {
                host: "workstation".into()
            }
        );
        let deny = [
            "fleet://shell?host=",
            "fleet://attach?host=workstation",
            "fleet://attach?host=&session=x",
            "fleet://run?host=h&session=x",
        ];
        for d in deny {
            assert_eq!(navigation(&u(d), &server, &[]), Action::Deny, "{d}");
        }
        assert_eq!(
            new_window(&u("fleet://attach?host=h&session=x")),
            Action::Deny
        );
    }
}
