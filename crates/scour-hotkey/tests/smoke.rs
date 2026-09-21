//! The public API, against stubs standing in for the desktops' tools.

use scour_hotkey::{Applies, Desktop, Error, Hotkey, Key, KeyError, Modifier, Status, Tools};

fn key(s: &str) -> Key {
    Key::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
}

#[test]
fn the_neutral_spelling_is_read_case_aside_and_written_lowercase() {
    assert_eq!(key("super+f").to_string(), "super+f");
    assert_eq!(key("Super+F").to_string(), "super+f");
    assert_eq!(key("ctrl+alt+s").to_string(), "ctrl+alt+s");
    // Modifiers come out in one order whatever the order typed.
    assert_eq!(
        key("shift+alt+ctrl+super+x").to_string(),
        "super+ctrl+alt+shift+x"
    );
    assert_eq!(key("win+space").to_string(), "super+space");
    assert_eq!(key("Control+Option+Enter").to_string(), "ctrl+alt+return");
    assert_eq!(key("super+F2").to_string(), "super+F2");
    assert_eq!(key("super+f12").to_string(), "super+F12");
    assert_eq!(key("ctrl+\"").to_string(), "ctrl+\"");
    assert_eq!(key("ctrl++").to_string(), "ctrl++");
    assert_eq!(key("f").to_string(), "f");
    assert_eq!(key("ctrl+PgUp").to_string(), "ctrl+pageup");
    assert_eq!(
        key("super+ctrl+x").modifiers(),
        &[Modifier::Super, Modifier::Ctrl]
    );
}

#[test]
fn what_is_not_a_key_is_said_plainly() {
    assert_eq!(Key::parse(""), Err(KeyError::Empty));
    assert_eq!(Key::parse("   "), Err(KeyError::Empty));
    assert_eq!(Key::parse("ctrl+"), Err(KeyError::NoKey));
    assert_eq!(
        Key::parse("hyper+f"),
        Err(KeyError::UnknownModifier("hyper".into()))
    );
    assert!(Key::parse("ctrl+alt+").is_err());
}

#[test]
fn gnome_spelling_round_trips() {
    for (ours, gnome) in [
        ("super+f", "<Super>f"),
        ("ctrl+\"", "<Control>quotedbl"),
        ("super+period", "<Super>period"),
        ("super+.", "<Super>period"),
        ("super+shift+F2", "<Super><Shift>F2"),
        ("ctrl+alt+space", "<Control><Alt>space"),
        ("super+return", "<Super>Return"),
        ("alt+pageup", "<Alt>Page_Up"),
    ] {
        let k = key(ours);
        assert_eq!(k.gnome(), gnome, "{ours}");
        let back = Key::from_gnome(gnome).unwrap_or_else(|| panic!("{gnome}"));
        assert_eq!(back.gnome(), gnome, "{gnome} does not survive a round trip");
    }
    // What GNOME itself writes for a Ctrl key, and what a settings panel writes.
    assert_eq!(
        Key::from_gnome("<Primary><Shift>e").map(|k| k.to_string()),
        Some("ctrl+shift+e".to_owned())
    );
    assert_eq!(Key::from_gnome(""), None);
    assert_eq!(Key::from_gnome("<Super>"), None);
    assert_eq!(Key::from_gnome("<Wat>f"), None);
}

#[test]
fn kde_spelling_round_trips() {
    for (ours, kde) in [
        ("super+f", "Meta+F"),
        ("ctrl+alt+s", "Ctrl+Alt+S"),
        ("super+shift+F2", "Meta+Shift+F2"),
        ("super+space", "Meta+Space"),
        ("ctrl+\"", "Ctrl+\""),
        ("ctrl++", "Ctrl++"),
        ("alt+escape", "Alt+Esc"),
    ] {
        let k = key(ours);
        assert_eq!(k.kde(), kde, "{ours}");
        let back = Key::from_kde(kde).unwrap_or_else(|| panic!("{kde}"));
        assert_eq!(back.kde(), kde, "{kde} does not survive a round trip");
    }
    assert_eq!(Key::from_kde("Hyper+F"), None);
    assert_eq!(Key::from_kde(""), None);
}

#[test]
fn a_sandbox_and_an_unknown_desktop_can_only_name_the_command() {
    let sandbox = Hotkey::new(Tools {
        sandboxed: true,
        gsettings: Some("/usr/bin/gsettings".into()),
        gnome_keys: true,
        ..Tools::default()
    });
    assert_eq!(sandbox.desktop(), Desktop::Flatpak);
    assert!(!sandbox.can_bind());
    assert_eq!(
        sandbox.status(),
        Ok(Status::CannotBind {
            command: "flatpak run com.revisetouch.Scour".into()
        })
    );
    assert_eq!(sandbox.bind(&key("super+f")), Err(Error::Sandboxed));
    assert_eq!(sandbox.clear(), Err(Error::Sandboxed));

    let other = Hotkey::new(Tools {
        desktop_var: "XFCE".into(),
        ..Tools::default()
    });
    assert_eq!(other.desktop(), Desktop::Other);
    assert_eq!(
        other.status(),
        Ok(Status::CannotBind {
            command: "scour-gui".into()
        })
    );
    assert_eq!(other.bind(&key("super+f")), Err(Error::Unsupported));
}

#[test]
fn the_command_is_the_window_beside_this_executable_when_there_is_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let gui = dir.path().join("scour-gui");
    std::fs::write(&gui, "").expect("write");
    let tools = Tools {
        exe: Some(dir.path().join("scour-tui")),
        ..Tools::default()
    };
    assert_eq!(tools.command(), gui.display().to_string());
    let alone = Tools {
        exe: Some(dir.path().join("elsewhere").join("scour-tui")),
        ..Tools::default()
    };
    assert_eq!(alone.command(), "scour-gui");
}

