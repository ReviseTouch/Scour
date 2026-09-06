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
/// Sizes as every face says them. See [`scour_ui::format::size`].
fn format_size(bytes: u64, _unused: ()) -> String {
    scour_ui::format::size(bytes, '.')
}
/// What the old call sites pass as a unit; the shared formatter picks its own.
const BINARY: () = ();
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
                    // A folder's number is what is under it, and the `~` says
                    // it is the size of what the index holds — whatever the
                    // scan rules exclude is not in it. A dash where the index
                    // could not say, which is not the same as zero.
                    match (h.is_dir, h.under) {
                        (true, Some(u)) => format!("~{}", format_size(u.disk, BINARY)),
                        (true, None) => "—".to_owned(),
                        (false, _) => format_size(h.meta.size as u64, BINARY),
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
            // A count prints one number and a person reading it is not asking
            // what it cost. `--json` carries it for anyone who is.
            took_us: _,
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
        Response::Duplicates {
            groups,
            candidates,
            waste,
            proven,
            read,
            unconfirmed,
        } => {
            for g in groups {
                // The saving first, because it is the reason to look: a header
                // line per group and then its paths, so the output stays
                // greppable and a person reads down the left edge.
                println!(
                    "{:>10}  ×{}  {}  [{}]",
                    format_size(g.waste, BINARY),
                    g.paths.len(),
                    format_size(g.size, BINARY),
                    t(match g.certainty.as_str() {
                        "content" => "identical",
                        "edges" => "same ends",
                        _ => "same size only",
                    })
                );
                for p in &g.paths {
                    println!("            {p}");
                }
            }
            // **Two numbers, not one.** The first is what was read and
            // compared; the second is what merely shares a size with
            // something. On this disk they are 18.29 GiB and 39.36 GiB, and
            // printing only the larger tells somebody they can delete
            // database pages that happen to be the same length.
            eprintln!(
                "{} {} · {} {} · {} {} · {} {}",
                format_size(*proven, BINARY),
                t("confirmed"),
                format_size(*waste, BINARY),
                t("could be freed"),
                candidates,
                t("candidates"),
                format_size(*read, BINARY),
                t("read")
            );
            // **The one line that must never be dropped.** A partial answer
            // that looks complete is what gets files deleted on a guess.
            if *unconfirmed > 0 {
                eprintln!(
                    "{unconfirmed} {}",
                    t("groups were not confirmed: raise --budget-mb")
                );
            }
        }
        Response::Settings(s) => {
            // Printed rather than hidden, because `scour where` exists for the
            // same reason: when something is remembered wrongly, the first
            // question is what is remembered.
            println!("{}{}", label("columns"), s.columns.join(", "));
            println!("{}{}", label("sort"), s.sort);
            for q in s.history.iter().take(20) {
                println!("{}{q}", label("history"));
            }
        }
        Response::Preview(l) => {
            println!(
                "{}{}",
                label("shape"),
                if l.shape == "none" { "—" } else { &l.shape }
            );
            if !l.kind.is_empty() {
                println!("{}{}", label("type"), l.kind);
            }
            println!("{}{}", label("size"), l.len);
            if !l.head.is_empty() {
                println!();
                print!("{}", l.head);
                if l.cut {
                    // Said here rather than appended to the text, which would
                    // put the note inside the file being previewed.
                    println!("\n[…]");
                }
            }
        }
        // **No CLI command asks for this**, and none is being added: making
        // thumbnails is what a window that draws them wants, and a terminal
        // that drew one would have nowhere to put it. The arm exists because
        // the match is exhaustive, and it prints the one thing a terminal
        // could do with the answer — the paths that have a picture now, plain,
        // so nothing here needs a word from the catalogue.
        Response::Thumbnails(made) => {
            for path in &made.ready {
                println!("{path}");
            }
        }
        // Built in first and marked, because the difference is the whole
        // point: one list can be edited and the other cannot, and it is the
        // uneditable one that does nearly all of the excluding.
        Response::Rules {
            builtin_paths,
            builtin_dirs,
            builtin_files,
            config_paths,
            config_dirs,
            config_files,
            config_allow,
            added_paths,
            added_dirs,
            added_files,
            added_allow,
            off,
        } => {
            for (what, kind, list) in [
                ("builtin path", "path", builtin_paths),
                ("builtin dir", "dir", builtin_dirs),
                ("builtin file", "file", builtin_files),
                ("config path", "path", config_paths),
                ("config dir", "dir", config_dirs),
                ("config file", "file", config_files),
                ("config allow", "allow", config_allow),
                ("added path", "path", added_paths),
                ("added dir", "dir", added_dirs),
                ("added file", "file", added_files),
                ("added allow", "allow", added_allow),
            ] {
                for v in list {
                    // A switched-off rule is listed and marked rather than
                    // hidden: it is still a rule somebody wrote, and the whole
                    // point of the switch is that it can be turned back on.
                    let id = scour_settings::rule_id(kind, v);
                    let state = if off.iter().any(|o| o.eq_ignore_ascii_case(&id)) {
                        format!("  ({})", t("off"))
                    } else {
                        String::new()
                    };
                    println!("{}{v}{state}", label(what));
                }
            }
        }
        Response::Places(p) => {
            println!("{}{}", label("home"), p.home);
            for place in &p.places {
                println!("{}{}", label(&place.label), place.path);
            }
            for m in p.mounts.iter().filter(|m| !m.reads) {
                println!("{}{}", label("noatime"), m.at);
            }
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
        Response::Tree { root, took_us: _ } => print_tree(root, ""),
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
        // An export does not arrive here. Its pieces go to the file or the
        // pipe as they come — see `run_export` in `main.rs` — and passing one
        // through the pretty-printer would put it on stdout a second time.
        //
        // Named rather than covered by a wildcard: this match is what makes
        // whoever adds the next response variant decide how it looks, and one
        // `_` would retire that for every variant after it.
        Response::ExportChunk { .. } | Response::ExportDone { .. } => {}
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

/// `YYYY-MM-DD HH:MM` in UTC, or a dash for a time nothing set.
///
/// The shape is [`scour_ui::format::stamp`]'s — the same one every face
/// prints, because a listing read here and in a window has to be the same
/// listing. The dash is this one's own: a column of text has nowhere else to
/// put "nothing".
fn stamp(secs: i64) -> String {
    match scour_ui::format::stamp(secs).as_str() {
        "" => "—".into(),
        said => said.to_owned(),
    }
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
            // The two that mean "this is not doing what it looks like". They
            // come first because a term can be both excluded and misread, and
            // the misreading is the thing worth saying.
            Role::UnknownField | Role::BadValue => "4;31",
            // The whole of an excluded term, not the `!` in front of it.
            _ if s.not => "1;31",
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
            // A `;` typed instead of a space is drawn like one: quiet.
            Role::Space => "2",
        };
        out.push_str(&format!("\x1b[{code}m{text}\x1b[0m"));
    }
    out
}

