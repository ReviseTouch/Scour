//! Turning a reply into something a person reads.
//!
//! Only this file composes sentences. Everything below it returned numbers,
//! codes and typed variants precisely so that the wording lives in one place
//! and can be translated once — which is what `t()` below actually does.
//!
//! The English text is the key, so an untranslated string is still correct,
//! just untranslated. `SCOUR_LANG=tr scour status` switches this program alone.

use std::sync::OnceLock;

use anyhow::Result;
use humansize::{BINARY, format_size};
use scour_core::{Catalog, FacetBy, Kind, Role, TreeNode};
use scour_i18n::Catalogue;
use scour_proto::Response;

/// The language this run speaks.
fn catalogue() -> &'static Catalogue {
    static C: OnceLock<Catalogue> = OnceLock::new();
    C.get_or_init(Catalogue::from_environment)
}

/// Translate. The argument is the English text, which is also the key.
fn t(msgid: &str) -> String {
    catalogue().get(msgid).into_owned()
}

/// A label, padded so the column lines up whatever language it is in.
fn label(msgid: &str) -> String {
    // Twelve columns *and* a space. `değiştirilme` is exactly twelve
    // characters, so padding alone leaves the value touching the label — the
    // kind of thing that only shows up in the language nobody tested in.
    format!("{:<12} ", t(msgid))
}

/// Say which terms the engine could not read the way they were written.
///
/// **The one thing a search box gets for free and a command line does not.**
/// The parser never fails, so `dm:yarin` becomes a search for the text
/// "dm:yarin" and answers `0 of 0` — the same answer a query that was
/// understood and matched nothing gives, and the reader has no way to tell
/// them apart. A window colours the term while it is being typed; here there
/// is one line of output and then the shell prompt.
///
/// To stderr, beside the timing line, so a pipeline still receives only paths.
///
/// **[`Role::BadValue`] only, though the wire carries both warning roles.**
/// `UnknownField` means "letters, a colon, and not a field", which is a
/// perfectly ordinary thing to search for: `http://example.com`, `12:30`,
/// `C:`. Warning about those would put a line of noise under a query that did
/// exactly what it looked like. `BadValue` is the other case — a field that
/// *is* real, refusing the value written for it — and that is nearly always a
/// mistake.
fn complain(misread: &[scour_core::Span], query: Option<&str>) {
    let Some(query) = query else { return };
    let mut said: Vec<&str> = Vec::new();
    for span in misread.iter().filter(|s| s.role == Role::BadValue) {
        let term = span.term_of(query);
        // One line per term, not per span: `ext:a size:>x;>y` can refuse twice
        // inside one term and saying so twice adds nothing.
        if term.is_empty() || said.contains(&term) {
            continue;
        }
        said.push(term);
        eprintln!(
            "{}: `{term}` {}",
            t("warning"),
            t("was searched for as text")
        );
    }
}

