//! Turning a reply into something a person reads. Only this file composes
//! sentences, so the wording is translated in one place. The English text is the
//! key, so an untranslated string is still correct; `SCOUR_LANG=tr` switches it.

use std::sync::OnceLock;

use anyhow::Result;
/// Sizes as every face says them. See [`scour_ui::format::size`].
fn format_size(bytes: u64, _unused: ()) -> String {
    scour_ui::format::size(bytes, '.')
}
/// The unit argument every call site passes; the shared formatter picks its own.
const BINARY: () = ();
use scour_chart::{bar, fold, segment_cells, shares};
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
    // Twelve columns and a space: `değiştirilme` is exactly twelve characters,
    // so padding alone leaves the value touching the label.
    format!("{:<12} ", t(msgid))
}

/// Say which terms the engine could not read as written, to stderr so a pipeline
/// still receives only paths. [`Role::BadValue`] only, though the wire carries
/// both roles: `UnknownField` is ordinary text like `http://example.com`.
fn complain(misread: &[scour_core::Span], query: Option<&str>) {
    let Some(query) = query else { return };
    let mut said: Vec<&str> = Vec::new();
    for span in misread.iter().filter(|s| s.role == Role::BadValue) {
        let term = span.term_of(query);
        // One line per term, not per span: one term can refuse twice.
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
                    // A folder's number is what is under it; `~` says it counts
                    // only what the index holds. A dash is unknown, not zero.
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
                "{} of {total} in {:.2} ms{}{}{}",
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
                // Only when it dominates: a deep page builds a path per row to
                // reach the offset — 225 ms at offset 200,000 against 0.54 ms at
                // zero — so the cost has to be named as the page number's.
                if r.rows_built > r.hits.len() as u64 * 2 {
                    format!(" · {} {}", r.rows_built, t("paths built"))
                } else {
                    String::new()
                },
                // Printed only when something was actually held back.
                if (r.total as usize) > r.hits.len() || r.capped {
                    // Larger than what was just shown, and placed before the
                    // query: everything after the query is the query, so `-n`
                    // written after it is searched for. See `Args::query`.
                    format!(
                        " · -n {} {}",
                        (r.hits.len() * 8).max(20),
                        t("before the query, for more")
                    )
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
            // No timing line here; `--json` carries it for anyone who wants it.
            took_us: _,
        } => {
            println!("{total}{}", if *capped { "+" } else { "" });
            complain(misread, echo);
        }
        Response::Facets(f) => {
            print!("{}", facets_said(f, at_a_terminal(), in_colour()));
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
                // The saving first: a header line per group, then its paths.
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
            // Two numbers: what was read and compared, then what merely shares
            // a size — 18.29 GiB against 39.36 GiB on this disk.
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
            // Never dropped: a partial answer that looks complete gets files
            // deleted on a guess.
            if *unconfirmed > 0 {
                eprintln!(
                    "{unconfirmed} {}",
                    t("groups were not confirmed: raise --budget-mb")
                );
            }
        }
        Response::Settings(s) => {
            // Shown, not hidden: when something is remembered wrongly, the
            // first question is what is remembered.
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
        // No CLI command asks for thumbnails; the arm exists because the match
        // is exhaustive, and prints only the paths that now have a picture.
        Response::Thumbnails(made) => {
            for path in &made.ready {
                println!("{path}");
            }
        }
        // Built-in first and marked: only the other list can be edited, and the
        // built-in one does nearly all of the excluding.
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
                    // A switched-off rule is listed and marked, not hidden.
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
            print!(
                "{}",
                if at_a_terminal() {
                    usage_block(u, in_colour(), terminal_width())
                } else {
                    usage_table(u)
                }
            );
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
            // The colouring is meant for a search box; a terminal has colours
            // too, and printing it here proves the spans line up.
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
        // An export never arrives here; `run_export` in `main.rs` writes its
        // pieces as they come. Named rather than covered by a `_`, so the next
        // response variant added has to be given a look here.
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

/// The age of a directory's bytes as one bar: six bands — today, this week, this
/// month, six months, this year, older — drawn as shares of the total.
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

/// Twenty cells: wide enough to tell a third from a half, narrow enough to
/// leave a name beside it on eighty columns. The age strip and every folder bar.
const BAR: usize = 20;
/// The kind bar has a line to itself, so it is twice that.
const WIDE: usize = 40;
/// The size column, right-aligned, in the block and in the table alike.
const SIZE: usize = 10;
/// Three spaces before the age strip and three after it — the legend's column.
const GUTTER: usize = 3;
/// The six age bands, in the order the colours run — the window's own list.
const BANDS: [&str; scour_core::AGE_BANDS] = [
    "today",
    "this week",
    "this month",
    "six months",
    "this year",
    "older",
];

/// Is a person watching? The picture is drawn only then. Into a pipe every
/// command prints what it printed before, because that is what is scripted
/// against — the same rule [`paint`] follows for colour.
fn at_a_terminal() -> bool {
    std::io::IsTerminal::is_terminal(&std::io::stdout())
}

/// Colour on top of that, and `NO_COLOR` takes it off again.
fn in_colour() -> bool {
    at_a_terminal() && std::env::var_os("NO_COLOR").is_none()
}

/// How wide the terminal is, for the one line that must not wrap. Eighty when
/// nothing answers — a pipe, a file, a `tty` that has no size.
#[cfg(unix)]
fn terminal_width() -> usize {
    const ASSUMED: usize = 80;
    // SAFETY: `ws` is plain data the call fills; the ioctl reads only the
    // descriptor and writes only into it.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let asked = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) };
    if asked == 0 && ws.ws_col > 0 {
        ws.ws_col as usize
    } else {
        ASSUMED
    }
}

#[cfg(not(unix))]
fn terminal_width() -> usize {
    80
}

/// A run of cells in one colour: truecolor, reset after. The palette is the
/// windows', so a band is the same colour wherever it is drawn.
fn ink(run: &str, c: scour_ui::Rgba) -> String {
    format!("\x1b[38;2;{};{};{}m{run}\x1b[0m", c.r, c.g, c.b)
}

/// A share as the language writes it: `81,5%` in Turkish, `81.5%` in English.
fn percent(share: f64) -> String {
    let said = format!("{share:.1}%");
    match scour_ui::format::decimal_mark(catalogue().language()) {
        '.' => said,
        mark => said.replace('.', &mark.to_string()),
    }
}

/// A count a person reads: `2.124.324`, grouped the way the language groups.
fn counted(n: u64) -> String {
    scour_ui::format::grouped(n, scour_ui::format::group_mark(catalogue().language()))
}

/// A part of a whole, and zero of nothing.
fn ratio(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 / whole as f64
    }
}

/// The scope's bytes by age: twenty cells, then the bands worth a word, as many
/// as `width` holds. `None` when nothing has an age — an empty strip says less
/// than no strip.
fn age_strip(age: &[u64; scour_core::AGE_BANDS], colour: bool, width: usize) -> Option<String> {
    if age.iter().all(|&b| b == 0) {
        return None;
    }
    let mut strip = String::new();
    // A band too small for a cell is skipped rather than painted: an escape
    // around nothing is two escapes in the stream and no colour on the screen.
    for (i, cells) in segment_cells(age, BAR).iter().enumerate() {
        let run: String = std::iter::repeat_n('▇', *cells).collect();
        if run.is_empty() {
            continue;
        }
        strip.push_str(&if colour {
            ink(&run, scour_ui::DARK.t[i])
        } else {
            run
        });
    }
    // A band under one percent keeps its cells and loses its number: six
    // figures in a row is a table, and this is a caption.
    let mut named: Vec<(usize, f64)> = shares(age, 0)
        .into_iter()
        .enumerate()
        .filter(|(_, share)| *share >= 1.0)
        .collect();
    let said = |named: &[(usize, f64)]| {
        named
            .iter()
            .map(|(band, share)| format!("{} {share:.0}", t(BANDS[*band])))
            .collect::<Vec<_>>()
            .join(" · ")
    };
    // The line never wraps: a legend that runs onto a second line reads as a
    // row of the list under it. The smallest band goes first — it is the one
    // the strip already shows least of.
    let mut legend = said(&named);
    while !named.is_empty() && GUTTER + BAR + GUTTER + legend.chars().count() > width {
        let smallest = named
            .iter()
            .enumerate()
            .min_by_key(|(_, (band, _))| (age[*band], std::cmp::Reverse(*band)))
            .map(|(at, _)| at)
            .expect("a band to drop");
        named.remove(smallest);
        legend = said(&named);
    }
    Some(format!("{0}{strip}{0}{legend}\n", " ".repeat(GUTTER)))
}

/// What `scour du` draws for a person: a headline, the scope's bytes by age,
/// then a bar a folder against the heaviest of them. The shares are
/// `scour_chart`'s, so this and the window say the same percentage.
fn usage_block(u: &scour_core::UsageResponse, colour: bool, width: usize) -> String {
    let mut out = format!(
        "    {:>SIZE$}   {} {}   {}\n",
        format_size(u.root.bytes, BINARY),
        counted(u.root.files),
        t("files"),
        u.root.path
    );
    if let Some(strip) = age_strip(&u.root.age, colour, width) {
        out.push_str(&strip);
    }
    out.push('\n');

    // What the named folders leave: the files in the scope itself and the
    // folders cut off the end of the list. It is in the shares, not in a row.
    let mut weights: Vec<u64> = u.children.iter().map(|c| c.bytes).collect();
    let named: u64 = weights.iter().sum();
    weights.push(u.root.bytes.saturating_sub(named));
    let said: Vec<String> = shares(&weights, 1)
        .iter()
        .take(u.children.len())
        .map(|share| percent(*share))
        .collect();
    // `81,5%` is five; only a folder that is the whole scope needs a sixth.
    let column = said
        .iter()
        .map(|s| s.chars().count())
        .max()
        .unwrap_or(5)
        .max(5);
    let heaviest = u.children.iter().map(|c| c.bytes).max().unwrap_or(0);
    for (c, share) in u.children.iter().zip(&said) {
        let drawn = bar(ratio(c.bytes, heaviest), BAR);
        let cells = drawn.trim_end_matches(' ');
        out.push_str(&format!(
            "    {:>SIZE$}  {share:>column$}   {}{}  {}\n",
            format_size(c.bytes, BINARY),
            if colour {
                ink(cells, scour_ui::DARK.k[1])
            } else {
                cells.to_owned()
            },
            " ".repeat(BAR - cells.chars().count()),
            scour_ui::path::leaf(&c.path)
        ));
    }
    if u.child_count as usize > u.children.len() {
        // The count ends where the shares end, which is where the eye already
        // is; the `…` stands in for the rows that are not there.
        out.push_str(&format!(
            "{:>8}{:>rest$} / {} {}\n",
            "…",
            u.children.len(),
            u.child_count,
            t("folders, largest first"),
            rest = 4 + SIZE + 2 + column - 8
        ));
    }
    // A total that disagrees with `du -l` for an unstated reason reads as a bug.
    out.push_str(&t("a hard-linked file is counted once, like du"));
    out.push('\n');
    out
}

/// The same reply as columns, for whatever is reading the pipe. Unchanged, and
/// meant to stay so: a script cannot be asked to follow a redesign.
fn usage_table(u: &scour_core::UsageResponse) -> String {
    let mut out = format!(
        "{:>10}  {:>10}  {:>9}  {}\n",
        t("size"),
        t("on disk"),
        t("files"),
        u.root.path
    );
    let mut row = |d: &scour_core::DirUsage| {
        out.push_str(&format!(
            "{:>10}  {:>10}  {:>9}  {}  {}\n",
            format_size(d.bytes, BINARY),
            format_size(d.disk, BINARY),
            d.files,
            ages(d),
            d.path
        ));
    };
    row(&u.root);
    for c in &u.children {
        row(c);
    }
    if u.child_count as usize > u.children.len() {
        out.push_str(&format!(
            "{} / {} {}\n",
            u.children.len(),
            u.child_count,
            t("folders, largest first")
        ));
    }
    out.push_str(&t("a hard-linked file is counted once, like du"));
    out.push('\n');
    out
}

/// The grouped counts: a line of the whole bar for a person, then the rows.
/// `block` is what a terminal gets; a pipe gets the same rows it always got.
fn facets_said(f: &scour_core::FacetResponse, block: bool, colour: bool) -> String {
    // Only a kind facet is translated: its key is a token a rail turns into
    // `kind:build`. An extension can be spelled the same way (`ext:bin`), so
    // `by` is what decides.
    let shown: Vec<String> = f
        .facets
        .iter()
        .map(|x| match f.by {
            FacetBy::Kind => facet_word(&x.key),
            _ => x.key.clone(),
        })
        .collect();
    let width = shown.iter().map(|s| s.chars().count()).max().unwrap_or(0);
    let counts: Vec<u64> = f.facets.iter().map(|x| x.count).collect();
    let mut out = String::new();
    let said: Vec<String> = if block {
        if let Some(line) = facet_bar(&counts, colour) {
            out.push_str(&line);
        }
        shares(&counts, 1).iter().map(|s| percent(*s)).collect()
    } else {
        Vec::new()
    };
    let column = said
        .iter()
        .map(|s| s.chars().count())
        .max()
        .unwrap_or(5)
        .max(5);
    for (i, (key, x)) in shown.iter().zip(&f.facets).enumerate() {
        match said.get(i) {
            Some(share) => out.push_str(&format!(
                "{key:<width$}  {:>10}  {share:>column$}\n",
                x.count
            )),
            None => out.push_str(&format!("{key:<width$}  {:>10}\n", x.count)),
        }
    }
    if f.capped {
        out.push_str(&t("counts are a lower bound: the scan hit its cap"));
        out.push('\n');
    }
    out
}

/// One bar cut into the groups, largest first: six take the ring's six steps
/// and the rest share the colour the ring gives the rest.
fn facet_bar(counts: &[u64], colour: bool) -> Option<String> {
    if counts.iter().all(|&c| c == 0) {
        return None;
    }
    let ranked = fold(counts.iter().copied().enumerate().collect(), |x| x.1, 6);
    let mut step = vec![scour_ui::DARK.kx; counts.len()];
    for (i, (which, _)) in ranked.kept.iter().enumerate() {
        step[*which] = scour_ui::DARK.k[i];
    }
    let mut out = String::new();
    for (i, cells) in segment_cells(counts, WIDE).iter().enumerate() {
        let run: String = std::iter::repeat_n('▓', *cells).collect();
        if run.is_empty() {
            continue;
        }
        out.push_str(&if colour { ink(&run, step[i]) } else { run });
    }
    out.push('\n');
    Some(out)
}

/// The word for a kind, through `Kind::msgid()` rather than a second table here:
/// every face has to name the kinds the same way.
fn kind_tag(k: Kind) -> String {
    t(k.msgid())
}

/// The word for a `kind:` facet key. An unknown token is printed as it arrived: a
/// newer service can send a kind this build has never heard of.
fn facet_word(token: &str) -> String {
    match Kind::from_name(token) {
        Some([k]) => kind_tag(*k),
        _ => token.to_owned(),
    }
}

/// `YYYY-MM-DD HH:MM` in the local zone, or a dash for a time nothing set. The
/// shape is [`scour_ui::format::stamp`]'s, so every face prints one listing.
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
pub fn mcp_config(client: crate::McpClient) -> Result<()> {
    use crate::McpClient::*;
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("scour-mcp")))
        .filter(|p| p.exists())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "scour-mcp".to_owned());
    let json = format!(
        "{{\n  \"mcpServers\": {{\n    \"scour\": {{\n      \"command\": \"{exe}\",\n      \"args\": []\n    }}\n  }}\n}}"
    );
    // Where each client reads its servers from, and the shape it wants.
    let (snippet, goes) = match client {
        ClaudeDesktop => (
            json,
            "~/.config/Claude/claude_desktop_config.json (Linux) · ~/Library/Application Support/Claude/claude_desktop_config.json (macOS) · %APPDATA%\\Claude\\claude_desktop_config.json (Windows)",
        ),
        ClaudeCode => (
            format!("claude mcp add scour -- {exe}"),
            "one command in a terminal; or the claude-desktop JSON in .mcp.json at the project root",
        ),
        Codex => (
            format!("[mcp_servers.scour]\ncommand = \"{exe}\""),
            "~/.codex/config.toml",
        ),
        Cursor => (
            json,
            "~/.cursor/mcp.json, or .cursor/mcp.json in the project",
        ),
        Gemini => (json, "~/.gemini/settings.json"),
        Vscode => (
            format!(
                "{{\n  \"servers\": {{\n    \"scour\": {{\n      \"type\": \"stdio\",\n      \"command\": \"{exe}\"\n    }}\n  }}\n}}"
            ),
            ".vscode/mcp.json in the project",
        ),
    };
    println!("{snippet}");
    eprintln!("\n{} {goes}", t("Goes in:"));
    eprintln!(
        "{}",
        t("Start `scourd` first — the MCP server is a client of it, like this one.")
    );
    Ok(())
}

