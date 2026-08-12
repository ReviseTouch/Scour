//! Turning a reply into text a model reads.
//!
//! Compact on purpose. Every token spent on formatting is a token not spent on
//! the answer, and a filesystem tool that floods a context window is a tool
//! that gets turned off. So: no box drawing, no repeated absolute prefixes
//! where a relative path will do, and totals stated once rather than implied.
//!
//! What is never trimmed is the part that says the answer is incomplete. A
//! model that cannot tell a full listing from a truncated one will draw
//! confident wrong conclusions from it.

use humansize::{BINARY, format_size};
use scour_core::{Error, Kind, Role, Span, TreeNode};
use scour_proto::Response;

pub fn human(r: &Response) -> String {
    match r {
        Response::Search(s) => {
            let mut out = String::new();
            for h in &s.hits {
                out.push_str(&format!(
                    "{}  {}  {}\n",
                    if h.is_dir {
                        "dir ".to_owned()
                    } else {
                        format!("{:>9}", format_size(h.meta.size as u64, BINARY))
                    },
                    day(h.meta.mtime),
                    h.path
                ));
            }
            let total = if s.capped {
                format!("{}+", s.total)
            } else {
                s.total.to_string()
            };
            out.push_str(&format!(
                "\n{} shown of {total} matches ({:.1} ms).",
                s.hits.len(),
                s.took_us as f64 / 1000.0
            ));
            if (s.hits.len() as u64) < s.total {
                out.push_str(" Ask for a later offset, or narrow the query.");
            }
            out
        }
        Response::Count {
            total,
            capped,
            // Warnings are appended by `warning` for every reply alike, so that
            // no arm here can forget one.
            misread: _,
        } => {
            if *capped {
                format!("At least {total} matches (counting stopped at the cap).")
            } else {
                format!("{total} matches.")
            }
        }
        Response::Duplicates {
            groups,
            candidates,
            waste,
            proven,
            read,
            unconfirmed,
        } => {
            if groups.is_empty() {
                return format!(
                    "No duplicates among the {candidates} files at or above the size floor."
                );
            }
            let mut out = String::new();
            for g in groups {
                out.push_str(&format!(
                    "{} reclaimable — {} copies of {} ({})\n",
                    format_size(g.waste, BINARY),
                    g.paths.len(),
                    format_size(g.size, BINARY),
                    // Never "identical" unless it was read and compared. The
                    // caller may be about to delete one of these.
                    match g.certainty.as_str() {
                        "content" => "read and compared, identical",
                        "edges" => "same size and same first and last 4 KB, not fully compared",
                        _ => "same size only, nothing read",
                    }
                ));
                for p in &g.paths {
                    out.push_str(&format!("  {p}\n"));
                }
            }
            out.push_str(&format!(
                "\n{} confirmed reclaimable — read and compared. {} more only shares a size \
                 with something and was NOT verified. {candidates} candidates, {} read.",
                format_size(*proven, BINARY),
                format_size(waste.saturating_sub(*proven), BINARY),
                format_size(*read, BINARY),
            ));
            if *unconfirmed > 0 {
                out.push_str(&format!(
                    " {unconfirmed} group(s) were NOT confirmed by reading — treat those as \
                     candidates, not as duplicates."
                ));
            }
            out
        }
        Response::Facets(f) => {
            if f.facets.is_empty() {
                return "Nothing matched.".into();
            }
            let mut out = String::new();
            for x in &f.facets {
                let key = if x.key.is_empty() {
                    "(no extension)"
                } else {
                    &x.key
                };
                out.push_str(&format!("{:>9}  {key}\n", x.count));
            }
            // Left as the token rather than translated, because the caller is
            // a model and the useful thing it can do with `build` is write
            // `kind:build`.
            if f.capped {
                out.push_str("(counts are a lower bound: the scan hit its cap)\n");
            }
            out
        }
        Response::Usage(u) => {
            let mut out = String::new();
            let row = |out: &mut String, d: &scour_core::DirUsage| {
                out.push_str(&format!(
                    "{:>10}  {:>10}  {:>9}  {}\n",
                    format_size(d.bytes, BINARY),
                    format_size(d.disk, BINARY),
                    d.files,
                    d.path
                ));
            };
            out.push_str("      size    on disk      files  path\n");
            row(&mut out, &u.root);
            for c in &u.children {
                row(&mut out, c);
            }
            if u.child_count as usize > u.children.len() {
                out.push_str(&format!(
                    "({} of {} child folders shown, largest first)\n",
                    u.children.len(),
                    u.child_count
                ));
            }
            // Said rather than left to be discovered: a total that folds
            // hard links while the shell's does not is how a report ends up
            // disagreeing with `du -l` for no visible reason.
            out.push_str("(a hard-linked file is counted once, as du counts it)\n");
            out
        }
        Response::Tree { root } => {
            let mut out = String::new();
            tree(root, 0, &mut out);
            out
        }
        Response::Stat(e) => {
            let mut out = format!("{}\n", e.path);
            out.push_str(&format!("  type      {}\n", kind_word(e.kind())));
            if e.is_dir {
                if e.meta.items >= 0 {
                    out.push_str(&format!("  items     {}\n", e.meta.items));
                }
            } else {
                out.push_str(&format!(
                    "  size      {}\n",
                    format_size(e.meta.size as u64, BINARY)
                ));
            }
            out.push_str(&format!("  modified  {}\n", day(e.meta.mtime)));
            out.push_str(&format!("  created   {}\n", day(e.meta.ctime)));
            let mode = scour_core::mode_string(e.meta.mode);
            if !mode.is_empty() {
                out.push_str(&format!("  mode      {mode}\n"));
            }
            out
        }
        Response::Explain {
            description,
            needs_content,
            // A model reads the sentence; the colouring is for a search box.
            ..
        } => {
            let mut out = format!("The query means: {description}");
            if *needs_content {
                out.push_str("\nIt asks about document contents, which this index may not hold.");
            }
            out
        }
        Response::Sources { sources } => {
            if sources.is_empty() {
                return "Nothing is indexed.".into();
            }
            let mut out = String::from("Indexed:\n");
            for s in sources {
                for r in &s.roots {
                    out.push_str(&format!("  {r}\n"));
                }
            }
            out.push_str("Paths outside these are not in the index and will never be found.\n");
            out
        }
        Response::Status(s) => {
            let mut out = format!(
                "{} entries, {} on disk.",
                s.entries,
                format_size(s.index_bytes, BINARY)
            );
            if s.cold {
                out.push_str(" The index is empty — nothing has been scanned yet.");
            } else if s.scanning {
                out.push_str(&format!(
                    " A scan is running ({} seen so far), so results may be incomplete.",
                    s.scanned
                ));
            }
            out
        }
        Response::Stats(s) => format!(
            "{} entries ({} folders), {} on disk, contents {}.",
            s.entries,
            s.dirs,
            format_size(s.bytes_on_disk, BINARY),
            if s.has_content {
                "indexed"
            } else {
                "not indexed"
            }
        ),
        Response::Maintained(m) => format!("{:?} finished in {} ms.", m.level, m.took_ms),
        Response::Accepted => "Accepted.".into(),
        Response::Text { text } => text.clone(),
    }
}

