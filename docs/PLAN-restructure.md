# Restructuring plan

## Where it stands (2026-11-24)

Carried out the same day, one commit a step, every step checked; the record
is `git log 9314ac9^..`. The decisions of §6 were taken as recommended,
except the first (below).

| phase | done | left, and why |
|---|---|---|
| 0 the rule as a test | `apps/scourd/tests/layers.rs` | — |
| 1 big files below the socket | tests out of 37 files; `lib.rs` is `mod`/`pub use` in nine crates; `fanotify.rs`, `engine.rs`, `search.rs`, `index.rs` by subject, the trait block's five longest methods a call each | `scour-engine`'s `fn run`: one loop over fifteen mutable locals; cutting it means a struct for them, a change to the service's core rather than a move |
| 2 `scour-extract` | one ZIP reader and XML flattener for grep and the preview; grep measured on it (`docs/MEASUREMENTS.md`) | — |
| 3 `scour-client` | `Pool` (page, MCP), `Lazy` and `boot` (window, terminal), the rail's and the count's wording | each face keeps its own idea of a stale answer (decision 3) |
| 4 the faces | `page.html` in parts; the bridge's routes, the terminal's `App` and `draw.rs`, the service's request match, the window's `main()` and `apply()` by section; `main.slint` 8,887 → 5,374 lines | the header and the list in `MainWindow` are tied by element ids (`list.viewport-y` read by the window's own properties, the query input focused from elsewhere): taking them out turns those into properties |
| 5 small repeats | `scour_ui::path::home_form`; the terminal's msgid test reads every file (decision 4: it was a hole) | `sort_of`/`sort_key` differ on purpose; the hotkey's wording and the bridge's JSON are different jobs |

**Decision 1 went the other way.** `Extractor` in `scour-core` was deleted, not
reshaped: an implementation would have to live in `scour-extract`, which, like
`scour-grep` and `scour-preview`, depends on nothing of Scour's. Its `Sink` is
the trait now.

**Found on the way, and fixed:** the accents test had failed since `3180b79`
(the brute force was not given the new typo rule); `inflate_whole` panicked
near the end of an archive's budget; the page re-sent a refused request on a
new connection; xlsx sheets with strings in their cells showed nothing.

---

For whoever carries it out. Second draft: the first was drawn from sizes and
dependency lists; this one from the outlines of the files themselves — every
top-level item and its span in the large files, both `link.rs`, the two
container readers, the faces' state structs, `AGENTS.md` and `scripts/check`.
Function bodies were read only where a claim rested on them. Three claims of
the first draft did not survive and are named in §7.

## 0. What is already true

30 crates and 7 apps, and the big cut holds: no face links the engine, the
index or the source, and the leaf crates depend on nothing of Scour's. This
plan does not redraw that. `AGENTS.md` is the contract and already says what
a file should be: **`lib.rs` is `mod` and `pub use` only, about 300 lines a
file, 40 a function.** The work is mostly making the code meet its own rule,
plus two new crates and one trait.

What stands in the way is not big *files* but big *items*:

| item | lines |
|---|---|
| `MainWindow` component, `scour-gui/ui/main.slint` 1574–8887 | 7,300 |
| `fn main()`, `scour-gui/src/main.rs` 596–4143 | 3,550 |
| `impl App`, `scour-tui/src/app.rs` 586–2549 | 1,960 |
| `impl Index for NativeIndex`, `index.rs` 2249–3453 | 1,200 |
| `fn apply()`, `scour-gui/src/main.rs` 4683–5798 | 1,115 |
| `impl NativeIndex`, `index.rs` 316–1421 | 1,100 |
| `impl Engine`, `engine.rs` 430–1308 | 880 |
| `fn walk_segment`, `search.rs` 1319–1928 | 610 |
| `fn run`, `engine.rs` 1394–1868 | 475 |
| `fn run`, `scourd/src/handle.rs` 66–527 | 460 |
| the page's one `<script>`, `page.html` 2633–11056 | 8,400 |

A file can be cut between items by moving text. An item can only be cut by
changing code. The plan keeps those two kinds of step apart.

## 1. Rules for the whole job

- **Behaviour does not change** in Phases 0–4. No renames beyond what a move
  forces, no "while I am here".
- One step, one commit; a crate's own tests first (`derle cargo test -p …`),
  `derle bash scripts/check` green before the next phase.
- No new worktree, no subagent rounds. `AGENTS.md` and the comment policy
  hold; a change to the wire, the CLI, the MCP surface or a `scour-core`
  trait changes `AGENTS.md` in the same commit, and a new crate changes
  `docs/ARCHITECTURE.md` §2 and the `PORTABLE` list in `scripts/check`.
- **Things that read source.** Each of these keeps passing while checking
  nothing if code moves out from under it — repoint it in the same commit:
  - `scour-web/src/main.rs:1904` and `:2167`, `include_str!("main.rs")`;
  - `scour-tui/src/app.rs:2830–2836`, a hand-kept list of seven files for
    the msgid test. It already omits `report.rs`, `disks.rs` and `theme.rs`;
    say so to the owner rather than deciding it — and make the list
    something a new file cannot miss (a `build.rs` glob) when it is touched;
  - `scripts/page-script.py`, `apps/scour-web/tests/page.test.cjs`
    (`SCOUR_PAGE` or `../src/page.html`) and `scripts/check`, all of which
    read `page.html` as one file;
  - `scour-gui/build.rs`, `rows.rs` and `main.rs`, which name `main.slint`.
- **Hot code is moved, never reshaped, in this job**: `walk_segment`,
  `reach`, `evaluate`, `compile_match`, `merge_rows` in the index, the
  `fanotify` event loop (`fill`, `parse`, `drain`, `window`), and the grep
  ZIP stream. Any step that touches the files they live in runs
  `scripts/bench` before and after, alternately, per `docs/MEASUREMENTS.md`.

## 2. Target shape

```
faces        scour-web   scour-gui   scour-tui   scour   scour-mcp
                 │
client       scour-client    NEW  a held connection; lanes; who is stale
shared       scour-ui  scour-page  scour-i18n  scour-settings  …   (as now)
transport    scour-proto  scour-ipc                                (as now)
─────────────────────────── one socket ───────────────────────────
service      scourd          names the concrete types, in src/wire.rs
search       scour-engine    Arc<dyn Index>, Box<dyn Source>,
                             (later) Arc<dyn ContentIndex>
tools        scour-grep  scour-preview  scour-dupes  scour-export  …
             └── scour-extract   NEW  containers and text, one reader
storage      scour-index-native      (later) scour-content-index
source       scour-source-fs  scour-watch
contract     scour-core  scour-query
```

Two new crates, not four. There is no `scour-view` (§7).

## 3. Phases

### Phase 0 — the rule as a test

`AGENTS.md` states the layering in a table and `Cargo.toml` in a comment;
nothing checks either. Add a test over `cargo metadata` with the allowed
edges written in it: a face never depends on `scour-engine`,
`scour-index-native`, `scour-source-fs` or `scour-grep`; only `scourd`
depends on `scour-engine` and on a concrete index or source outside
dev-dependencies; a leaf crate (`grep`, `preview`, `thumbs`, `trash`, `clip`,
`drag`, `name`, `openers`, `sendto`, `launch`, `hotkey`, `chart`, `dupes`,
`places`, `volumes`, `settings`) has no `scour-*` dependency. Phases 2 and 3
each edit that list visibly.

### Phase 1 — cuts between items (text moves only)

**1a. Tests out.** The cheapest large win, no risk:

| file | code | tests |
|---|---|---|
| `scour-source-fs/src/fanotify.rs` | 1,367 | 2,050 |
| `scour-index-native/src/index.rs` | 3,453 | 900 |
| `scour-gui/src/main.rs` | 7,296 | 527 |
| `scour-tui/src/app.rs` | 2,557 | 360 |
| `scour-engine/src/engine.rs` | 2,570 | 205 |

Inline tests reach private items, so they go to `foo/tests.rs` under
`#[cfg(test)] mod tests;`, not to `tests/`.

**1b. `lib.rs` is `mod` and `pub use`.** Thirteen crates break the rule;
the two that matter: `scour-proto` (1,162 — `call.rs` for `Call`/`Reply`/
`Outcome`, `request.rs` for the enum and its defaults, `response.rs`,
`grep.rs`, `disks.rs`; `Request::is_mutating` and the round-trip test stay
with what they guard) and `scour-ui` (1,332 — palette, columns, bands,
kinds). Then `scour-settings` (960), `scour-openers`, `scour-dupes`,
`scour-trash`, `scour-page`, `scour-preview`, `scour-sendto`. Re-export
everything, so no caller changes.

**1c. Files cut between items.**

- `fanotify.rs` → `fanotify/handle.rs` (`FsTag`, `handle_of`, `handle_ino`,
  `btrfs_root`), `names.rs` (`NameSlot`, `Node`, `NameArena`, `DirMap`),
  `mounts.rs` (`mounts`, `mountinfo`, `note_mounts`, `cover_mounts`),
  `events.rs` (`Seen`, `fill`, `parse`, `event_path`, `drain`, `window`),
  `watch.rs` (`Sub`, `FanWatch`, `try_start`, the inherited descriptor).
- `engine.rs` → `options.rs` (`EngineOptions`, `Tuning`, `Reading`,
  `Typos`), `prepare.rs` (`Prepare`, `Prepared`, `prepare_loop`), `scan.rs`
  (`scan`, `scan_all`, `scan_lanes`, `LaneWalk`, `PendingWalks`, the retry
  schedule), `sink.rs` (`ToIndex`, `Show`, `Forward`), `run.rs` (`fn run`,
  whole), and `engine.rs` keeping `Shared`, `Engine` and its `impl`.
  A struct's private fields become `pub(super)`; nothing wider.
- `search.rs` → `plan.rs` (`Test`, `Clause`, `Plan`, `compile_*`,
  `evaluate`, block narrowing), `walk.rs` (`walk_segment`, `in_parts`, the
  thread knobs), `order.rs` already exists — the sort-key half
  (`SortValue`, `sort_value*`, `numeric_key`, `zone_*`, `page_order`,
  `narrow`, `sort_hits`) goes beside or into it; *check what `order.rs`
  holds first*.
- `index.rs` → `meta.rs` (`Meta`, `SegRef`, format), `staging.rs`
  (`Pending`, `Flight`, `Landed`, `Building`, tiers, `FoldGate`),
  `reach.rs` (`Candidate`, `Cursor`, `reach`, `Span`, `first_of`,
  `merge_rows`), `commit.rs` (`CommitStamp`, `trace_commit`,
  `sweep_orphans`, allocator trimming), `facets.rs` (`Tally`,
  `extensions_of`), `doomed.rs` (`Doomed`, `spare_unchanged`).

**1d. `impl` blocks cut between methods.** Rust lets one type's `impl`
stand in several modules of its crate, so the three big blocks split
without a line of logic changing: `impl Index for NativeIndex` cannot (a
trait impl is one block) — its long methods become one-line calls into
inherent methods that live in the new files; `impl NativeIndex` and
`impl Engine` split by subject (search, facets, usage, export, duplicates,
maintenance); `impl App` in the terminal by panel, the way `disks.rs` and
`v2.rs` already do it.

Phase 1 leaves every function exactly as long as it was.

### Phase 2 — `scour-extract`

Confirmed: there are two ZIP readers and two XML-to-text strippers.

| | `scour-grep` | `scour-preview` |
|---|---|---|
| central directory | `zip.rs`: `find_eocd`, `parse_cd_bytes`, `parse_local_headers`, `load_file_cd` | `ooxml.rs`: `read_zip_directory` |
| inflate | `flate2`, streamed in 256 KiB windows, entry and archive caps, nesting to depth 2 | `miniz_oxide`, whole entry, 8 MiB cap |
| XML → text | `xml.rs`: `strip_xml`, `udf_cdata` (bytes, a cap) | `append_xml_text`, `decode_entities` (strings, breaks and tabs) |
| knows | which entries are worth reading (`triage`, `classify_archive_entry`) | sheet order, shared strings, slide numbers |

And `scour-core/src/traits/extractor.rs` declares `Extractor` and
`ContentSink` that nothing implements ("Nothing implements this yet").

The crate, a leaf like the two it serves: **the container layer** (find the
directory, list entries, hand an entry's bytes as a stream under stated
caps) and **the text layer** (XML to text into a sink, with the structure
callbacks `ContentSink` already names). `scour-grep` keeps its search over
the stream and its triage; `scour-preview` keeps what it knows about a
spreadsheet. One inflater in the workspace, not two.

Order: build the crate with tests taken from both sides; move
`scour-preview` onto it first (a preview a few milliseconds slower is not a
regression anybody measures); move `scour-grep` last and **only if the
grep numbers in `docs/MEASUREMENTS.md` hold**, alternately run. If they do
not, grep keeps its reader, the crate is still the one place the next
format is added, and the plan says so in `ARCHITECTURE.md`.

The core trait: `extract(&self, input: &mut dyn Read, …)` cannot open a ZIP,
which needs to seek. Change it to what the first real implementer needs and
implement it here, or delete it — the owner's call (§6) — but do not leave
a contract beside a second, different one.

### Phase 3 — `scour-client`

What the read showed: the two `link.rs` share a shape, not a body. `Ask` has
27 variants in the window and 23 in the terminal, about fifteen in common;
`Got` likewise. What is truly written more than once:

1. **A held connection that reconnects**: `scour-web/src/main.rs:491–498`
   and `scour-mcp/src/tools.rs:185–193` — call on the held client, on
   failure open a fresh one and call once more.
2. **A lane**: a thread, a queue, connect lazily, start the service through
   `scour-launch` on the first failure, report down and up once each
   (`spawn_lane` in the window, `serve` in the terminal).
3. **Who is stale.** The window keeps four counters (`search`, `query`,
   `lens`, `grep`) and drops an ask whose number is not the newest *before
   it reaches the service*; the terminal drops queued asks older than the
   newest queued generation. These are **different rules**.
4. **Ask → `Request`**: decisions with a reason in a comment — facets always
   ask for both groups because the scan cap follows the question; the count
   is uncapped; `TYPING_CAP = 1_000` — each written per face.

The crate (`scour-core`, `scour-proto`, `scour-ipc`, `scour-launch`, nothing
else): `Held` for (1); `Lanes<A, G>` for (2), generic over the face's own
`Ask` and `Got`, with the face giving `fn answer(&mut Client, A) -> G` and
a sink; staleness as a small trait on `A` (`fn stamp(&self) ->
Option<(Domain, u64)>`); and the request builders of (4) as plain functions.
Faces keep their enums — forcing one `Ask` on four faces would be the
abstraction that has to be fought later.

Steps: `Held` into the bridge and the MCP server; the request builders;
`Lanes` under the terminal (`--once`/`--press` check it), then the window.
**Point 3 is not a move**: giving both faces one rule changes what one of
them drops. Keep each face's rule as its own implementation of the trait in
this job, and put the question of unifying them to the owner with both
written side by side.

This is also the door for what is not written yet: a plugin host, an AI
helper and a test harness are all clients.

### Phase 4 — cuts through items, in the faces

Not text moves; each step is small, mechanical and checked on screen.

- **`scour-gui`, `fn main()` (3,550 lines).** It is the place every Slint
  callback is bound. One function per area — `wire_list`, `wire_rail`,
  `wire_menu`, `wire_settings`, `wire_report`, `wire_peek`, … — each taking
  the window, the shared state and the link, the way `hotkey::wire`,
  `grep_ui::wire` and `qwire` already do. Move a group of closures at a
  time, unchanged. `fn apply()` (1,115) is the `Got` dispatcher: one
  function per `Got` family in the module that already owns the subject
  (`report`, `disks`, `peek`, `rows`). Check each step with
  `SCOUR_GUI_SNAP` and the synthetic-event variables, same states before
  and after.
- **`MainWindow` (7,300 lines of one component).** The structs and the 28
  small components above it move to files first (`types.slint`,
  `widgets.slint`) — a text move. Then panels out of `MainWindow` one at a
  time, largest first, each a component with `in`/`out` properties and
  callbacks. Read the Slint-traps notes before the first: bindings that
  reach across a component boundary, `preferred-height` cycles and
  `min-width` behave differently once a subtree is its own component, and
  `build.rs` decides what is recompiled.
- **`page.html`.** Style (2,140 lines) and script (8,400) into files joined
  into one served document at build time, in a fixed order, by `build.rs`.
  **One** implementation of the join with three consumers: `include_str!`
  of the output, `page.test.cjs` (through `SCOUR_PAGE`, which it already
  honours), and `scripts/page-script.py`. The single scope stays, so the
  duplicate-name check in `scripts/check` keeps its meaning; modules are a
  later job of their own.
- **`scourd/src/handle.rs`, `fn run` (460).** One function per request
  family, the `match` left as the table of contents.
- **`scour-engine`, `fn run` (475).** The service's main loop; by job kind.
  Not hot, but subtle (walks, retries, commits): last, and alone in its
  commit.

Left whole on purpose: `walk_segment`, `reach`, and the fanotify loop.

### Phase 5 — small repeats, into `scour-ui`

Found twice with the same name and job: `home_form` (`scour-gui/src/
grep_ui.rs`, `scour-tui/src/v2.rs`); `sort_of` / `sort_key` (name →
`SortKey`, in `link.rs` and `app.rs`); what the hotkey row *says* about a
state (`shown`, `read_back`, `nudge` in the window; `answer`, `state` in
the bridge) — *read both before moving*, the bridge's may be a wire shape
rather than wording. Each is an hour; none needs a crate.

## 4. Where the next features go

| feature | crate | joins at |
|---|---|---|
| index of contents | `scour-content-index`, implementing a **new trait in `scour-core`** (candidates for a pattern, fed the same change stream) | `scourd/src/wire.rs` names it; `scour-engine/src/content.rs` asks the trait first and reads through `scour-grep` for what it cannot answer — which is today's whole behaviour. Per `AGENTS.md`: the trait goes in core, the dependency never into the engine |
| richer preview | `scour-preview`: `Shape` becomes a table of providers (text, code, picture, PDF, office, archive listing, media facts), each `sniff` + `look` | `Request::Preview`; the answer grows fields under `#[serde(default)]` |
| AI query writing | `scour-ask`: a sentence in, a query and its reading out. Depends on `scour-query` only — `syntax.rs` to prompt with, the parser to check the answer, `describe` to read it back | `Request::Ask` in the service, so every face has it and one place holds the key and the consent. The provider is a trait (remote, local, none); the crate is a feature, so a build without it has no network code |
| plugins | none yet | below |

**Plugins: tables first, a host later.** Four tables are the extension
points: extractors (Phase 2), preview providers, actions
(`scour_ui::menu`), query fields (`scour-query/src/fields.rs`). Keep each a
static table with one way in. A plugin system is then a loader that adds
rows — out of process, over the socket or stdio, as `scour-mcp` already is
— and `scour-client` (Phase 3) is what such a process links. Decide the
loader when there is a second author.

`scour-ask` with a remote provider sends words off the machine. Default:
the syntax reference and the sentence, never a file name or a path.

## 5. Order and size

0 → 1a → 1b → 1c → 1d → 2 → 3 → 4 → 5. Phase 2 is independent of the rest
and is the one the content work waits on: it may go first. Two phases are
never open in the same file. Rough sizes: 0 half a day; 1 three days; 2 two
days and a measurement; 3 two days; 4 a week, most of it the window; 5 half
a day.

## 6. Decisions wanted

1. **`Extractor` in core**: reshape it for seekable input and implement it
   in `scour-extract` (recommended), or delete it until an index of
   contents needs it.
2. **If grep on the shared reader measures slower**: keep two readers
   (recommended — the measured path wins) or accept the cost for one.
3. **One staleness rule for window and terminal**, or each keeps its own
   (recommended for this job).
4. **The terminal's msgid test** skips `report.rs`, `disks.rs`, `theme.rs`:
   intended, or a hole?
5. **`scour-ask` in the service** (recommended) or in a face.

## 7. What the first draft got wrong

- **"The faces repeat each other" — mostly not.** `report.rs` is drawing
  into Slint in the window and pure shapes in the terminal; the shares both
  use are already `scour-chart`'s. The three `disks.rs` are a drawing, a
  set of `App` methods and six HTTP routes; the wording they share is
  already `scour_ui::disks`. The repeats that exist are Phase 3 and Phase 5.
- **`scour-view` is dropped.** The window's `State` (43 fields, Slint
  models beside it) and the terminal's `App` (90 fields: menus, panels, key
  capture, hover) share about ten names and no structure; the terminal
  pages through `scour_page::Pages`, the window keeps its own offset and
  rows. A shared reducer would be a third state to keep in step with two.
- **"Files under 800 lines"** — the house rule is 300 and already written.
  And the obstacle is items, not files (§0), which the first draft did not
  see.