/// Writing a script and starting one are one race: a fork on another test's
/// thread inherits the file still open for writing, and the exec then says
/// `Text file busy`. The tests that start a process take turns.
#[cfg(unix)]
fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// A `gsettings` that keeps its settings in a file beside itself.
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

#[cfg(unix)]
#[test]
fn gnome_bind_read_rebind_clear() {
    let _turn = one_at_a_time();
    let dir = tempfile::tempdir().expect("tempdir");
    let gsettings = stub_gsettings(dir.path());
    let hk = Hotkey::new(Tools {
        gsettings: Some(gsettings.clone()),
        gnome_keys: true,
        desktop_var: "GNOME".into(),
        ..Tools::default()
    });
    assert_eq!(hk.desktop(), Desktop::Gnome);
    assert!(hk.can_bind());
    assert_eq!(hk.status(), Ok(Status::Unbound));

    assert_eq!(hk.bind(&key("super+f")), Ok(Applies::Now));
    assert_eq!(hk.status(), Ok(Status::Bound(key("super+f"))));
    let db = std::fs::read_to_string(dir.path().join("db")).expect("db");
    assert!(db.contains("binding=<Super>f"), "{db}");
    assert!(db.contains("name=Scour"), "{db}");
    assert!(db.contains("command=scour-gui"), "{db}");
    assert!(
        db.contains("custom-keybindings=['/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/scour/']"),
        "{db}"
    );

    // A second binding replaces the first and does not list the entry twice.
    assert_eq!(hk.bind(&key("ctrl+alt+s")), Ok(Applies::Now));
    assert_eq!(hk.status(), Ok(Status::Bound(key("ctrl+alt+s"))));
    let db = std::fs::read_to_string(dir.path().join("db")).expect("db");
    let list = db
        .lines()
        .find(|l| l.contains(" custom-keybindings="))
        .unwrap_or_default();
    assert_eq!(list.matches("custom-keybindings/scour/").count(), 1, "{db}");

    assert_eq!(hk.clear(), Ok(()));
    assert_eq!(hk.status(), Ok(Status::Unbound));
    let db = std::fs::read_to_string(dir.path().join("db")).expect("db");
    assert!(db.contains("custom-keybindings=@as []"), "{db}");
    assert!(!db.contains("binding="), "{db}");
}

#[cfg(unix)]
#[test]
fn gnome_leaves_other_peoples_bindings_alone() {
    let _turn = one_at_a_time();
    let dir = tempfile::tempdir().expect("tempdir");
    let gsettings = stub_gsettings(dir.path());
    std::fs::write(
        dir.path().join("db"),
        "org.gnome.settings-daemon.plugins.media-keys custom-keybindings=['/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/emoji/']\n",
    )
    .expect("seed");
    let hk = Hotkey::new(Tools {
        gsettings: Some(gsettings),
        gnome_keys: true,
        ..Tools::default()
    });
    assert_eq!(hk.status(), Ok(Status::Unbound));
    hk.bind(&key("super+f")).expect("bind");
    let db = std::fs::read_to_string(dir.path().join("db")).expect("db");
    assert!(
        db.contains("custom-keybindings=['/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/emoji/', '/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/scour/']"),
        "{db}"
    );
    hk.clear().expect("clear");
    let db = std::fs::read_to_string(dir.path().join("db")).expect("db");
    assert!(db.contains("custom-keybindings/emoji/']"), "{db}");
    assert!(!db.contains("scour/"), "{db}");
}

#[cfg(unix)]
#[test]
fn kde_writes_the_launch_key_of_the_desktop_entry() {
    let _turn = one_at_a_time();
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let write = dir.path().join("kwriteconfig6");
    let read = dir.path().join("kreadconfig6");
    std::fs::write(
        &write,
        r#"#!/bin/bash
db="$(dirname "$0")/kdb"; touch "$db"
# --file F --group services --group scour.desktop --key _launch VALUE|--delete
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
    let hk = Hotkey::new(Tools {
        kwriteconfig: Some(write),
        kreadconfig: Some(read),
        desktop_var: "KDE".into(),
        ..Tools::default()
    });
    assert_eq!(hk.desktop(), Desktop::Kde);
    assert_eq!(hk.status(), Ok(Status::Unbound));
    assert_eq!(hk.bind(&key("super+f")), Ok(Applies::AfterLogin));
    let db = std::fs::read_to_string(dir.path().join("kdb")).expect("kdb");
    assert_eq!(
        db.trim(),
        "kglobalshortcutsrc|services|scour.desktop|_launch=Meta+F"
    );
    assert_eq!(hk.status(), Ok(Status::Bound(key("super+f"))));
    hk.clear().expect("clear");
    assert_eq!(hk.status(), Ok(Status::Unbound));
}

#[test]
fn kde_is_only_kde_when_the_session_says_so() {
    // KDE's tools on a GNOME session: the session wins.
    let tools = Tools {
        gsettings: Some("/usr/bin/gsettings".into()),
        gnome_keys: true,
        kwriteconfig: Some("/usr/bin/kwriteconfig6".into()),
        kreadconfig: Some("/usr/bin/kreadconfig6".into()),
        desktop_var: "GNOME".into(),
        ..Tools::default()
    };
    assert_eq!(tools.desktop(), Desktop::Gnome);
    let tools = Tools {
        desktop_var: "KDE".into(),
        ..tools
    };
    assert_eq!(tools.desktop(), Desktop::Kde);
}
