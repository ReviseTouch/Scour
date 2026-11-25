use super::*;
use crate::lens::{Lens, SIZES, bands, launch, lens_of, term_at};

fn t(q: &str) -> Term {
    term_at(q, q.len()).unwrap()
}

#[test]
fn the_presets_are_empty_then_the_lens_s_bands() {
    assert_eq!(
        SIZE_PRESETS[0],
        SizePreset {
            label: "Empty",
            term: "empty:"
        }
    );
    let bands: Vec<(String, &str)> = SIZE_PRESETS[1..]
        .iter()
        .map(|p| (p.term.replace("size:", ""), p.label))
        .collect();
    let want: Vec<(String, &str)> = SIZES.iter().map(|(v, l)| (v.to_string(), *l)).collect();
    assert_eq!(bands, want);
    assert_eq!(NOT_EMPTY, "!empty:");
}

#[test]
fn every_preset_round_trips_with_and_without_its_negation() {
    for (i, p) in SIZE_PRESETS.iter().enumerate() {
        for q in [p.term.to_string(), format!("!{}", p.term)] {
            let term = t(&q);
            assert_eq!(size_preset_of(&q, &term), Some(i), "{q}");
            assert_eq!(size_preset_term(&term, i), q);
        }
    }
    assert_eq!(size_preset_term(&t("!empty:"), 4), "!size:>1gb");
    assert_eq!(size_preset_term(&t("size:>1gb"), 0), "empty:");
}

#[test]
fn other_spellings_read_and_strangers_do_not() {
    for (q, want) in [
        ("bos:", Some(0)),
        ("EMPTY:", Some(0)),
        ("!bos:", Some(0)),
        ("size:<100KB", Some(1)),
        ("sizE:10MB..1GB", Some(3)),
        ("size:>0", None),
        ("size:0", None),
        ("size:>2gb", None),
        ("empty:x", None),
        ("rapor", None),
    ] {
        assert_eq!(size_preset_of(q, &t(q)), want, "{q}");
    }
}

#[test]
fn each_preset_is_counted_over_the_query_less_its_size_terms() {
    let c = size_counts("rapor size:>1gb dm:7d !empty:");
    assert_eq!(c[0], "rapor dm:7d empty:");
    assert_eq!(c[4], "rapor dm:7d size:>1gb");
    assert_eq!(size_counts("")[2], "size:100kb..10mb");
    assert_eq!(
        size_counts("size:<1kb|rapor ext:pdf")[1],
        "ext:pdf size:<100kb"
    );
}

/// The service reads `empty:` and `bos:`, any case; not `boş:` (a name).
#[test]
fn an_empty_term_opens_the_size_lens_and_says_empty() {
    for q in ["empty:", "bos:", "BOS:", "Empty:", "!empty:", "rapor !bos:"] {
        let term = t(q);
        assert_eq!(lens_of(q, &term), Some(Lens::Size), "{q}");
        assert_eq!(size_said(q, &term), Some("Empty"), "{q}");
    }
    assert_eq!(lens_of("boş:", &t("boş:")), None);
    assert_eq!(size_said("size:>1gb", &t("size:>1gb")), Some("over 1 GB"));
    assert_eq!(size_said("size:>0", &t("size:>0")), None);
    // An older face writing the bands of `lens::bands` on `empty:` writes size.
    assert_eq!(bands(&t("!empty:"), SIZES)[0].term, "!size:<100kb");
    let held: Vec<String> = launch("rapor empty: :", ":", |m| m.to_string())
        .into_iter()
        .filter(|s| s.held)
        .map(|s| s.choice.term)
        .collect();
    assert_eq!(held, ["size:"]);
}
