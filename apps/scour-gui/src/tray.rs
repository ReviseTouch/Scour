//! `scour-gui --drag PATH…`: a small window of its own to drag files from,
//! for the faces that cannot start a drag. Chromium hands a page's `file://`
//! on as `about:blank#`, and a terminal's window is the terminal's — so the
//! page and the terminal open this, and the drag is this window's.
//!
//! It goes once a target has taken the files; a drag let go where nothing
//! takes them leaves it open for another try.

use std::path::Path;

use anyhow::{Context, Result};
use scour_core::Catalog;
use scour_i18n::Catalogue;
use slint::ComponentHandle;

use crate::{DragTray, Fonts, Theme, drag};

pub fn run(asked: &[String], config: &scour_config::Config) -> Result<()> {
    // Only what is there: the list is a moment old by the time it gets here.
    let paths: Vec<String> = asked
        .iter()
        .filter(|p| Path::new(p).symlink_metadata().is_ok())
        .cloned()
        .collect();
    if paths.is_empty() {
        anyhow::bail!("nothing to drag: none of those paths exists");
    }
    let kept = scour_settings::Settings::load(&config.state_dir());
    let language = crate::language(&kept, config);
    let cat = Catalogue::for_language(&language);
    let decimal = scour_ui::format::decimal_mark(&language);

    let tray = DragTray::new().context("the drag window could not be created")?;
    if let Err(e) = slint::set_xdg_app_id("scour") {
        eprintln!("scour-gui: the window's app id could not be set: {e}");
    }
    let theme = tray.global::<Theme>();
    theme.set_dark_scheme(crate::scheme(&scour_ui::DARK));
    theme.set_light_scheme(crate::scheme(&scour_ui::LIGHT));
    tray.global::<Fonts>()
        .set_mono(crate::mono_family().as_str().into());

    let weight: u64 = paths
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum();
    if let [one] = paths.as_slice() {
        let is_dir = Path::new(one).is_dir();
        let name = scour_ui::path::leaf(one);
        let kind = scour_core::kind_of(is_dir, name, 0);
        tray.set_name(name.into());
        tray.set_ktoken(kind.token().into());
        tray.set_tint(crate::rows::tint_of(kind.token()));
        let folder = home_tilde(scour_ui::path::folder(one));
        tray.set_detail(if is_dir {
            folder.into()
        } else {
            format!("{folder} · {}", scour_ui::format::size(weight, decimal)).into()
        });
    } else {
        let n =
            scour_ui::format::grouped(paths.len() as u64, scour_ui::format::group_mark(&language));
        tray.set_name(cat.get("{n} items").replace("{n}", &n).into());
        tray.set_ktoken("".into());
        tray.set_tint(crate::rows::tint_of(""));
        tray.set_detail(scour_ui::format::size(weight, decimal).into());
    }
    tray.set_hint(
        cat.get("Drag from here into another program")
            .into_owned()
            .into(),
    );

    tray.on_close(|| {
        let _ = slint::quit_event_loop();
    });
    {
        let weak = tray.as_weak();
        tray.on_drag(move |x, y| {
            let Some(t) = weak.upgrade() else { return };
            let paths = paths.clone();
            let again = t.as_weak();
            drag::files(&t, paths, x, y, move |taken| {
                if taken {
                    let _ = slint::quit_event_loop();
                } else if let Some(t) = again.upgrade() {
                    t.set_dragging(false);
                }
            });
        });
    }
    {
        // As the main window: attached once on screen, before the first press.
        let weak = tray.as_weak();
        slint::Timer::single_shot(std::time::Duration::ZERO, move || {
            if let Some(t) = weak.upgrade() {
                drag::prepare(&t);
            }
        });
    }
    tray.run().context("the event loop failed")
}

/// `~/Belgeler` rather than `/home/hasan/Belgeler`: read in passing.
pub(crate) fn home_tilde(folder: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && folder.starts_with(&home) => {
            let rest = &folder[home.len()..];
            if rest.is_empty() || rest.starts_with('/') {
                return format!("~{rest}");
            }
            folder.to_owned()
        }
        _ => folder.to_owned(),
    }
}
