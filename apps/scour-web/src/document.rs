//! The page as served: its parts joined at build time, the palette, the menu's
//! drawings and the disks' words put in when it is asked for.

use super::*;

/// The page, built in, so that the binary is the whole of the program.
pub(super) const PAGE: &str = include_str!(concat!(env!("OUT_DIR"), "/page.html"));

/// The application's icon, the one the desktop shows. Written into the page as
/// a data URL: every other path asks for the token, and a tab icon cannot.
pub(super) const LOGO: &str = include_str!("../../../assets/scour.svg");

pub(super) fn data_url(svg: &str) -> String {
    let mut out = String::from("data:image/svg+xml,");
    for b in svg.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~/:=;,!*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// How a disk is spoken of, by wire name: `scour_ui::disks`, so every face
/// says the same. Msgids, translated by the page.
fn disk_words() -> serde_json::Value {
    use scour_ui::disks::{KINDS, STATES, kind, question, state};
    let table = |names: &[&str], word: fn(&str) -> &'static str| -> serde_json::Value {
        names
            .iter()
            .map(|n| ((*n).to_owned(), serde_json::Value::from(word(n))))
            .collect::<serde_json::Map<_, _>>()
            .into()
    };
    serde_json::json!({
        "kinds": table(&KINDS, kind),
        "states": table(&STATES, state),
        "questions": table(&KINDS, question),
    })
}

/// The menu's drawings, by name, drawn in whatever colour the text around
/// them is: the window reads the same files from `scour-ui`.
fn menu_icons() -> serde_json::Value {
    scour_ui::menu::ICONS
        .iter()
        .chain(&["stack", "chevron"])
        .filter_map(|name| {
            let svg = scour_ui::menu::icon_svg(name)?
                .trim()
                .replace("stroke=\"#000\"", "stroke=\"currentColor\"")
                .replace(" width=\"24\" height=\"24\"", "");
            Some(((*name).to_owned(), serde_json::Value::String(svg)))
        })
        .collect::<serde_json::Map<_, _>>()
        .into()
}

/// The page with its palette and its menu written in, built once. The colours
/// live in [`scour_ui`], so this page and `theme.slint` cannot drift.
pub(super) fn page() -> &'static str {
    static PAGE_WITH_THEME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PAGE_WITH_THEME.get_or_init(|| {
        let dark = scour_ui::css_vars(&scour_ui::DARK);
        let light = scour_ui::css_vars(&scour_ui::LIGHT);
        let metrics = scour_ui::css_metrics();
        // The `[data-theme]` rules come last: the menu's override must win.
        let theme = format!(
            ":root {{\n{dark}{metrics}  }}\n\n               @media (prefers-color-scheme: light) {{\n    :root {{\n{light}    }}\n  }}\n               :root[data-theme=\"light\"] {{\n{light}  }}\n               :root[data-theme=\"dark\"] {{\n{dark}  }}\n"
        );
        // `msgid` is the catalogue key the page looks up; `id` and `key` are not.
        let menu = serde_json::Value::Array(
            scour_ui::menu::ITEMS
                .iter()
                // Left out rather than greyed for ever: a browser cannot.
                .filter(|i| !i.except.contains(&scour_ui::faces::Face::Page))
                .map(|i| {
                    serde_json::json!({
                        "id": i.id,
                        "msgid": i.msgid,
                        "key": i.key,
                        "group": i.group,
                        "icon": i.icon,
                        "short": i.short,
                        "weight": match i.weight {
                            scour_ui::menu::Weight::Plain => "plain",
                            scour_ui::menu::Weight::Careful => "careful",
                            scour_ui::menu::Weight::Heavy => "heavy",
                        },
                        "when": match i.when {
                            scour_ui::menu::When::File => "file",
                            scour_ui::menu::When::Folder => "folder",
                            scour_ui::menu::When::One => "one",
                            scour_ui::menu::When::Many => "many",
                        },
                    })
                })
                .collect(),
        );
        // The preview's buttons for each shape of selection, by menu id.
        let peek_buttons = serde_json::json!({
            "file": scour_ui::preview::buttons(false, false),
            "folder": scour_ui::preview::buttons(true, false),
            "many": scour_ui::preview::buttons(false, true),
        });
        PAGE.replacen(
            "/* @THEME@ — see `scour-ui`; the bridge writes this block when it serves\n     the page, so that the window and this page cannot drift apart. */",
            &theme,
            1,
        )
        .replacen("/* @MENU@ */ []", &menu.to_string(), 1)
        .replacen("/* @MENUICONS@ */ {}", &menu_icons().to_string(), 1)
        .replacen("/* @PEEKBUTTONS@ */ {}", &peek_buttons.to_string(), 1)
        .replacen("/* @DISKS@ */ {}", &disk_words().to_string(), 1)
        .replace("@LOGO@", &data_url(LOGO))
        .replacen("<script>", &format!("<script nonce=\"{}\">", nonce()), 1)
    })
}