/// What `scour hotkey` was asked to do.
pub enum Deed<'a> {
    /// Say what the key is now.
    Show,
    Set(&'a str),
    Clear,
}

/// Everything one `hotkey` run prints, and what it leaves with: 0 done, 1 a
/// tool refused, 2 not a key. Built rather than printed, so the three states
/// can be read back in a test.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Said {
    pub out: String,
    pub err: String,
    pub code: i32,
}

/// The desktop key that opens Scour. Reaches no service: `gsettings` or
/// `kwriteconfig` is asked directly, through `scour_hotkey`.
pub fn hotkey(deed: Deed<'_>, json: bool) -> i32 {
    let said = hotkey_said(&scour_hotkey::Hotkey::detect(), deed, json);
    print!("{}", said.out);
    eprint!("{}", said.err);
    said.code
}

/// What `scour-hotkey` says about a combination, in the reader's language. The
/// English is the key, so a variant with no entry comes out as itself.
fn key_error(e: &scour_hotkey::KeyError) -> String {
    match e {
        scour_hotkey::KeyError::UnknownModifier(m) => t("not a modifier: {m}").replace("{m}", m),
        other => t(&other.to_string()),
    }
}

/// The same for what the desktop refused. [`scour_hotkey::Error::Tool`] carries
/// the tool's own words, which no catalogue can hold.
fn tool_error(e: &scour_hotkey::Error) -> String {
    match e {
        scour_hotkey::Error::Tool(why) => why.clone(),
        other => t(&other.to_string()),
    }
}