pub fn human(reply: &Response, echo: Option<&str>) -> Result<()> {
    match reply {
        Response::Search(r) => {
            for h in &r.hits {
                println!(
                    "{:>10}  {:>10}  {}",
                    kind_tag(h.kind),
                    if h.is_dir {
                        "—".to_owned()
                    } else {
                        format_size(h.meta.size as u64, BINARY)
                    },
                    h.path
                );
            }
            let total = if r.capped {
                format!("{}+", r.total)
            } else {
                r.total.to_string()
            };
            eprintln!(
                "{} of {total} in {:.2} ms{}{}",
                r.hits.len(),
                r.took_us as f64 / 1000.0,
                if r.fast_path {
                    if r.rows_visited > 0 {
                        format!(" ({} rows)", r.rows_visited)
                    } else {
                        String::new()
                    }
                } else {
                    format!(" ({})", t("full scan"))
                },
                // Only when it dominates. Paging deep means building a path
                // per row to reach the offset and throwing all but a page of
                // them away — 225 ms at offset 200,000 against 0.54 ms at
                // zero — and without this the slowness looks like the query's
                // fault rather than the page number's.
                if r.rows_built > r.hits.len() as u64 * 2 {
                    format!(" · {} {}", r.rows_built, t("paths built"))
                } else {
                    String::new()
                }
            );
            complain(&r.misread, echo);
        }
        Response::Count {
            total,
            capped,
            misread,
        } => {
            println!("{total}{}", if *capped { "+" } else { "" });
            complain(misread, echo);
        }
        Response::Facets(f) => {
            // A kind facet is keyed by the token so that a rail can build
            // `kind:build` from it; a person should see the word for it in
            // their own language. `by` is what says which of the two this is —
            // an extension can be spelled like a token (`ext:bin` against
            // `kind:bin`) and translating it would be a plain mistake.
            let shown: Vec<String> = f
                .facets
                .iter()
                .map(|x| match f.by {
                    FacetBy::Kind => facet_word(&x.key),
                    _ => x.key.clone(),
                })
                .collect();
            let width = shown.iter().map(|s| s.chars().count()).max().unwrap_or(0);
            for (key, x) in shown.iter().zip(&f.facets) {
                println!("{key:<width$}  {:>10}", x.count);
            }
            if f.capped {
                println!("{}", t("counts are a lower bound: the scan hit its cap"));
            }
            complain(&f.misread, echo);
        }
        Response::Usage(u) => {
            println!(
                "{:>10}  {:>10}  {:>9}  {}",
                t("size"),
                t("on disk"),
                t("files"),
                u.root.path
            );
            let row = |d: &scour_core::DirUsage| {
                println!(
                    "{:>10}  {:>10}  {:>9}  {}  {}",
                    format_size(d.bytes, BINARY),
                    format_size(d.disk, BINARY),
                    d.files,
                    ages(d),
                    d.path
                );
            };
            row(&u.root);
            for c in &u.children {
                row(c);
            }
            if u.child_count as usize > u.children.len() {
                println!(
                    "{} / {} {}",
                    u.children.len(),
                    u.child_count,
                    t("folders, largest first")
                );
            }
            // Said rather than left to be found out: a total that disagrees
            // with `du -l` for a reason nobody stated reads as a bug.
            println!("{}", t("a hard-linked file is counted once, like du"));
        }
        Response::Tree { root } => print_tree(root, ""),
        Response::Stat(e) => {
            println!("{}{}", label("path"), e.path);
            println!("{}{}", label("name"), e.name());
            println!("{}{}", label("kind"), kind_tag(e.kind()));
            if !e.is_dir {
                println!(
                    "{}{}",
                    label("size"),
                    format_size(e.meta.size as u64, BINARY)
                );
                println!(
                    "{}{}",
                    label("on disk"),
                    format_size(e.meta.disk as u64, BINARY)
                );
            } else if e.meta.items >= 0 {
                println!("{}{}", label("items"), e.meta.items);
            }
            println!("{}{}", label("modified"), stamp(e.meta.mtime));
            println!("{}{}", label("created"), stamp(e.meta.ctime));
            println!("{}{}", label("accessed"), stamp(e.meta.atime));
            let mode = scour_core::mode_string(e.meta.mode);
            if !mode.is_empty() {
                println!("{}{mode}  {}:{}", label("mode"), e.meta.uid, e.meta.gid);
            }
        }
        Response::Explain {
            description,
            needs_content,
            spans,
            ..
        } => {
            // The colouring is meant for a search box, but a terminal has
            // colours too — and printing it here is what proves the spans line
            // up with the query rather than merely claiming to.
            if let Some(q) = echo {
                println!("{}", paint(q, spans));
            }
            println!("{description}");
            if *needs_content {
                println!(
                    "({})",
                    t("needs document contents, which this index may not have")
                );
            }
        }
        Response::Sources { sources } => {
            for s in sources {
                println!("{}  {:?}", s.name, s.kind);
                for r in &s.roots {
                    println!("    {r}");
                }
                println!("    {:?}", s.caps);
            }
        }
        Response::Status(s) => {
            println!("{}{}", label("entries"), s.entries);
            println!("{}{}", label("index"), format_size(s.index_bytes, BINARY));
            println!(
                "{}{}  {} {}",
                label("sources"),
                s.sources,
                t("watching"),
                s.watching
            );
            println!(
                "{}{}",
                label("scanning"),
                if s.scanning { t("yes") } else { t("no") }
            );
            if s.scanning {
                println!("{}{}", label("seen so far"), s.scanned);
            }
            println!("{}{}", label("pending"), s.pending);
            println!(
                "{}{}{}",
                label("unsorted"),
                s.unsorted,
                if s.rebuild_advised {
                    format!("  ({})", t("a rebuild would speed searches up"))
                } else {
                    String::new()
                }
            );
            if s.cold {
                println!("\n{}", t("The index is empty. Run `scour rescan`."));
            }
        }
        Response::Stats(s) => {
            println!("{}{}", label("entries"), s.entries);
            println!("{}{}", label("folders"), s.dirs);
            println!(
                "{}{}",
                label("on disk"),
                format_size(s.bytes_on_disk, BINARY)
            );
            println!("{}{}", label("segments"), s.segments);
            println!("{}{}", label("unsorted"), s.unsorted_entries);
            println!(
                "{}{}",
                label("contents"),
                if s.has_content {
                    t("indexed")
                } else {
                    t("not indexed")
                }
            );
        }
        Response::Maintained(r) => {
            println!(
                "{:?}: {} → {} in {} ms",
                r.level,
                format_size(r.bytes_before, BINARY),
                format_size(r.bytes_after, BINARY),
                r.took_ms
            );
        }
        Response::Accepted => println!("{}", t("accepted")),
        Response::Text { text } => println!("{text}"),
    }
    Ok(())
}

