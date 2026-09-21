//! The report tab: what a folder weighs, drawn.
//!
//! Every share and every segment width comes from `scour-chart`, so all four
//! faces cut the same bar the same way; the decisions with a right answer are
//! functions with tests here, not bindings in the `.slint`.

use scour_chart::{Rest, Segment, arcs, fold, segments, shares};
use scour_core::UsageResponse;
use scour_i18n::Catalogue;
use slint::{ModelRc, VecModel};

use crate::{Big, Facet, Kid, MainWindow, Seg, Slice, compact_bytes, grouped, t};

/// The six age bands, in the order the colours run.
pub const BANDS: [&str; 6] = [
    "today",
    "this week",
    "this month",
    "six months",
    "this year",
    "older",
];

/// How wide a segment has to be, in pixels, before a word set inside it can be
/// read rather than merely clipped.
const LABEL_FLOOR: f32 = 64.0;

/// The ring's own box, which the `Path` is given one to one; the circle through
/// the middle of the band, and how thick the band is. The ink runs from 10 to
/// 140 and stays inside the box.
const RING: f64 = 150.0;
const RADIUS: f64 = 54.0;
const THICK: f64 = 22.0;

/// How many kinds get a slice of their own before the others are folded into
/// one. Six, because that is how many steps the ramp has.
const KINDS: usize = 6;

/// The tone past the end of the ramp: what was folded away.
const REST_TONE: i32 = 6;

/// How many of the heaviest files are named.
const BIGGEST: usize = 6;

/// How many characters a file's folder may take before it is cut at a slash.
const DIR_ROOM: usize = 30;

/// What the scope last weighed, kept because the strips are relabelled when the
/// window changes width and weighing a folder is a walk.
#[derive(Default)]
pub struct Kept {
    pub usage: Option<UsageResponse>,
    pub path: String,
}

// --- the decisions ---------------------------------------------------------

/// Which step of the ramp a segment takes: the six in order, then the last one
/// over and over. What was folded away is not on the ramp — see [`REST_TONE`].
pub fn tone(i: usize) -> i32 {
    i.min(5) as i32
}

/// Which segments of a `strip` pixels wide are wide enough to carry a word.
/// Decided here rather than in the `.slint`: a text that measures itself inside
/// a segment whose width comes from the strip is a binding loop.
pub fn wide_enough(cuts: &[Segment], strip: f32, floor: f32) -> Vec<bool> {
    cuts.iter()
        .map(|s| strip * s.width as f32 >= floor)
        .collect()
}

/// What is left once the heaviest children have a row each: the other folders,
/// and the files sitting directly in this one. `None` when nothing is left, and
/// `None` when filtered — two filtered answers do not subtract into "the rest".
pub fn rest_of(root: u64, shown: &[u64], child_count: u32, filtered: bool) -> Option<Rest> {
    let value = root.saturating_sub(shown.iter().sum());
    let count = (child_count as usize).saturating_sub(shown.len());
    if filtered || (value == 0 && count == 0) {
        return None;
    }
    Some(Rest { count, value })
}

/// One slice of the ring as SVG path data, in the box the `Path` is given: the
/// outer arc clockwise from twelve o'clock, across the band, and the inner arc
/// back. **Filled, not stroked** — a stroked arc sends the software renderer's
/// stroker into a loop it does not come out of (zeno 0.3.3, `add_split_join`).
pub fn arc_commands(start: f64, sweep: f64) -> String {
    let outer = RADIUS + THICK / 2.0;
    let inner = RADIUS - THICK / 2.0;
    let at = |deg: f64, r: f64| {
        let a = deg.to_radians();
        (RING / 2.0 + r * a.sin(), RING / 2.0 - r * a.cos())
    };
    if sweep <= 0.0 {
        return String::new();
    }
    // A whole turn has no two ends to meet at: two half turns out and two back,
    // the way round deciding which side of the band is the hole.
    if sweep >= 360.0 {
        let (ox, oy) = at(start, outer);
        let (ox2, oy2) = at(start + 180.0, outer);
        let (ix, iy) = at(start, inner);
        let (ix2, iy2) = at(start + 180.0, inner);
        return format!(
            "M {ox:.3} {oy:.3} A {outer} {outer} 0 0 1 {ox2:.3} {oy2:.3} \
             A {outer} {outer} 0 0 1 {ox:.3} {oy:.3} Z \
             M {ix:.3} {iy:.3} A {inner} {inner} 0 0 0 {ix2:.3} {iy2:.3} \
             A {inner} {inner} 0 0 0 {ix:.3} {iy:.3} Z"
        );
    }
    let large = i32::from(sweep > 180.0);
    let (ax, ay) = at(start, outer);
    let (bx, by) = at(start + sweep, outer);
    let (cx, cy) = at(start + sweep, inner);
    let (dx, dy) = at(start, inner);
    format!(
        "M {ax:.3} {ay:.3} A {outer} {outer} 0 {large} 1 {bx:.3} {by:.3} \
         L {cx:.3} {cy:.3} A {inner} {inner} 0 {large} 0 {dx:.3} {dy:.3} Z"
    )
}

