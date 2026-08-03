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
use scour_core::{Catalog, Kind, TreeNode};
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

pub fn human(reply: &Response) -> Result<()> {
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
                "{} of {total} in {:.2} ms{}",
                r.hits.len(),
                r.took_us as f64 / 1000.0,
                if r.fast_path {
                    if r.rows_visited > 0 {
                        format!(" ({} rows)", r.rows_visited)
                    } else {
                        String::new()
                    }
                } else {
                    " (full scan)".to_owned()
                }
            );
        }
        Response::Count { total, capped } => {
            println!("{total}{}", if *capped { "+" } else { "" });
        }
        Response::Facets(f) => {
            let width = f
                .facets
                .iter()
                .map(|x| x.key.chars().count())
                .max()
                .unwrap_or(0);
            for x in &f.facets {
                println!("{:<width$}  {:>10}", x.key, x.count);
            }
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
        } => {
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
fn kind_tag(k: Kind) -> String {
    t(k.msgid())
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
            "a rebuild would speed searches up",
            "The index is empty. Run `scour rescan`.",
            "needs document contents, which this index may not have",
            "Start `scourd` first — the MCP server is a client of it, like this one.",
        ];
        let missing: Vec<&str> = used.iter().copied().filter(|m| c.get(m) == **m).collect();
        assert!(missing.is_empty(), "untranslated: {missing:?}");
    }
}