fn print_tree(node: &TreeNode, prefix: &str) {
    let count = if node.is_dir && node.children > 0 {
        format!("  ({})", node.children)
    } else if node.is_dir {
        String::new()
    } else {
        format!("  {}", format_size(node.size as u64, BINARY))
    };
    println!(
        "{prefix}{}{}{count}",
        node.name,
        if node.is_dir { "/" } else { "" }
    );
    let deeper = format!("{prefix}  ");
    for child in &node.nodes {
        print_tree(child, &deeper);
    }
    if node.truncated {
        println!(
            "{deeper}… {} more",
            node.children.saturating_sub(node.nodes.len() as u64)
        );
    }
}

/// The word for a kind.
///
/// Through `Kind::msgid()` rather than a second table here: the kinds are core
/// vocabulary and every frontend has to name them the same way, or a filter
/// called "Belge" in one place and "Document" in another is the same filter
/// with two names.
/// The age of a directory's bytes, as one compact bar.
///
/// Six bands — today, this week, this month, six months, this year, older —
/// drawn as a share of the total rather than as numbers, because the question
/// it answers is comparative: twenty-five gigabytes matters less than
/// twenty-five gigabytes nothing has touched in a year.
fn ages(d: &scour_core::DirUsage) -> String {
    const BLOCKS: [char; 5] = ['\u{2581}', '\u{2583}', '\u{2585}', '\u{2587}', '\u{2588}'];
    let total = d.bytes.max(1);
    d.age
        .iter()
        .map(|b| {
            if *b == 0 {
                ' '
            } else {
                let share = (*b as f64 / total as f64 * BLOCKS.len() as f64).ceil() as usize;
                BLOCKS[share.clamp(1, BLOCKS.len()) - 1]
            }
        })
        .collect()
}

fn kind_tag(k: Kind) -> String {
    t(k.msgid())
}

/// The word for a `kind:` facet key.
///
/// An unknown token is printed as it arrived rather than dropped: a service
/// newer than this client can send a kind this build has never heard of, and
/// showing the token is a much better answer than showing nothing.
fn facet_word(token: &str) -> String {
    match Kind::from_name(token) {
        Some([k]) => kind_tag(*k),
        _ => token.to_owned(),
    }
}

/// `YYYY-MM-DD HH:MM` in UTC.
///
/// Local time would need the zone database, and a file listing is read for
/// ordering far more often than for the exact minute. The one place this is
/// wrong enough to matter is a user interface, which will have a clock.
fn stamp(secs: i64) -> String {
    if secs <= 0 {
        return "—".into();
    }
    let days = secs.div_euclid(86_400);
    let rest = secs.rem_euclid(86_400);
    let (y, m, d) = civil(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        rest / 3600,
        (rest % 3600) / 60
    )
}

