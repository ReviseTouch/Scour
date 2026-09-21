//! The desktop's key that opens Scour, over the page's own route.
//!
//! What a combination is, and which program speaks to which desktop, is
//! `scour-hotkey`'s. What is here is the shape the page reads: reading is a
//! `GET`, setting and clearing carry a body and are a `POST`, and every answer
//! ends in the same state so one drawing serves all three.

use std::net::TcpStream;

use scour_hotkey::{Applies, Desktop, Hotkey, Key, Status};

use crate::http;

/// The route's answer before it reaches a socket. A refusal carries its own
/// status line: a combination that is not one is the caller's mistake, a tool
/// that ran and refused is the desktop's.
#[derive(Debug)]
pub enum Answer {
    Ok(serde_json::Value),
    Fail(&'static str, String),
}

/// Detected once: `Tools::detect` walks `PATH` and starts `gsettings`.
fn detected() -> &'static Hotkey {
    static HOTKEY: std::sync::OnceLock<Hotkey> = std::sync::OnceLock::new();
    HOTKEY.get_or_init(Hotkey::detect)
}

pub fn api_hotkey(stream: &mut TcpStream, req: &http::Req) {
    match answer(detected(), &req.method, &req.body) {
        Answer::Ok(body) => http::json(stream, &body),
        Answer::Fail(status, why) => http::fail(stream, status, &why),
    }
}

/// The route against a given [`Hotkey`], so a test can point it at stub tools
/// and never reach the desktop somebody is sitting at.
pub fn answer(hk: &Hotkey, method: &str, body: &str) -> Answer {
    if method != "POST" {
        return Answer::Ok(state(hk, None));
    }
    let asked: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return Answer::Fail("400 Bad Request", e.to_string()),
    };
    if let Some(text) = asked.get("set").and_then(serde_json::Value::as_str) {
        let key = match Key::parse(text) {
            Ok(k) => k,
            // `KeyError`'s own words: the page shows them beside the field.
            Err(e) => return Answer::Fail("400 Bad Request", e.to_string()),
        };
        return match hk.bind(&key) {
            Ok(applies) => Answer::Ok(state(hk, Some(applies))),
            Err(e) => Answer::Fail("502 Bad Gateway", e.to_string()),
        };
    }
    if asked.get("clear").and_then(serde_json::Value::as_bool) == Some(true) {
        return match hk.clear() {
            Ok(()) => Answer::Ok(state(hk, None)),
            Err(e) => Answer::Fail("502 Bad Gateway", e.to_string()),
        };
    }
    Answer::Fail(
        "400 Bad Request",
        "neither a key to set nor a clear".to_owned(),
    )
}