/// Where a file sits, said as briefly as it can be: under the scope rather than
/// from the root, and cut at a slash with an ellipsis past `room` characters.
pub fn under_scope(path: &str, scope: &str, room: usize) -> String {
    let dir = scour_ui::path::folder(path);
    // Nothing is the scope when everything is indexed, and then the path is
    // absolute and stays so.
    let under = match scope.is_empty() {
        true => dir,
        false => dir
            .strip_prefix(scope)
            .map_or(dir, |rest| rest.trim_start_matches('/')),
    };
    if under.chars().count() <= room {
        return under.to_string();
    }
    let mut kept = String::new();
    for part in under.split('/') {
        if kept.chars().count() + part.chars().count() + 1 > room {
            break;
        }
        if !kept.is_empty() {
            kept.push('/');
        }
        kept.push_str(part);
    }
    // Not one component fits: cut mid-name rather than say nothing at all.
    if kept.is_empty() {
        kept = under.chars().take(room).collect();
    }
    format!("{kept}/…")
}

// --- what the window is told ------------------------------------------------

/// The scope, as a run of buttons: everything, then each ancestor.
fn crumb_of(cat: &Catalogue, path: &str) -> Vec<Facet> {
    scour_ui::path::steps(path, &t(cat, "Everything"))
        .into_iter()
        .map(|(label, walked)| Facet {
            label: label.as_str().into(),
            token: walked.as_str().into(),
            count: slint::SharedString::new(),
            share: 0.0,
        })
        .collect()
}

/// The report's fixed words, set whenever the scope changes or the language does.
pub fn words(w: &MainWindow, cat: &Catalogue, path: &str) {
    w.set_head_folder(t(cat, "Folder"));
    w.set_head_age(t(cat, "By age"));
    w.set_head_share(t(cat, "Share"));
    w.set_head_files(t(cat, "Files"));
    w.set_head_bytes(t(cat, "Where the bytes are"));
    w.set_kids_empty(t(
        cat,
        "There are no further folders to show under this one.",
    ));
    w.set_kids_note(t(
        cat,
        "The bar behind a row is the folder's share, against the heaviest. The coloured strip is the age of its bytes. Clicking a row descends into it.",
    ));
    w.set_head_kinds(t(cat, "By kind"));
    w.set_kinds_word(t(cat, "kinds"));
    // Not the page's sentence: this window leaves the scope where it is.
    w.set_kinds_note(t(
        cat,
        "Clicking a slice adds a kind: term to the query and opens the search tab.",
    ));
    w.set_head_biggest(t(cat, "Largest files"));
    w.set_biggest_note(t(
        cat,
        "Right-click opens the same menu: open, open the folder, move to trash.",
    ));
    w.set_head_jump(t(cat, "Search in this folder"));
    w.set_head_dupes(t(cat, "Duplicate files"));
    w.set_report_under(if path.is_empty() {
        t(cat, "everything")
    } else {
        path.into()
    });
    w.set_jump_label(t(cat, "Search in this scope"));
    w.set_jump_note(t(
        cat,
        "From the report into the search: an under: term is added to the query and the search tab opens with the same scope.",
    ));
}

