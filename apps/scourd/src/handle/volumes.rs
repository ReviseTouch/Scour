//! The disks and the added folders, each with what the engine is doing on it.

use std::collections::HashMap;
use std::path::Path;

use scour_config::SourceCfg;
use scour_core::Walked;
use scour_engine::Keeping;
use scour_proto::Seen;
use scour_volumes::Volume;

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
    let found: Vec<Volume> = plan.disks.iter().map(|d| d.volume.clone()).collect();
    let sources: Vec<&SourceCfg> = plan.sources.iter().map(|p| &p.source).collect();
    let disks = plan
        .disks
        .into_iter()
        .map(|mut d| {
            let on = on_disk(&d.volume.id, &found, &sources);
            d.seen = seen_of(&on, &walks, &keeping);
            d
        })
        .collect();
    Response::Volumes { disks, folders }
}

/// The disk a path lies on: the one whose root is its longest prefix, so
/// `/mnt/depo/x` is on `/mnt/depo` and not on `/`.
fn disk_of<'a>(path: &Path, volumes: &'a [Volume]) -> Option<&'a str> {
    volumes
        .iter()
        .flat_map(|v| v.roots.iter().map(move |r| (v, Path::new(r))))
        .filter(|(_, r)| path.starts_with(r))
        .max_by_key(|(_, r)| r.components().count())
        .map(|(v, _)| v.id.as_str())
}

/// The names of the sources with a root on the disk `id`.
fn on_disk<'a>(id: &str, volumes: &[Volume], sources: &[&'a SourceCfg]) -> Vec<&'a str> {
    sources
        .iter()
        .filter(|s| s.roots.iter().any(|r| disk_of(r, volumes) == Some(id)))
        .map(|s| s.name.as_str())
        .collect()
}

/// What a disk's sources say, together: live only if every one is, walking if
/// any is, the latest change, and a walk only once every one has been walked.
fn seen_of(
    names: &[&str],
    walks: &HashMap<String, Walked>,
    keeping: &HashMap<String, Keeping>,
) -> Seen {
    if names.is_empty() {
        return Seen::default();
    }
    let kept: Vec<Keeping> = names
        .iter()
        .map(|n| keeping.get(*n).copied().unwrap_or_default())
        .collect();
    Seen {
        walked: walked_of(names, walks),
        live: kept.iter().all(|k| k.live),
        changed: kept.iter().filter_map(|k| k.changed).max(),
        walking: kept.iter().any(|k| k.walking),
    }
}

