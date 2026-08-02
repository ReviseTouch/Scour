//! Turning a reply into something a person reads.
//!
//! Only this file composes sentences. Everything below it returned numbers,
//! codes and typed variants precisely so that the wording lives in one place
//! and can be translated once.

use anyhow::Result;
use humansize::{BINARY, format_size};
use scour_core::{Kind, TreeNode};
use scour_proto::Response;

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
                if r.fast_path { "" } else { " (full scan)" }
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
            println!("path      {}", e.path);
            println!("name      {}", e.name());
            println!("kind      {}", kind_tag(e.kind()));
            if !e.is_dir {
                println!("size      {}", format_size(e.meta.size as u64, BINARY));
                println!("on disk   {}", format_size(e.meta.disk as u64, BINARY));
            } else if e.meta.items >= 0 {
                println!("items     {}", e.meta.items);
            }
            println!("modified  {}", stamp(e.meta.mtime));
            println!("created   {}", stamp(e.meta.ctime));
            println!("accessed  {}", stamp(e.meta.atime));
            let mode = scour_core::mode_string(e.meta.mode);
            if !mode.is_empty() {
                println!("mode      {mode}  {}:{}", e.meta.uid, e.meta.gid);
            }
        }
        Response::Explain {
            description,
            needs_content,
        } => {
            println!("{description}");
            if *needs_content {
                println!("(needs document contents, which this index may not have)");
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
            println!("entries       {}", s.entries);
            println!("index         {}", format_size(s.index_bytes, BINARY));
            println!("sources       {}  watching {}", s.sources, s.watching);
            println!("scanning      {}", if s.scanning { "yes" } else { "no" });
            if s.scanning {
                println!("seen so far   {}", s.scanned);
            }
            println!("pending       {}", s.pending);
            println!(
                "unsorted      {}{}",
                s.unsorted,
                if s.rebuild_advised {
                    "  (a rebuild would speed searches up)"
                } else {
                    ""
                }
            );
            if s.cold {
                println!("\nThe index is empty. Run `scour rescan`.");
            }
        }
        Response::Stats(s) => {
            println!("entries       {}", s.entries);
            println!("folders       {}", s.dirs);
            println!("on disk       {}", format_size(s.bytes_on_disk, BINARY));
            println!("segments      {}", s.segments);
            println!("unsorted      {}", s.unsorted_entries);
            println!(
                "contents      {}",
                if s.has_content {
                    "indexed"
                } else {
                    "not indexed"
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
        Response::Accepted => println!("accepted"),
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

fn kind_tag(k: Kind) -> &'static str {
    match k {
        Kind::Dir => "folder",
        Kind::Code => "code",
        Kind::Image => "image",
        Kind::Archive => "archive",
        Kind::Doc => "document",
        Kind::Exec => "program",
        Kind::Media => "media",
        Kind::File => "file",
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
    println!("settings   {}", scour_config::config_path().display());
    println!("index      {}", config.index.dir.display());
    println!("socket     {}", config.socket());
    println!(
        "service    {}",
        if scour_ipc::is_running(&config.socket()) {
            "running"
        } else {
            "not running"
        }
    );
    if let Some(e) = problem {
        println!("\nproblem    {e}");
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
    eprintln!("\nStart `scourd` first — the MCP server is a client of it, like this one.");
    Ok(())
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
    fn every_kind_has_a_word() {
        for k in Kind::ALL {
            assert!(!kind_tag(k).is_empty());
        }
    }
}