/// Draw a weighed folder: what it comes to, how old its bytes are, and where
/// the weight sits.
pub fn draw_usage(w: &MainWindow, cat: &Catalogue, path: &str, u: &UsageResponse) {
    w.set_crumb(ModelRc::new(VecModel::from(crumb_of(cat, path))));
    w.set_report_total(compact_bytes(u.root.bytes).into());
    w.set_report_files(
        t(cat, "{files} files · {disk} on disk · {n} folders")
            .replace("{files}", &grouped(u.root.files))
            .replace("{disk}", &compact_bytes(u.root.disk))
            .replace("{n}", &grouped(u.child_count as u64))
            .into(),
    );
    w.set_report_took(format!("{:.1} ms", u.took_us as f64 / 1000.0).into());
    age_strip(w, cat, &u.root.age);
    kid_rows(w, cat, u);
}

/// The scope's bytes by age: the bar, and the six words under it.
fn age_strip(w: &MainWindow, cat: &Catalogue, age: &[u64]) {
    let cuts = segments(age);
    let fits = wide_enough(&cuts, w.get_strip_width(), LABEL_FLOOR);
    let pct = shares(age, 0);
    let segs: Vec<Seg> = cuts
        .iter()
        .enumerate()
        .map(|(i, s)| Seg {
            start: s.start as f32,
            width: s.width as f32,
            tone: tone(i),
            label: if fits[i] {
                format!("{} {:.0}%", t(cat, BANDS[i]), pct[i]).into()
            } else {
                slint::SharedString::new()
            },
        })
        .collect();
    w.set_age_segs(ModelRc::new(VecModel::from(segs)));
    let key: Vec<Facet> = BANDS
        .iter()
        .enumerate()
        .map(|(i, band)| Facet {
            label: t(cat, band),
            count: format!("{:.0}%", pct[i]).into(),
            token: slint::SharedString::new(),
            share: 0.0,
        })
        .collect();
    w.set_age_key(ModelRc::new(VecModel::from(key)));
}

/// Where the bytes are: the folder bar, a row per folder, and the rest last.
fn kid_rows(w: &MainWindow, cat: &Catalogue, u: &UsageResponse) {
    let shown: Vec<u64> = u.children.iter().map(|c| c.bytes).collect();
    // The window weighs without a query, so what is left over is comparable.
    let rest = rest_of(u.root.bytes, &shown, u.child_count, false);
    let mut values = shown.clone();
    if let Some(r) = rest {
        values.push(r.value);
    }
    let pct = shares(&values, 1);
    folder_strip(w, u, &values, &pct);
    w.set_kids_aside(if u.child_count as usize > u.children.len() {
        t(cat, "the heaviest {shown} of {total} folders")
            .replace("{shown}", &grouped(u.children.len() as u64))
            .replace("{total}", &grouped(u.child_count as u64))
            .into()
    } else {
        slint::SharedString::new()
    });
    w.set_kids(ModelRc::new(VecModel::from(folder_rows(
        cat, u, rest, &pct,
    ))));
}

/// The shown children and the folded rest as one bar, named where there is room.
fn folder_strip(w: &MainWindow, u: &UsageResponse, values: &[u64], pct: &[f64]) {
    let cuts = segments(values);
    let fits = wide_enough(&cuts, w.get_strip_width(), LABEL_FLOOR);
    let named = u.children.len();
    let segs: Vec<Seg> = cuts
        .iter()
        .enumerate()
        .map(|(i, s)| Seg {
            start: s.start as f32,
            width: s.width as f32,
            tone: if i < named { tone(i) } else { REST_TONE },
            // The folded rest carries no word: it is not one folder.
            label: if fits[i] && i < named {
                format!(
                    "{} {:.0}%",
                    scour_ui::path::leaf(&u.children[i].path),
                    pct[i]
                )
                .into()
            } else {
                slint::SharedString::new()
            },
        })
        .collect();
    w.set_kid_segs(ModelRc::new(VecModel::from(segs)));
}