/// The sources' walks as one: the oldest end, every entry counted once, the
/// first problem. None while any of them has not been walked.
fn walked_of(names: &[&str], walks: &HashMap<String, Walked>) -> Option<Walked> {
    let all: Vec<&Walked> = names.iter().map(|n| walks.get(*n)).collect::<Option<_>>()?;
    Some(Walked {
        at: all.iter().map(|w| w.at).min()?,
        entries: all.iter().map(|w| w.entries).sum(),
        unreadable: all.iter().map(|w| w.unreadable).sum(),
        problem: all
            .iter()
            .map(|w| w.problem.as_str())
            .find(|p| !p.is_empty())
            .unwrap_or_default()
            .to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use scour_config::SourceKindCfg;
    use scour_volumes::Kind;

    fn disk(id: &str, root: &str) -> Volume {
        Volume {
            id: id.into(),
            roots: vec![root.into()],
            label: String::new(),
            kind: Kind::Fixed,
            fs: "ext4".into(),
            size: 0,
        }
    }

    fn source(name: &str, roots: &[&str]) -> SourceCfg {
        SourceCfg {
            name: name.into(),
            kind: SourceKindCfg::Local,
            roots: roots.iter().map(Into::into).collect(),
            watch: true,
        }
    }

    /// The owner's machine: the system disk and Depo, three named sources.
    fn machine() -> (Vec<Volume>, Vec<SourceCfg>) {
        let disks = vec![disk("uuid:cachy", "/"), disk("uuid:depo", "/mnt/depo")];
        let sources = vec![
            source("home", &["/home/hasan"]),
            source("depo", &["/mnt/depo"]),
            source("sistem", &["/usr", "/etc", "/opt", "/var"]),
        ];
        (disks, sources)
    }

    fn walk(at: i64, entries: u64) -> Walked {
        Walked {
            at,
            entries,
            unreadable: 0,
            problem: String::new(),
        }
    }

    fn kept(live: bool, changed: Option<i64>, walking: bool) -> Keeping {
        Keeping {
            live,
            changed,
            walking,
        }
    }

    #[test]
    fn a_named_source_lands_on_the_disk_with_the_longest_root() {
        let (disks, sources) = machine();
        let sources: Vec<&SourceCfg> = sources.iter().collect();
        assert_eq!(on_disk("uuid:depo", &disks, &sources), vec!["depo"]);
        assert_eq!(
            on_disk("uuid:cachy", &disks, &sources),
            vec!["home", "sistem"],
            "/ holds every path, but /mnt/depo is Depo's"
        );
    }

    #[test]
    fn a_prefix_is_whole_components() {
        let disks = vec![disk("uuid:cachy", "/"), disk("uuid:depo", "/mnt/depo")];
        assert_eq!(
            disk_of(Path::new("/mnt/depot/x"), &disks),
            Some("uuid:cachy")
        );
        assert_eq!(disk_of(Path::new("/mnt/depo"), &disks), Some("uuid:depo"));
        assert_eq!(disk_of(Path::new("relative"), &disks), None);
    }

    #[test]
    fn a_disk_named_by_its_id_is_still_found() {
        let disks = vec![disk("uuid:stick", "/run/media/h/STICK")];
        let own = source("uuid:stick", &["/run/media/h/STICK"]);
        assert_eq!(on_disk("uuid:stick", &disks, &[&own]), vec!["uuid:stick"]);
    }

    #[test]
    fn a_disk_is_live_only_when_every_source_on_it_is() {
        let keeping = HashMap::from([
            ("home".to_owned(), kept(true, Some(50), false)),
            ("sistem".to_owned(), kept(false, Some(80), true)),
        ]);
        let seen = seen_of(&["home", "sistem"], &HashMap::new(), &keeping);
        assert!(!seen.live, "sistem is polled, so the disk is not all live");
        assert!(seen.walking, "one of them is being walked");
        assert_eq!(seen.changed, Some(80), "the latest change");

        let one = seen_of(&["home"], &HashMap::new(), &keeping);
        assert!(one.live && !one.walking);
        assert_eq!(one.changed, Some(50));
    }

    #[test]
    fn a_source_the_engine_does_not_report_is_not_live() {
        let keeping = HashMap::from([("home".to_owned(), kept(true, None, false))]);
        assert!(!seen_of(&["home", "gone"], &HashMap::new(), &keeping).live);
    }

    #[test]
    fn a_walk_is_the_oldest_and_counts_them_all() {
        let mut walks = HashMap::from([
            ("home".to_owned(), walk(300, 10)),
            ("sistem".to_owned(), walk(100, 5)),
        ]);
        walks.get_mut("sistem").unwrap().problem = "denied".into();
        let w = walked_of(&["home", "sistem"], &walks).expect("both walked");
        assert_eq!((w.at, w.entries), (100, 15));
        assert_eq!(w.problem, "denied");
        assert_eq!(
            walked_of(&["home", "depo"], &walks),
            None,
            "depo has not been walked in this run"
        );
    }

    #[test]
    fn a_disk_with_no_source_says_nothing() {
        let keeping = HashMap::from([("home".to_owned(), kept(true, Some(1), true))]);
        let walks = HashMap::from([("home".to_owned(), walk(1, 1))]);
        assert_eq!(seen_of(&[], &walks, &keeping), Seen::default());
    }
}