fn tree(node: &TreeNode, depth: usize, out: &mut String) {
    let pad = "  ".repeat(depth);
    if node.is_dir {
        out.push_str(&format!(
            "{pad}{}/  ({} entries)\n",
            node.name, node.children
        ));
    } else {
        out.push_str(&format!(
            "{pad}{}  {}\n",
            node.name,
            format_size(node.size as u64, BINARY)
        ));
    }
    for c in &node.nodes {
        tree(c, depth + 1, out);
    }
    // Never trimmed: without this a model reads a partial listing as a
    // complete one and concludes the missing files do not exist.
    if node.truncated {
        let left = node.children.saturating_sub(node.nodes.len() as u64);
        out.push_str(&format!(
            "{pad}  … {left} more not shown{}\n",
            if node.nodes.is_empty() {
                " (ask for this path to see them)"
            } else {
                ""
            }
        ));
    }
}

/// The English text of a typed failure, plus what to do about it.
///
/// The advice is the point: a model that is told "the index is not ready yet"
/// and nothing else will either give up or retry forever.
/// What the engine could not read the way it was written, if anything.
///
/// **The worst failure this server has, and it looks like a correct answer.**
/// The parser never fails: `dm:yarin` is not a date, so the whole term becomes
/// a search for the text "dm:yarin", and the reply is `0 matches.` — which is
/// also what a query that was understood and matched nothing says. A model
/// reading that concludes the files are not there and moves on. There is no
/// second question it would know to ask.
///
/// Appended rather than woven into each arm, so that adding a reply type
/// cannot quietly drop it.
///
/// [`Role::BadValue`] only, though the wire carries both warning roles:
/// `UnknownField` means letters and a colon that are not a field, which is
/// what `http://example.com` and `12:30` legitimately are. A field that exists
/// and refuses its value is the case that is nearly always a mistake.
pub fn warning(r: &Response, query: Option<&str>) -> String {
    let (Some(query), Some(misread)) = (query, misread_of(r)) else {
        return String::new();
    };
    let mut terms: Vec<&str> = Vec::new();
    for s in misread.iter().filter(|s| s.role == Role::BadValue) {
        let term = s.term_of(query);
        if !term.is_empty() && !terms.contains(&term) {
            terms.push(term);
        }
    }
    if terms.is_empty() {
        return String::new();
    }
    format!(
        "\n\nNote: {} not read as {} — the whole term was searched for as \
         ordinary text, so this answer is about a different question than the \
         one you meant. Call scour_syntax for the field's accepted values.",
        terms
            .iter()
            .map(|t| format!("`{t}` was"))
            .collect::<Vec<_>>()
            .join(", "),
        if terms.len() == 1 {
            "a filter"
        } else {
            "filters"
        },
    )
}