/// GNOME and KDE are names, not words, so none of these is translated. The
/// `--json` spelling is this one in lower case: one table, two readers.
fn desktop_word(d: scour_hotkey::Desktop) -> &'static str {
    match d {
        scour_hotkey::Desktop::Gnome => "GNOME",
        scour_hotkey::Desktop::Kde => "KDE",
        scour_hotkey::Desktop::Flatpak => "Flatpak",
        scour_hotkey::Desktop::Other => "other",
    }
}

fn hotkey_said(hk: &scour_hotkey::Hotkey, deed: Deed<'_>, json: bool) -> Said {
    use scour_hotkey::{Applies, Error, Key, Status};
    let mut said = Said::default();
    // The combination is read before the desktop is touched: a misspelling
    // must not leave half an entry behind.
    let wanted = match &deed {
        Deed::Set(text) => match Key::parse(text) {
            Ok(key) => Some(key),
            Err(e) => {
                said.err = format!("{}\n", key_error(&e));
                said.code = 2;
                return said;
            }
        },
        _ => None,
    };
    let mut applies: Option<Applies> = None;
    let did = match (&deed, &wanted) {
        (Deed::Set(_), Some(key)) => hk.bind(key).map(|a| applies = Some(a)),
        (Deed::Clear, _) => hk.clear(),
        _ => Ok(()),
    };
    // Asked afterwards either way: the answer is what the desktop says the key
    // is now, not what was written at it.
    let state = hk.status();
    if did.is_err() || state.is_err() {
        said.code = 1;
    }
    let key = match &state {
        Ok(Status::Bound(k)) => Some(k.to_string()),
        _ => None,
    };
    if json {
        let value = serde_json::json!({
            "desktop": desktop_word(hk.desktop()).to_ascii_lowercase(),
            "can_bind": hk.can_bind(),
            "key": key,
            "command": hk.command(),
            "applies": applies.map(|a| match a {
                Applies::Now => "now",
                Applies::AfterLogin => "after-login",
            }),
        });
        said.out = format!("{value:#}\n");
    } else {
        match &state {
            // Nothing to show but what to do by hand, and what to bind it to.
            Ok(Status::CannotBind { command }) => {
                said.out = format!(
                    "{}{}\n{}\n  {command}\n",
                    label("desktop"),
                    desktop_word(hk.desktop()),
                    t("This desktop cannot be bound from here. Bind a key of your choice to:"),
                );
            }
            Ok(_) => {
                said.out = format!(
                    "{}{}\n{}{}\n{}{}\n",
                    label("desktop"),
                    desktop_word(hk.desktop()),
                    label("key"),
                    key.unwrap_or_else(|| t("not bound")),
                    label("command"),
                    hk.command(),
                );
            }
            Err(_) => {}
        }
    }
    // The block above has already said it; a second line repeating it is noise.
    let told = !json && matches!(state, Ok(Status::CannotBind { .. }));
    if let Err(e) = &did
        && !(told && matches!(e, Error::Unsupported | Error::Sandboxed))
    {
        said.err.push_str(&format!("{}\n", tool_error(e)));
    }
    if let Err(e) = &state {
        said.err.push_str(&format!("{}\n", tool_error(e)));
    }
    // KDE writes the file; `kglobalaccel` reads it at the next login.
    if applies == Some(Applies::AfterLogin) {
        said.err
            .push_str(&format!("{}\n", t("It takes effect after the next login.")));
    }
    said
}