/// One row a folder, heaviest first, and one muted row for everything else.
fn folder_rows(cat: &Catalogue, u: &UsageResponse, rest: Option<Rest>, pct: &[f64]) -> Vec<Kid> {
    let most = u.children.iter().map(|c| c.bytes).max().unwrap_or(1).max(1);
    let mut kids: Vec<Kid> = u
        .children
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let band = |at: usize| {
                if c.bytes == 0 {
                    0.0
                } else {
                    c.age[at] as f32 / c.bytes as f32
                }
            };
            Kid {
                name: scour_ui::path::leaf(&c.path).into(),
                path: c.path.as_str().into(),
                size: compact_bytes(c.bytes).into(),
                share: format!("{:.1}%", pct[i]).into(),
                files: grouped(c.files).into(),
                bar: c.bytes as f32 / most as f32,
                rest: false,
                a0: band(0),
                a1: band(1),
                a2: band(2),
                a3: band(3),
                a4: band(4),
                a5: band(5),
            }
        })
        .collect();
    if let Some(r) = rest {
        kids.push(Kid {
            name: if r.count == 0 {
                t(cat, "the files here")
            } else {
                t(cat, "the other {n} folders and the files here")
                    .replace("{n}", &grouped(r.count as u64))
                    .into()
            },
            path: slint::SharedString::new(),
            size: compact_bytes(r.value).into(),
            share: format!("{:.1}%", pct[u.children.len()]).into(),
            files: slint::SharedString::new(),
            bar: 0.0,
            rest: true,
            a0: 0.0,
            a1: 0.0,
            a2: 0.0,
            a3: 0.0,
            a4: 0.0,
            a5: 0.0,
        });
    }
    kids
}