/// What the desktop says now. `applies` is only on a binding just written.
fn state(hk: &Hotkey, applies: Option<Applies>) -> serde_json::Value {
    // A binding in a spelling this crate does not read is a fact about the
    // desktop, not a failed request: the row still has a command to show.
    let (key, error) = match hk.status() {
        Ok(Status::Bound(k)) => (Some(k.to_string()), None),
        Ok(Status::Unbound | Status::CannotBind { .. }) => (None, None),
        Err(e) => (None, Some(e.to_string())),
    };
    let mut out = serde_json::json!({
        "desktop": match hk.desktop() {
            Desktop::Gnome => "gnome",
            Desktop::Kde => "kde",
            Desktop::Flatpak => "flatpak",
            Desktop::Other => "other",
        },
        "can_bind": hk.can_bind(),
        "key": key,
        "command": hk.command(),
        "error": error,
    });
    if let Some(applies) = applies {
        out["applies"] = serde_json::Value::from(match applies {
            Applies::Now => "now",
            Applies::AfterLogin => "after_login",
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use scour_hotkey::Tools;

    fn ok(a: Answer) -> serde_json::Value {
        match a {
            Answer::Ok(v) => v,
            Answer::Fail(status, why) => panic!("{status}: {why}"),
        }
    }

    fn refused(a: Answer) -> (&'static str, String) {
        match a {
            Answer::Fail(status, why) => (status, why),
            Answer::Ok(v) => panic!("answered {v} where a refusal was due"),
        }
    }

    /// Writing a script and starting one are the same race: a fork on another
    /// thread inherits the file still open for writing, and the exec then says
    /// `Text file busy`. These are the only tests here that start a process.
    #[cfg(unix)]
    fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A `gsettings` keeping its settings in a file beside itself — the stub
    /// from `scour-hotkey`'s own tests, so nothing here touches a desktop.
    #[cfg(unix)]
    fn stub_gsettings(dir: &std::path::Path) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("gsettings");
        std::fs::write(
            &script,
            r#"#!/bin/bash
db="$(dirname "$0")/db"; touch "$db"
case "$1" in
  get)
    line=$(grep -F -- "$2 $3=" "$db" | head -1)
    if [ -n "$line" ]; then printf '%s\n' "${line#*=}"
    elif [ "$3" = custom-keybindings ]; then echo "@as []"
    else echo "''"; fi ;;
  set)
    grep -v -F -- "$2 $3=" "$db" > "$db.new" || true
    printf '%s %s=%s\n' "$2" "$3" "$4" >> "$db.new"; mv "$db.new" "$db" ;;
  reset-recursively)
    grep -v -F -- "$2 " "$db" > "$db.new" || true; mv "$db.new" "$db" ;;
  *) echo "stub: $*" >&2; exit 1 ;;
esac
"#,
        )
        .expect("write stub");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        script
    }

    /// The two KDE tools, keeping their one key in a file beside themselves.
    #[cfg(unix)]
    fn stub_kde(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let write = dir.join("kwriteconfig6");
        let read = dir.join("kreadconfig6");
        std::fs::write(
            &write,
            r#"#!/bin/bash
db="$(dirname "$0")/kdb"; touch "$db"
key="$2|$4|$6|$8"
grep -v -F -- "$key=" "$db" > "$db.new" || true
if [ "$9" != --delete ]; then printf '%s=%s\n' "$key" "$9" >> "$db.new"; fi
mv "$db.new" "$db"
"#,
        )
        .expect("write");
        std::fs::write(
            &read,
            r#"#!/bin/bash
db="$(dirname "$0")/kdb"; touch "$db"
line=$(grep -F -- "$2|$4|$6|$8=" "$db" | head -1)
printf '%s\n' "${line#*=}"
"#,
        )
        .expect("write");
        for s in [&write, &read] {
            std::fs::set_permissions(s, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        (write, read)
    }

    #[cfg(unix)]
    fn gnome(dir: &std::path::Path) -> Hotkey {
        Hotkey::new(Tools {
            gsettings: Some(stub_gsettings(dir)),
            gnome_keys: true,
            desktop_var: "GNOME".into(),
            ..Tools::default()
        })
    }

    /// The state a page can draw without ever reaching a desktop's tools.
    #[test]
    fn a_desktop_nothing_here_can_bind_names_the_command_instead() {
        let hk = Hotkey::new(Tools {
            desktop_var: "XFCE".into(),
            ..Tools::default()
        });
        let s = ok(answer(&hk, "GET", ""));
        assert_eq!(s["desktop"], "other");
        assert_eq!(s["can_bind"], false);
        assert_eq!(s["key"], serde_json::Value::Null);
        assert_eq!(s["command"], "scour-open");
        assert_eq!(s["error"], serde_json::Value::Null);
        assert!(s.get("applies").is_none(), "nothing was bound");
    }

    #[cfg(unix)]
    #[test]
    fn the_key_is_read_set_and_cleared_through_the_one_shape() {
        let _spawning = one_at_a_time();
        let dir = tempfile::tempdir().expect("tempdir");
        let hk = gnome(dir.path());

        let s = ok(answer(&hk, "GET", ""));
        assert_eq!(s["desktop"], "gnome");
        assert_eq!(s["can_bind"], true);
        assert_eq!(s["key"], serde_json::Value::Null);

        let s = ok(answer(&hk, "POST", r#"{"set":"Super+F"}"#));
        // The neutral spelling comes back, not what was typed.
        assert_eq!(s["key"], "super+f");
        assert_eq!(s["applies"], "now");

        // And a plain read afterwards says the same without an `applies`.
        let s = ok(answer(&hk, "GET", ""));
        assert_eq!(s["key"], "super+f");
        assert!(s.get("applies").is_none());

        let s = ok(answer(&hk, "POST", r#"{"clear":true}"#));
        assert_eq!(s["key"], serde_json::Value::Null);
        assert!(s.get("applies").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_combination_that_is_not_one_is_the_callers_mistake() {
        let _spawning = one_at_a_time();
        let dir = tempfile::tempdir().expect("tempdir");
        let hk = gnome(dir.path());
        for (asked, says) in [
            (r#"{"set":"hyper+f"}"#, "not a modifier: hyper"),
            (r#"{"set":"ctrl+"}"#, "a key has to follow the modifiers"),
            (r#"{"set":""}"#, "no key given"),
        ] {
            let (status, why) = refused(answer(&hk, "POST", asked));
            assert_eq!(status, "400 Bad Request", "{asked}");
            assert_eq!(why, says, "{asked}");
        }
        // Nothing was written by any of them.
        assert_eq!(ok(answer(&hk, "GET", ""))["key"], serde_json::Value::Null);

        let (status, _) = refused(answer(&hk, "POST", r#"{"what":1}"#));
        assert_eq!(status, "400 Bad Request");
        let (status, _) = refused(answer(&hk, "POST", "not json"));
        assert_eq!(status, "400 Bad Request");
    }

    #[cfg(unix)]
    #[test]
    fn kde_says_the_binding_waits_for_the_next_login() {
        let _spawning = one_at_a_time();
        let dir = tempfile::tempdir().expect("tempdir");
        let (write, read) = stub_kde(dir.path());
        let hk = Hotkey::new(Tools {
            kwriteconfig: Some(write),
            kreadconfig: Some(read),
            desktop_var: "KDE".into(),
            ..Tools::default()
        });
        let s = ok(answer(&hk, "POST", r#"{"set":"ctrl+alt+s"}"#));
        assert_eq!(s["desktop"], "kde");
        assert_eq!(s["key"], "ctrl+alt+s");
        assert_eq!(s["applies"], "after_login");
    }

    /// A tool that ran and refused is the desktop's failure, not the caller's.
    #[cfg(unix)]
    #[test]
    fn a_tool_that_refuses_comes_back_as_a_bad_gateway() {
        let _spawning = one_at_a_time();
        let hk = Hotkey::new(Tools {
            gsettings: Some("/bin/false".into()),
            gnome_keys: true,
            ..Tools::default()
        });
        let (status, why) = refused(answer(&hk, "POST", r#"{"set":"super+f"}"#));
        assert_eq!(status, "502 Bad Gateway");
        assert!(why.contains("gsettings"), "{why}");
        // A read still answers: the row has a state to draw.
        let s = ok(answer(&hk, "GET", ""));
        assert_eq!(s["key"], serde_json::Value::Null);
        assert!(s["error"].is_string(), "{s}");
    }
}
