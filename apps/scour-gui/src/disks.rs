//! The disks panel, and the question about a disk that came: what the window
//! draws from a `volumes` answer, in the words `scour_ui::disks` gives every face.

use scour_i18n::Catalogue;
use scour_proto::DiskState;
use slint::{ModelRc, VecModel};

use crate::{Disk, FolderRow, MainWindow, compact_bytes, grouped, t};

/// The panel's rows and the question, from the service's list. Returns the
/// disk the question is about, which an answer names; empty when none waits.
pub fn draw(w: &MainWindow, cat: &Catalogue, disks: &[scour_proto::Disk]) -> String {
    let rows: Vec<Disk> = disks.iter().map(|d| row(cat, d)).collect();
    w.set_disks(ModelRc::new(VecModel::from(rows)));
    let asking = disks.iter().find(|d| d.state == DiskState::Asking);
    let question = asking
        .map(|d| {
            t(cat, scour_ui::disks::question(d.volume.kind.as_str()))
                .replace("{name}", d.volume.name())
        })
        .unwrap_or_default();
    w.set_disk_question(question.into());
    asking.map(|d| d.volume.id.clone()).unwrap_or_default()
}

fn row(cat: &Catalogue, d: &scour_proto::Disk) -> Disk {
    let v = &d.volume;
    let mut aside = t(cat, scour_ui::disks::kind(v.kind.as_str())).to_string();
    if v.size > 0 {
        aside.push_str(" · ");
        aside.push_str(&compact_bytes(v.size));
    }
    aside.push_str(" · ");
    aside.push_str(&t(cat, scour_ui::disks::state(d.state.as_str())));
    let s = &d.seen;
    Disk {
        id: v.id.as_str().into(),
        name: v.name().into(),
        aside: aside.into(),
        roots: v.roots.join("  ").into(),
        on: d.state.indexed(),
        fixed: d.state == DiskState::Configured,
        status: if d.state.indexed() {
            keeping(cat, s.walked.as_ref(), s.live, s.changed, s.walking).into()
        } else {
            Default::default()
        },
        walking: s.walking,
        root: v.roots.first().cloned().unwrap_or_default().into(),
    }
}

/// How a disk or a folder is kept up to date, in one line: how many files,
/// watched live or when it was last walked, and when a change last reached
/// the index. A date beside something watched live would only say when the
/// service started.
pub fn keeping(
    cat: &Catalogue,
    walked: Option<&scour_core::Walked>,
    live: bool,
    changed: Option<i64>,
    walking: bool,
) -> String {
    if walking {
        return t(cat, "Being walked…").to_string();
    }
    let mut parts: Vec<String> = Vec::new();
    if let Some(wk) = walked {
        parts.push(t(cat, "{n} files").replace("{n}", &grouped(wk.entries)));
    }
    if live {
        parts.push(t(cat, "watched live").to_string());
    } else if let Some(wk) = walked {
        parts.push(t(cat, "walked {when}").replace("{when}", &scour_ui::format::stamp(wk.at)));
    }
    if let Some(at) = changed {
        parts.push(t(cat, "last change {when}").replace("{when}", &scour_ui::format::stamp(at)));
    }
    if parts.is_empty() {
        return t(cat, "Not walked yet").to_string();
    }
    parts.join(" · ")
}

/// The "Folders scanned" rows: each folder's switches and how its last walk
/// went, so a share the service may not read says so instead of staying empty.
pub fn draw_folders(w: &MainWindow, cat: &Catalogue, folders: &[scour_proto::FolderState]) {
    let rows: Vec<FolderRow> = folders
        .iter()
        .map(|f| {
            let (status, problem) = if !f.on {
                (t(cat, "Switched off: not in the index").to_string(), false)
            } else if f.held {
                (
                    t(cat, "Inside a disk that is scanned already").to_string(),
                    false,
                )
            } else {
                match &f.walked {
                    Some(wk) if !wk.problem.is_empty() && !f.walking => (
                        t(cat, "Could not be read: {why}").replace("{why}", &wk.problem),
                        true,
                    ),
                    walked => (
                        keeping(cat, walked.as_ref(), f.live, f.changed, f.walking),
                        false,
                    ),
                }
            };
            FolderRow {
                path: f.path.as_str().into(),
                on: f.on,
                watched: f.watched,
                status: status.into(),
                problem,
                walking: f.walking,
                walkable: f.on && !f.held,
            }
        })
        .collect();
    w.set_folder_rows(ModelRc::new(VecModel::from(rows)));
}

/// The panel's fixed words, set with the rest when the language is.
pub fn words(w: &MainWindow, cat: &Catalogue) {
    w.set_disks_title(t(cat, "Disks"));
    w.set_disks_note(t(
        cat,
        "Every disk on this machine. A built-in one is indexed unless switched off; \
         one plugged in or on the network is asked about first.",
    ));
    w.set_disks_after(t(
        cat,
        "Switching a disk on walks it; switching one off takes its files out of the index.",
    ));
    w.set_disks_none(t(cat, "No disks were found."));
    w.set_disk_yes(t(cat, "Index it"));
    w.set_disk_later(t(cat, "Not now"));
    w.set_disk_never(t(cat, "Never"));
}