/// The ring: the six heaviest kinds and the rest, as arcs and as a legend.
pub fn draw_kinds(
    w: &MainWindow,
    cat: &Catalogue,
    counted: &[scour_core::Facet],
    total: u64,
    capped: bool,
) {
    // The taxonomy's order, so the legend does not reshuffle; `fold` then takes
    // the largest six, and its sort is stable.
    let found: Vec<(slint::SharedString, &'static str, u64)> = crate::rows::offered_kinds()
        .iter()
        .filter_map(|k| {
            let token = k.token();
            counted
                .iter()
                .find(|x| x.key == token)
                .map(|hit| (t(cat, k.msgid()), token, hit.count))
        })
        .collect();
    w.set_kinds_count(grouped(found.len() as u64).into());

    let folded = fold(found, |x| x.2, KINDS);
    let mut values: Vec<u64> = folded.kept.iter().map(|x| x.2).collect();
    if let Some(r) = folded.rest {
        values.push(r.value);
    }
    let pct = shares(&values, 1);
    let ring = arcs(&values);
    let mut slices: Vec<Slice> = folded
        .kept
        .iter()
        .enumerate()
        .map(|(i, (label, token, count))| Slice {
            commands: arc_commands(ring[i].start, ring[i].sweep).into(),
            tone: tone(i),
            label: label.clone(),
            count: grouped(*count).into(),
            percent: format!("{:.1}%", pct[i]).into(),
            token: format!("kind:{token}").into(),
        })
        .collect();
    if let Some(r) = folded.rest {
        let last = slices.len();
        slices.push(Slice {
            commands: arc_commands(ring[last].start, ring[last].sweep).into(),
            tone: REST_TONE,
            label: t(cat, "the other {n} kinds")
                .replace("{n}", &grouped(r.count as u64))
                .into(),
            count: grouped(r.value).into(),
            percent: format!("{:.1}%", pct[last]).into(),
            // Not one term, so not one press.
            token: slint::SharedString::new(),
        });
    }
    w.set_report_kinds(ModelRc::new(VecModel::from(slices)));
    w.set_kinds_sampled(if capped {
        t(cat, "{n} sampled rows")
            .replace("{n}", &grouped(total))
            .into()
    } else {
        slint::SharedString::new()
    });
}

/// The heaviest files under the scope, each against the largest of them.
pub fn draw_biggest(w: &MainWindow, scope: &str, hits: &[scour_core::Hit]) {
    let bytes = |h: &scour_core::Hit| h.meta.size.max(0) as u64;
    let most = hits.iter().map(bytes).max().unwrap_or(1).max(1);
    let rows: Vec<Big> = hits
        .iter()
        .take(BIGGEST)
        .map(|h| Big {
            name: h.name().into(),
            dir: under_scope(&h.path, scope, DIR_ROOM).into(),
            size: compact_bytes(bytes(h)).into(),
            share: bytes(h) as f32 / most as f32,
            path: h.path.as_str().into(),
        })
        .collect();
    w.set_report_big(ModelRc::new(VecModel::from(rows)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ramp_runs_out_rather_than_wrapping_round_to_the_first_step() {
        assert_eq!(tone(0), 0);
        assert_eq!(tone(5), 5);
        assert_eq!(tone(6), 5, "a seventh folder is not the first one again");
        assert_eq!(tone(40), 5);
        assert_ne!(REST_TONE, tone(6), "the rest is off the ramp");
    }

    /// The live home, at the mock-up's own width: three of the six age bands
    /// are wide enough for a word and three are not.
    #[test]
    fn only_a_segment_wide_enough_to_read_carries_a_word() {
        let age = [143u64, 34, 648, 154, 5, 17];
        let fits = wide_enough(&segments(&age), 860.0, LABEL_FLOOR);
        assert_eq!(fits, vec![true, false, true, true, false, false]);
        // The same shares in a narrow window: only the one that dominates.
        let fits = wide_enough(&segments(&age), 300.0, LABEL_FLOOR);
        assert_eq!(fits, vec![false, false, true, false, false, false]);
        // Before the first layout the strip has no width at all.
        assert!(
            wide_enough(&segments(&age), 0.0, LABEL_FLOOR)
                .iter()
                .all(|f| !f)
        );
        assert!(wide_enough(&[], 860.0, LABEL_FLOOR).is_empty());
    }

    #[test]
    fn the_rest_is_what_the_shown_folders_leave_and_is_dropped_when_filtered() {
        // Twelve of sixty-three shown, the rest holding fifty-one folders.
        let shown = vec![600u64, 30, 14];
        let rest = rest_of(700, &shown, 63, false).expect("something is left");
        assert_eq!(rest.count, 60);
        assert_eq!(rest.value, 56);
        // Every folder shown, but files sit directly in the scope.
        assert_eq!(
            rest_of(700, &shown, 3, false),
            Some(Rest {
                count: 0,
                value: 56
            })
        );
        // Every folder shown and nothing loose: no row.
        assert_eq!(rest_of(644, &shown, 3, false), None);
        // A filtered weighing: the difference is not "the rest".
        assert_eq!(rest_of(700, &shown, 63, true), None);
        // Children can outweigh a filtered root; it must not wrap round.
        assert_eq!(rest_of(1, &shown, 3, false), None);
    }

    #[test]
    fn a_slice_is_a_band_that_starts_at_twelve_and_runs_clockwise() {
        // A quarter turn: out to the top of the band, round to its right-hand
        // side, across the band and back.
        assert_eq!(
            arc_commands(0.0, 90.0),
            "M 75.000 10.000 A 65 65 0 0 1 140.000 75.000 \
             L 118.000 75.000 A 43 43 0 0 0 75.000 32.000 Z"
        );
        // Past a half turn the large-arc flag has to be set, or the renderer
        // draws the short way round.
        assert!(
            arc_commands(0.0, 270.0).contains("A 65 65 0 1 1"),
            "{}",
            arc_commands(0.0, 270.0)
        );
        assert!(arc_commands(90.0, 90.0).starts_with("M 140.000 75.000"));
        // A single kind fills the ring: two subpaths, the inner one the hole,
        // since an arc between one point and itself draws nothing.
        let whole = arc_commands(0.0, 360.0);
        assert_eq!(whole.matches('M').count(), 2, "{whole}");
        assert_eq!(whole.matches(" A ").count(), 4, "{whole}");
        assert!(
            whole.contains("A 43 43 0 0 0"),
            "the hole winds back: {whole}"
        );
        assert_eq!(arc_commands(0.0, 0.0), "", "an empty slice draws nothing");
    }

    #[test]
    fn a_folder_is_named_under_the_scope_and_cut_at_a_slash() {
        assert_eq!(
            under_scope("/home/hasan/vm/cuce/disk.qcow2", "/home/hasan", DIR_ROOM),
            "vm/cuce"
        );
        assert_eq!(
            under_scope(
                "/home/hasan/.config/Claude/vm_bundles/f3c91a44/rootfs.img",
                "/home/hasan",
                DIR_ROOM
            ),
            ".config/Claude/vm_bundles/…",
            "cut at a slash, never mid-name, when there is a slash to cut at"
        );
        // Everything indexed: the scope is empty and the path stays absolute.
        assert_eq!(under_scope("/etc/hosts", "", DIR_ROOM), "/etc");
        // One component longer than the whole budget is cut mid-name.
        let long = "/a".to_string() + &"b".repeat(60) + "/x";
        assert!(under_scope(&long, "", DIR_ROOM).ends_with("/…"));
        assert_eq!(under_scope("/x.txt", "", DIR_ROOM), "/");
    }
}