fn civil(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Where everything lives. Answered without a service, because this is what
/// you reach for when the service is the thing not working.
pub fn locations() -> Result<()> {
    let (config, problem) = scour_config::Config::load_or_default();
    println!(
        "{}{}",
        label("settings"),
        scour_config::config_path().display()
    );
    println!("{}{}", label("index"), config.index.dir.display());
    println!("{}{}", label("socket"), config.socket());
    println!(
        "{}{}",
        label("service"),
        if scour_ipc::is_running(&config.socket()) {
            t("running")
        } else {
            t("not running")
        }
    );
    println!(
        "{}{} ({})",
        label("language"),
        catalogue().locale(),
        languages()
    );
    if let Some(e) = problem {
        println!("\n{}{e}", label("problem"));
    }
    Ok(())
}

/// The block to paste into an MCP client's configuration.
pub fn mcp_config() -> Result<()> {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("scour-mcp")))
        .filter(|p| p.exists())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "scour-mcp".to_owned());
    println!(
        r#"{{
  "mcpServers": {{
    "scour": {{
      "command": "{exe}",
      "args": []
    }}
  }}
}}"#
    );
    eprintln!(
        "\n{}",
        t("Start `scourd` first — the MCP server is a client of it, like this one.")
    );
    Ok(())
}

/// Which languages this build ships, for `scour where`.
fn languages() -> String {
    scour_i18n::LANGUAGES
        .iter()
        .map(|(tag, name)| format!("{tag} {name}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The query, with each run in the colour its role earns.
///
/// A terminal is not the audience for this — a search box is — but the two
/// warning roles are worth having at the command line too, because they are
/// the cases where the engine answers a question nobody asked. `kind:zurna`
/// looks like a filter and is a text search, and here it is underlined in red
/// rather than discovered three screens of results later.
///
/// Colours are dropped when the output is not a terminal, so `scour explain |
/// grep` sees plain text.
fn paint(query: &str, spans: &[scour_core::Span]) -> String {
    use scour_core::Role;
    if !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        return query.to_owned();
    }
    let mut out = String::with_capacity(query.len() * 2);
    for s in spans {
        let text = s.of(query);
        let code = match s.role {
            Role::Text => "0",
            Role::Glob => "35",
            Role::Phrase => "36",
            Role::Quote => "2;36",
            Role::Field => "1;34",
            Role::Colon | Role::Sep => "2",
            Role::Value => "32",
            Role::Cmp => "33",
            Role::Not => "1;31",
            Role::Or => "1;33",
            Role::Space => "0",
            // The two that mean "this is not doing what it looks like".
            Role::UnknownField | Role::BadValue => "4;31",
        };
        out.push_str(&format!("\x1b[{code}m{text}\x1b[0m"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_read_as_dates() {
        assert_eq!(stamp(0), "—", "an unknown time is not 1970");
        assert_eq!(stamp(1_769_817_600), "2026-01-31 00:00");
        assert_eq!(stamp(1_769_817_600 + 3_661), "2026-01-31 01:01");
    }

    #[test]
    fn every_kind_has_a_word_in_every_shipped_language() {
        for (tag, _) in scour_i18n::LANGUAGES {
            let c = scour_i18n::Catalogue::for_language(tag);
            for k in Kind::ALL {
                let word = c.get(k.msgid());
                assert!(!word.is_empty(), "{tag} has no word for {k:?}");
            }
        }
    }

    /// Every string this file asks for exists in the Turkish catalogue.
    ///
    /// A missing one is not a failure at run time — it falls back to English —
    /// which is exactly why it needs a test: a half-translated program looks
    /// fine until someone reads it.
    #[test]
    fn the_turkish_catalogue_covers_what_this_file_uses() {
        let c = scour_i18n::Catalogue::for_language("tr");
        let used = [
            "path",
            "name",
            "kind",
            "size",
            "on disk",
            "items",
            "modified",
            "created",
            "accessed",
            "mode",
            "entries",
            "folders",
            "index",
            "sources",
            "watching",
            "scanning",
            "seen so far",
            "pending",
            "unsorted",
            "segments",
            "contents",
            "yes",
            "no",
            "indexed",
            "not indexed",
            "running",
            "not running",
            "settings",
            "socket",
            "service",
            "problem",
            "accepted",
            "full scan",
            "paths built",
            "warning",
            "was searched for as text",
            "a rebuild would speed searches up",
            "The index is empty. Run `scour rescan`.",
            "needs document contents, which this index may not have",
            "Start `scourd` first — the MCP server is a client of it, like this one.",
        ];
        let missing: Vec<&str> = used.iter().copied().filter(|m| c.get(m) == **m).collect();
        assert!(missing.is_empty(), "untranslated: {missing:?}");
    }
}
