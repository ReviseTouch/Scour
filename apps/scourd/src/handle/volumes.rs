//! The disks and the added folders, each with what the engine is doing on it.

use super::*;

/// Every disk and every added folder, with its walk and its watch.
pub(super) fn volumes(engine: &Engine, live: &Live, kept: &Kept) -> Response {
    let plan = live.plan();
    let walks = engine.walks();
    let keeping = engine.keeping();
    let settings = kept
        .settings
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    let named: std::collections::HashSet<String> =
        plan.sources.iter().map(|s| s.source.name.clone()).collect();
    let folders = settings
        .folders
        .iter()
        .map(|f| {
            let name = crate::disks::folder_source(f);
            let on = !settings.folders_off.contains(f);
            let k = keeping.get(&name).copied().unwrap_or_default();
            scour_proto::FolderState {
                path: f.clone(),
                on,
                watched: !settings.folders_unwatched.contains(f),
                held: on && !named.contains(&name),
                walked: walks.get(&name).cloned(),
                live: k.live,
                changed: k.changed,
                walking: k.walking,
            }
        })
        .collect();
    // A disk's source is named by the volume's id.
    let disks = plan
        .disks
        .into_iter()
        .map(|mut d| {
            let k = keeping.get(&d.volume.id).copied().unwrap_or_default();
            d.seen = scour_proto::Seen {
                walked: walks.get(&d.volume.id).cloned(),
                live: k.live,
                changed: k.changed,
                walking: k.walking,
            };
            d
        })
        .collect();
    Response::Volumes { disks, folders }
}