/// Open another face, or say which one opens.
///
/// The launcher is the one thing that knows how to start each of them — which
/// terminal, which flag — and it is also what the desktop entry runs. So this
/// asks it rather than knowing any of that itself.
pub fn faces(face: Option<&str>) -> anyhow::Result<()> {
    let dir = std::env::var("XDG_DATA_HOME")
        .unwrap_or_else(|_| format!("{}/.local/share", std::env::var("HOME").unwrap_or_default()));
    let settings = format!("{dir}/scour/state/settings.json");
    let Some(face) = face else {
        // Read rather than asked over the socket: this is a question about
        // what would open, and the answer has to be available when nothing is
        // running.
        let said = std::fs::read_to_string(&settings)
            .ok()
            .and_then(|text| {
                text.split(r#""face""#)
                    .nth(1)?
                    .split('"')
                    .nth(1)
                    .map(str::to_owned)
            })
            .filter(|f| !f.is_empty())
            .unwrap_or_else(|| "window".into());
        println!("{said}");
        return Ok(());
    };
    if !matches!(face, "window" | "tui" | "browser") {
        anyhow::bail!("no such face: {face} — try window, tui or browser");
    }
    let status = std::process::Command::new("scour-open").arg(face).status();
    match status {
        Ok(_) => Ok(()),
        Err(e) => anyhow::bail!("scour-open: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **In the person's zone, like every other face.** This pinned UTC for
    /// as long as `scour_ui::format::stamp` was UTC; the day that changed —
    /// a file saved at 12:08 showing as 09:08 in the window — this test was
    /// the one place still insisting on the old answer. The format is pinned
    /// through the pure half, the zone through the same call the code uses.
    #[test]
    fn timestamps_read_as_dates() {
        use scour_ui::format::{local_offset, stamp_at};
        assert_eq!(stamp(0), "—", "an unknown time is not 1970");
        let at = 1_769_817_600;
        assert_eq!(stamp_at(at, 0), "2026-01-31 00:00", "the shape, zone-free");
        assert_eq!(stamp(at), stamp_at(at, local_offset(at)));
        assert_eq!(stamp(at + 3_661), stamp_at(at + 3_661, local_offset(at + 3_661)));
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
            "identical",
            "same ends",
            "same size only",
            "confirmed",
            "could be freed",
            "candidates",
            "read",
            "groups were not confirmed: raise --budget-mb",
            "warning",
            "columns",
            "sort",
            "history",
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