fn misread_of(r: &Response) -> Option<&[Span]> {
    match r {
        Response::Search(s) => Some(&s.misread),
        Response::Count { misread, .. } => Some(misread),
        Response::Facets(f) => Some(&f.misread),
        _ => None,
    }
}

pub fn failure(e: &Error) -> String {
    let advice = match e {
        Error::QueryTooShort { .. } => {
            " Use at least three characters, or narrow with a field like ext: or under:."
        }
        Error::ContentNotIndexed => " This index holds file names, not document contents.",
        Error::NotIndexed => " The first scan has not finished. Try again shortly.",
        Error::NotFound { .. } => " Check the path, or call scour_sources to see what is indexed.",
        Error::Unreachable { .. } => {
            " The Scour service is not running. It has to be started separately."
        }
        _ => "",
    };
    format!("{e}.{advice}")
}

/// The word for a kind, for a reader who is a model.
///
/// Straight from `Kind::msgid()` and not through the catalogue: this surface
/// is English by design. A second table here is a second thing to forget when
/// a kind is added, which is exactly what happened to the first version.
fn kind_word(k: Kind) -> String {
    k.msgid().to_lowercase()
}

/// `YYYY-MM-DD`. The time of day is rarely what is being asked and doubles the
/// width of every row.
fn day(secs: i64) -> String {
    if secs <= 0 {
        return "??????????".into();
    }
    let z = secs.div_euclid(86_400) + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{m:02}-{d:02}", if m <= 2 { y + 1 } else { y })
}

#[cfg(test)]
mod tests {
    use super::*;
    use scour_core::{Facet, FacetResponse, Hit, SearchResponse};

    #[test]
    fn a_truncated_listing_always_says_so() {
        let node = TreeNode {
            name: "u".into(),
            path: "/home/u".into(),
            is_dir: true,
            kind: Kind::Dir,
            size: 0,
            mtime: 0,
            children: 1_000,
            nodes: Vec::new(),
            truncated: true,
        };
        let out = human(&Response::Tree { root: node });
        assert!(out.contains("1000 more not shown"), "{out}");
    }

    #[test]
    fn a_capped_count_is_reported_as_a_floor() {
        assert!(
            human(&Response::Count {
                total: 10_000,
                capped: true,
                misread: Vec::new()
            })
            .starts_with("At least")
        );
        assert_eq!(
            human(&Response::Count {
                total: 7,
                capped: false,
                misread: Vec::new()
            }),
            "7 matches."
        );
    }

    #[test]
    fn a_partial_page_tells_the_caller_there_is_more() {
        let hit = Hit {
            id: scour_core::EntryId::path_hash(scour_core::SourceId(0), "/a"),
            path: "/a".into(),
            is_dir: false,
            kind: Kind::File,
            meta: scour_core::Meta {
                size: 10,
                mtime: 1_769_817_600,
                ..scour_core::Meta::UNKNOWN
            },
        };
        let out = human(&Response::Search(SearchResponse {
            hits: vec![hit],
            total: 500,
            capped: false,
            took_us: 1_200,
            fast_path: true,
            rows_visited: 0,
            rows_built: 0,
            misread: Vec::new(),
        }));
        assert!(out.contains("1 shown of 500"), "{out}");
        assert!(out.contains("narrow the query"), "{out}");
        assert!(out.contains("2026-01-31"), "{out}");
    }

    #[test]
    fn failures_carry_advice_a_caller_can_act_on() {
        let out = failure(&Error::QueryTooShort { need: 3 });
        assert!(out.contains("three characters"), "{out}");
        assert!(failure(&Error::Unreachable { detail: "x".into() }).contains("not running"));
    }

    #[test]
    fn empty_facets_say_so_rather_than_returning_nothing() {
        assert_eq!(
            human(&Response::Facets(FacetResponse::default())),
            "Nothing matched."
        );
        let f = FacetResponse {
            groups: Vec::new(),
            total: 0,
            facets: vec![Facet {
                key: "rs".into(),
                count: 3,
            }],
            by: scour_core::FacetBy::Ext { top: 10 },
            capped: false,
            took_us: 0,
            misread: Vec::new(),
        };
        assert!(human(&Response::Facets(f)).contains("rs"));
    }
}