/// Which languages this build ships, for `scour where`.
fn languages() -> String {
    scour_i18n::LANGUAGES
        .iter()
        .map(|(tag, name)| format!("{tag} {name}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The query, with each run in the colour its role earns. Colours are dropped
/// when the output is not a terminal, so `scour explain | grep` sees plain text.
fn paint(query: &str, spans: &[scour_core::Span]) -> String {
    use scour_core::Role;
    if !at_a_terminal() {
        return query.to_owned();
    }
    let mut out = String::with_capacity(query.len() * 2);
    for s in spans {
        let text = s.of(query);
        let code = match s.role {
            // First, because a term can be both excluded and misread and the
            // misreading is what is worth saying.
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

/// Open another face, or say which one opens. Runs `scour-open`, which owns the
/// list of terminals and is also what the desktop entry runs.
pub fn faces(face: Option<&str>) -> anyhow::Result<()> {
    let dir = std::env::var("XDG_DATA_HOME")
        .unwrap_or_else(|_| format!("{}/.local/share", std::env::var("HOME").unwrap_or_default()));
    let settings = format!("{dir}/scour/state/settings.json");
    let Some(face) = face else {
        // Read, not asked over the socket: the answer must exist when nothing
        // is running.
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

    /// The home the report was drawn from: twelve folders of sixty-three, the
    /// live figures `scour-chart`'s own test measures its shares against. Each
    /// folder's bytes sit in one age band, so the table's strip is readable;
    /// the scope's own six are what the block draws.
    fn home() -> scour_core::UsageResponse {
        const FOLDERS: [(&str, u64, u64); 12] = [
            ("Projeler", 651_571_270_524, 896_648),
            (".config", 31_643_455_926, 98_380),
            (".local", 15_738_588_363, 283_461),
            ("İndirilenler", 14_110_521_835, 52),
            ("Android", 10_863_393_896, 75_921),
            (".AffinityLinux", 10_852_991_366, 27_703),
            (".rustup", 9_242_344_819, 244_494),
            (".android", 8_271_160_692, 123),
            ("vm", 7_898_028_191, 7),
            (".gradle", 7_372_347_728, 56_897),
            (".cargo", 5_889_237_540, 209_217),
            ("pdflab", 4_856_318_717, 20_156),
        ];
        // 14.3, 3.4, 64.8, 15.4, 0.5, 1.7 percent of the whole, to the byte.
        let age = [
            114_251_175_399,
            27_164_615_130,
            516_926_646_736,
            123_039_727_353,
            3_994_796_343,
            13_582_307_565,
        ];
        scour_core::UsageResponse {
            root: scour_core::DirUsage {
                path: "/home/hasan".into(),
                bytes: 798_959_268_526,
                disk: 798_959_268_526,
                files: 2_124_324,
                age,
            },
            children: FOLDERS
                .iter()
                .map(|(name, bytes, files)| scour_core::DirUsage {
                    path: format!("/home/hasan/{name}"),
                    bytes: *bytes,
                    disk: *bytes,
                    files: *files,
                    age: [*bytes, 0, 0, 0, 0, 0],
                })
                .collect(),
            child_count: 63,
            took_us: 821_000,
        }
    }

    /// The block a terminal gets, to the column: the mock-up's picture, in
    /// block characters. Words come through the catalogue, so this reads the
    /// same in any language; the columns are the point and they are literal.
    /// A hundred and twenty columns, where all six bands fit — eighty is the
    /// test below.
    #[test]
    fn the_block_a_terminal_gets_is_the_report_drawn_in_cells() {
        let d = scour_ui::format::decimal_mark(catalogue().language());
        let g = scour_ui::format::group_mark(catalogue().language());
        // Every band is over a percent here, so every band is named.
        let legend = [
            ("today", 14),
            ("this week", 3),
            ("this month", 65),
            ("six months", 15),
            ("this year", 1),
            ("older", 2),
        ]
        .iter()
        .map(|(band, share)| format!("{} {share}", t(band)))
        .collect::<Vec<_>>()
        .join(" · ");
        let expect = [
            format!(
                "       744 GiB   2{g}124{g}324 {}   /home/hasan",
                t("files")
            ),
            // Twenty cells: 3 today, 1 this week, 13 this month, 3 six months.
            format!("   {}   {legend}", "▇".repeat(20)),
            String::new(),
            format!("       607 GiB  81{d}5%   {}  Projeler", "█".repeat(20)),
            format!("      29.5 GiB   4{d}0%   █                     .config"),
            format!("      14.7 GiB   2{d}0%   ▌                     .local"),
            format!("      13.1 GiB   1{d}8%   ▍                     İndirilenler"),
            format!("      10.1 GiB   1{d}4%   ▍                     Android"),
            format!("      10.1 GiB   1{d}4%   ▍                     .AffinityLinux"),
            format!("      8.61 GiB   1{d}1%   ▎                     .rustup"),
            format!("      7.70 GiB   1{d}0%   ▎                     .android"),
            format!("      7.36 GiB   1{d}0%   ▎                     vm"),
            format!("      6.87 GiB   0{d}9%   ▎                     .gradle"),
            format!("      5.48 GiB   0{d}7%   ▏                     .cargo"),
            format!("      4.52 GiB   0{d}6%   ▏                     pdflab"),
            format!("       …           12 / 63 {}", t("folders, largest first")),
            t("a hard-linked file is counted once, like du"),
            String::new(),
        ]
        .join("\n");
        assert_eq!(usage_block(&home(), false, 120), expect);
    }

    /// Into a pipe: the table that was there before any of this, byte for byte.
    #[test]
    fn a_pipe_gets_the_table_it_always_got() {
        let expect = [
            format!(
                "{:>10}  {:>10}  {:>9}  /home/hasan",
                t("size"),
                t("on disk"),
                t("files")
            ),
            "   744 GiB     744 GiB    2124324  ▁▁▇▁▁▁  /home/hasan".into(),
            "   607 GiB     607 GiB     896648  █       /home/hasan/Projeler".into(),
            "  29.5 GiB    29.5 GiB      98380  █       /home/hasan/.config".into(),
            "  14.7 GiB    14.7 GiB     283461  █       /home/hasan/.local".into(),
            "  13.1 GiB    13.1 GiB         52  █       /home/hasan/İndirilenler".into(),
            "  10.1 GiB    10.1 GiB      75921  █       /home/hasan/Android".into(),
            "  10.1 GiB    10.1 GiB      27703  █       /home/hasan/.AffinityLinux".into(),
            "  8.61 GiB    8.61 GiB     244494  █       /home/hasan/.rustup".into(),
            "  7.70 GiB    7.70 GiB        123  █       /home/hasan/.android".into(),
            "  7.36 GiB    7.36 GiB          7  █       /home/hasan/vm".into(),
            "  6.87 GiB    6.87 GiB      56897  █       /home/hasan/.gradle".into(),
            "  5.48 GiB    5.48 GiB     209217  █       /home/hasan/.cargo".into(),
            "  4.52 GiB    4.52 GiB      20156  █       /home/hasan/pdflab".into(),
            format!("12 / 63 {}", t("folders, largest first")),
            t("a hard-linked file is counted once, like du"),
            String::new(),
        ]
        .join("\n");
        assert_eq!(usage_table(&home()), expect);
    }

    /// A band too small to round to a percent keeps its cells and loses its
    /// number, and the strip is twenty cells whatever the split.
    #[test]
    fn the_legend_names_only_the_bands_worth_a_number() {
        let age = [700, 299, 1, 0, 0, 0];
        let said = age_strip(&age, false, 80).expect("bytes have an age");
        assert_eq!(
            said,
            format!(
                "   {}   {} 70 · {} 30\n",
                "▇".repeat(20),
                t("today"),
                t("this week")
            ),
            "the third band is a tenth of a percent"
        );
        assert_eq!(age_strip(&[0; 6], false, 80), None, "nothing to draw");
    }

    /// The line never wraps. Six bands over a percent fit in a hundred and
    /// twenty columns; in eighty they do not, and the smallest go — one at a
    /// time, until what is left fits and the next one back would not.
    #[test]
    fn a_legend_wider_than_the_terminal_loses_its_smallest_bands() {
        let age = home().root.age;
        // What is written after the strip, at that width.
        let legend = |width: usize| {
            let said = age_strip(&age, false, width).expect("bytes have an age");
            let line = said.trim_end_matches('\n');
            assert!(
                line.chars().count() <= width,
                "wrapped at {width}: {line:?}"
            );
            line.chars().skip(GUTTER + BAR + GUTTER).collect::<String>()
        };

        let all = legend(120);
        let named = |legend: &str| -> Vec<usize> {
            legend
                .split(" · ")
                .filter(|part| !part.is_empty())
                .map(|part| {
                    BANDS
                        .iter()
                        .position(|band| part.starts_with(&t(band)))
                        .unwrap_or_else(|| panic!("no band in {part:?}"))
                })
                .collect()
        };
        assert_eq!(named(&all), vec![0, 1, 2, 3, 4, 5], "all six at 120");

        let cut = legend(80);
        let kept = named(&cut);
        assert!(kept.len() < 6, "nothing was dropped at 80: {cut:?}");
        assert!(kept.windows(2).all(|w| w[0] < w[1]), "still in age order");
        // What went is smaller than anything that stayed, and the largest of
        // those would not have fitted either.
        let gone: Vec<usize> = (0..6).filter(|b| !kept.contains(b)).collect();
        let smallest_kept = kept.iter().map(|b| age[*b]).min().expect("some kept");
        assert!(
            gone.iter().all(|b| age[*b] <= smallest_kept),
            "a larger band was dropped before a smaller one: {gone:?}"
        );
        let biggest_gone = gone.iter().max_by_key(|b| age[**b]).expect("some gone");
        let one_more = format!(" · {} 1", t(BANDS[*biggest_gone]));
        assert!(
            GUTTER + BAR + GUTTER + cut.chars().count() + one_more.chars().count() > 80,
            "one more band would have fitted: {cut:?}"
        );
    }

    /// Colour is a run of cells in the band's own colour, and `false` is the
    /// same picture with no escape in it at all.
    #[test]
    fn the_cells_are_painted_in_the_bands_own_colours() {
        let age = [700, 299, 1, 0, 0, 0];
        let painted = age_strip(&age, true, 80).expect("bytes have an age");
        let c = scour_ui::DARK.t[0];
        assert!(
            painted.contains(&format!(
                "\x1b[38;2;{};{};{}m{}\x1b[0m",
                c.r,
                c.g,
                c.b,
                "▇".repeat(14)
            )),
            "today's cells in today's colour: {painted:?}"
        );
        assert!(
            !age_strip(&age, false, 80).expect("drawn").contains('\x1b'),
            "plain means plain"
        );
    }

    /// The groups, with the bar a terminal gets and the percentage beside the
    /// count. The keys are extensions, so no word here is translated.
    fn kinds() -> scour_core::FacetResponse {
        let keys = ["rs", "md", "png", "pdf", "txt", "zip", "ico"];
        let counts = [75_090u64, 49_295, 41_307, 16_128, 5_248, 4_786, 8_146];
        scour_core::FacetResponse {
            facets: keys
                .iter()
                .zip(counts)
                .map(|(key, count)| scour_core::Facet {
                    key: (*key).to_owned(),
                    count,
                })
                .collect(),
            by: FacetBy::Ext { top: 10 },
            total: counts.iter().sum(),
            ..Default::default()
        }
    }

    #[test]
    fn the_groups_get_a_bar_and_a_share_at_a_terminal() {
        let d = scour_ui::format::decimal_mark(catalogue().language());
        let expect = [
            // Forty cells: 15, 10, 8, 3, 1, 1, 2, in the order they are listed.
            "▓".repeat(40),
            format!("rs        75090  37{d}5%"),
            format!("md        49295  24{d}6%"),
            format!("png       41307  20{d}7%"),
            format!("pdf       16128   8{d}1%"),
            format!("txt        5248   2{d}6%"),
            format!("zip        4786   2{d}4%"),
            format!("ico        8146   4{d}1%"),
            String::new(),
        ]
        .join("\n");
        assert_eq!(facets_said(&kinds(), true, false), expect);
    }

    /// The same reply into a pipe: the two columns it printed before.
    #[test]
    fn a_pipe_gets_the_two_columns_it_always_got() {
        let expect = [
            "rs        75090",
            "md        49295",
            "png       41307",
            "pdf       16128",
            "txt        5248",
            "zip        4786",
            "ico        8146",
            "",
        ]
        .join("\n");
        assert_eq!(facets_said(&kinds(), false, false), expect);
    }

    /// The six largest take the ring's six steps; whatever is left over takes
    /// the colour the ring gives the rest — `zip` here, the smallest of seven.
    #[test]
    fn the_six_largest_groups_take_the_rings_six_steps() {
        let counts: Vec<u64> = kinds().facets.iter().map(|x| x.count).collect();
        let said = facet_bar(&counts, true).expect("something to draw");
        let rest = scour_ui::DARK.kx;
        assert!(
            said.contains(&format!(
                "\x1b[38;2;{};{};{}m▓\x1b[0m",
                rest.r, rest.g, rest.b
            )),
            "the seventh is the rest: {said:?}"
        );
        assert_eq!(facet_bar(&[0, 0], false), None, "nothing to draw");
    }

    /// In the person's zone: the format is pinned through the pure half, the
    /// zone through the same call the code uses.
    #[test]
    fn timestamps_read_as_dates() {
        use scour_ui::format::{local_offset, stamp_at};
        assert_eq!(stamp(0), "—", "an unknown time is not 1970");
        let at = 1_769_817_600;
        assert_eq!(stamp_at(at, 0), "2026-01-31 00:00", "the shape, zone-free");
        assert_eq!(stamp(at), stamp_at(at, local_offset(at)));
        assert_eq!(
            stamp(at + 3_661),
            stamp_at(at + 3_661, local_offset(at + 3_661))
        );
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

    /// Every string this file asks for exists in the Turkish catalogue; at run
    /// time a missing one falls back to English rather than failing.
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
            "desktop",
            "key",
            "command",
            "not bound",
            "This desktop cannot be bound from here. Bind a key of your choice to:",
            "It takes effect after the next login.",
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
            "files",
            "folders, largest first",
            "a hard-linked file is counted once, like du",
            "counts are a lower bound: the scan hit its cap",
            "today",
            "this week",
            "this month",
            "six months",
            "this year",
            "older",
            "was searched for as text",
            "a rebuild would speed searches up",
            "The index is empty. Run `scour rescan`.",
            "needs document contents, which this index may not have",
            "Start `scourd` first — the MCP server is a client of it, like this one.",
        ];
        let missing: Vec<&str> = used.iter().copied().filter(|m| c.get(m) == **m).collect();
        assert!(missing.is_empty(), "untranslated: {missing:?}");
    }

    /// Writing a script and starting one are one race: a fork on another test's
    /// thread inherits the file still open for writing, and the exec then says
    /// `Text file busy`. The tests that start a process take turns.
    #[cfg(unix)]
    fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A `gsettings` keeping its settings in a file beside itself, as
    /// `scour-hotkey`'s own tests use. Never this machine's desktop.
    #[cfg(unix)]
    fn stub_gsettings(dir: &std::path::Path) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("gsettings");
        std::fs::write(
            &script,
            r#"#!/bin/bash
db="$(dirname "$0")/db"; touch "$db"
case "$1" in
  get)
    line=$(grep -F -- "$2 $3=" "$db" | head -1)
    if [ -n "$line" ]; then printf '%s\n' "${line#*=}"
    elif [ "$3" = custom-keybindings ]; then echo "@as []"
    else echo "''"; fi ;;
  set)
    grep -v -F -- "$2 $3=" "$db" > "$db.new" || true
    printf '%s %s=%s\n' "$2" "$3" "$4" >> "$db.new"; mv "$db.new" "$db" ;;
  reset-recursively)
    grep -v -F -- "$2 " "$db" > "$db.new" || true; mv "$db.new" "$db" ;;
  *) echo "stub: $*" >&2; exit 1 ;;
esac
"#,
        )
        .expect("write stub");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        script
    }

    #[cfg(unix)]
    fn gnome(dir: &std::path::Path) -> scour_hotkey::Hotkey {
        scour_hotkey::Hotkey::new(scour_hotkey::Tools {
            gsettings: Some(stub_gsettings(dir)),
            gnome_keys: true,
            desktop_var: "GNOME".into(),
            ..Default::default()
        })
    }

    /// Unbound, bound, and bound-then-cleared, in both forms.
    #[cfg(unix)]
    #[test]
    fn the_key_reads_the_same_way_before_and_after_it_is_set() {
        let _turn = one_at_a_time();
        let dir = tempfile::tempdir().expect("tempdir");
        let hk = gnome(dir.path());

        // Built with the same helpers, not spelled out: this process speaks
        // whatever the machine's locale is, and the shape is what is being
        // checked.
        let block = |key: String| {
            format!(
                "{}GNOME\n{}{key}\n{}scour-open\n",
                label("desktop"),
                label("key"),
                label("command")
            )
        };

        let said = hotkey_said(&hk, Deed::Show, false);
        assert_eq!(said.code, 0);
        assert_eq!(said.out, block(t("not bound")));

        let said = hotkey_said(&hk, Deed::Set("Super+F"), false);
        assert_eq!(said.code, 0);
        assert_eq!(
            said.out,
            block("super+f".into()),
            "the combination as this crate spells it, not as it was typed"
        );
        // GNOME's is in force at once, so there is nothing to add.
        assert_eq!(said.err, "");
        assert_eq!(hotkey_said(&hk, Deed::Show, false).out, said.out);

        let said = hotkey_said(&hk, Deed::Clear, false);
        assert_eq!(said.code, 0);
        assert_eq!(said.out, block(t("not bound")));
    }

    /// `--json` is the same five facts without the wording.
    #[cfg(unix)]
    #[test]
    fn the_json_carries_what_a_script_needs() {
        let _turn = one_at_a_time();
        let dir = tempfile::tempdir().expect("tempdir");
        let hk = gnome(dir.path());
        let said = hotkey_said(&hk, Deed::Set("ctrl+alt+s"), true);
        let value: serde_json::Value = serde_json::from_str(&said.out).expect("json");
        assert_eq!(value["desktop"], "gnome");
        assert_eq!(value["can_bind"], true);
        assert_eq!(value["key"], "ctrl+alt+s");
        assert_eq!(value["command"], "scour-open");
        assert_eq!(value["applies"], "now");
        let said = hotkey_said(&hk, Deed::Clear, true);
        let value: serde_json::Value = serde_json::from_str(&said.out).expect("json");
        assert_eq!(value["key"], serde_json::Value::Null);
        assert_eq!(value["applies"], serde_json::Value::Null);
    }

    #[test]
    fn a_combination_that_is_not_one_is_refused_before_anything_is_written() {
        let hk = scour_hotkey::Hotkey::new(scour_hotkey::Tools::default());
        let said = hotkey_said(&hk, Deed::Set("hyper+f"), false);
        assert_eq!(said.code, 2, "not a tool failure");
        assert_eq!(
            said.err,
            format!("{}\n", t("not a modifier: {m}").replace("{m}", "hyper")),
            "the crate's own words, and the value it refused"
        );
        assert!(said.out.is_empty(), "and nothing was said about the state");
    }

    /// A desktop this program cannot write to says what to bind by hand, and
    /// `set` leaves with a code that stops a script.
    #[test]
    fn a_desktop_that_cannot_be_bound_names_the_command_instead() {
        let hk = scour_hotkey::Hotkey::new(scour_hotkey::Tools {
            desktop_var: "XFCE".into(),
            ..Default::default()
        });
        let said = hotkey_said(&hk, Deed::Show, false);
        assert_eq!(said.code, 0, "being told is not a failure");
        assert_eq!(
            said.out,
            format!(
                "{}other\n{}\n  scour-open\n",
                label("desktop"),
                t("This desktop cannot be bound from here. Bind a key of your choice to:")
            )
        );
        assert_eq!(said.err, "");

        for deed in [Deed::Set("super+f"), Deed::Clear] {
            let said = hotkey_said(&hk, deed, false);
            assert_eq!(said.code, 1, "a script has to be able to see this");
            assert!(said.out.contains("scour-open"), "{}", said.out);
            assert_eq!(said.err, "", "the sentence is not said twice");
        }
        // In JSON the sentence is not printed, so the reason goes to stderr.
        let said = hotkey_said(&hk, Deed::Set("super+f"), true);
        assert_eq!(said.code, 1);
        assert_eq!(
            said.err,
            format!("{}\n", t("this desktop cannot be bound from here"))
        );
    }
}
