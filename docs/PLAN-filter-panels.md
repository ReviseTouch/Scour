# Filter panels plan

For whoever carries it out. The owner approved the direction on 2026-11-25
from a clickable mock: `docs/design/filter-panels.html` (open it in a browser;
also https://claude.ai/artifact/Soegr5rVfVPh22fej544M6). The mock is the
picture; this file is the scope. Where they disagree, this file wins — the
mock's numbers are invented and three of its details turned out wrong (§7).

Nothing here is started. Each phase is one branch-sized piece that ships on
its own; stop after any of them and the product is whole.

## 0. What is already true

Read these before writing anything; most of the plan is rearranging what they
already do.

| what | where |
|---|---|
| which lens a term opens, the launcher's rows, the term a choice writes | `crates/scour-ui/src/lens.rs` (+ `lens_tests.rs`) |
| the page's lenses | `apps/scour-web/src/page/script/050-lens.js`, styles in `page/style/020-window.css` (`.lens`, `.lp-*`, `.lspark*`) |
| the page's capsules | `page/script/030-query.js`, styles `.qshadow .term.cap*` |
| the window's lenses | `apps/scour-gui/src/qlens.rs` (what to ask, the items), `qwire.rs` (`show_lens`, `show_place_lens`, `show_calendar`), `ui/main.slint` (`lens-*` properties), `ui/types.slint` (`LensRow`, `PlaceLeft`, `PlaceDir`, `Cap`) |
| the window's capsules | `apps/scour-gui/src/qbar.rs`, `qwire.rs` (`draw`, `carve`, `width_of`, `measured`) |
| the terminal's lenses | `apps/scour-tui/src/v2.rs` |
| disks and their state | `scour-proto/src/disks.rs` (`Disk`, `DiskState`, `Seen`), `scour-volumes/src/volume.rs` (`Volume`, `Kind`); request `volumes`, `choose`, `postpone` |
| counts a lens can ask for | `count`, `facets` (`kind`, `ext{top}`, `dir{path,top}`, `age{edges}`), `usage{path,top,q}`, `tree` |
| palette | `crates/scour-ui/src/palette.rs`; the page gets it through `@THEME@` |
| translations | English msgid in code, `lang/tr/LC_MESSAGES/scour.po` |

Checked against the running service on 2026-11-25:

- `rapor under:/mnt/depo|under:~/Belgeler` already reads as "rapor and (under
  A or under B)". **Several disks need no change to the language.**
- "Before a date" is `dm:<2025-01-01` or `dm:<30d`; a range is
  `dm:2026-11-12..2026-11-25`. A bare `dm:2025` or `dm:2025-03` is **not**
  read (it becomes a name search).
- A file of no bytes is `empty:`; `size:0` means "at least 0".
- There is no size facet. `age{edges}` takes ages in days and lifts the
  200,000-row sampling cap, so it costs a full walk of the matching rows.
- `Volume` has `size` but no free space.

## 1. Rules that hold through every phase

From `AGENTS.md`, `CLAUDE.md` and decisions the owner has already made. Break
none of them without asking.

1. **The text is the only model.** A panel choice replaces one term of the
   query text (`lens::replace`); a capsule is a drawing of a term. No second
   list of filters anywhere.
2. **Faces do not parse queries.** New logic about *which term* and *what a
   choice writes* goes in `scour_ui::lens`, tested there, and all three faces
   call it. The page mirrors it in JS today; keep the two in step and add the
   case to `apps/scour-web/tests/page.test.cjs`.
3. **Nothing moves under the pointer.** No hover that widens a capsule or
   shifts a row. A capsule's buttons live only in the pill floating *above*
   it. A panel hangs from its capsule; nothing opens centred or modal.
4. **Visible defaults are the owner's.** If a phase would change what a user
   sees without acting (a new always-on element, a different default sort, a
   new setting's default), stop and ask.
5. **The first page is sacred.** No phase may make typing-to-first-rows
   slower. Counts for a panel are asked after it is drawn, on the lens lane,
   and dropped by generation when stale — as they are today.
6. English identifiers, comments, commits, msgids; Turkish only in the `.po`.
   Files about 300 lines, functions about 40. No `Co-Authored-By` line.
7. Every cargo build goes through the queue: `derle cargo check -p <crate>`,
   `derle cargo test -p <crate>`; `derle bash scripts/check` once at the end
   of a phase. Running a built binary does not.
8. **Never test against the owner's service.** Copy the index
   (`cp --reflink`), write a config with `on_start = false` and
   `watch = false`, use a socket of your own under `/run/user/1000/` (a longer
   path overflows `sun_path`), and set `SCOUR_NO_AUTOSTART=1`. Do not use
   `pkill -f <pattern>` — it matches its own shell; find the pid first.
9. Look at what you built before calling it done. Page: headless Chromium.
   Window: a headless `gnome-shell` with `SCOUR_GUI_QUERY` / `SCOUR_GUI_KEY` /
   `SCOUR_GUI_SNAP` (see the `SCOUR_GUI_*` variables in `apps/scour-gui/src`).
   Say in the phase report what was looked at and what was not.
10. A wire or CLI change edits `AGENTS.md` in the same commit.

Branch from `main` as `feat/filter-panels`, one commit per numbered step.
`content/marks-and-bar` is unfinished work in the same files
(`qwire.rs`, `030-query.js`); do not build on it, and ask the owner whether it
lands first.

## 2. Phase A — the launcher (`:` and "Filter ▾")

Presentation only; no new request.

1. `lens.rs`: give `LAUNCH` rows a group (`narrow` / `how`) and a stable icon
   name (`folder`, `clock`, `shapes`, `ext`, `size`, `approx`, `read`). Keep
   the order: under, dm, kind, ext, size | fuzzy, content. `launch()` also
   says, per row, whether the query already holds that field (use `terms()`).
2. Page: each row is icon tile + name + one line about it + shortcut (`:k`
   style — the letters that already filter the list, shown, not new keys).
   Two group headings. The `content:` row uses the amber capsule colours.
   A row whose field is in the query shows an "in the query" tag, and choosing
   it puts the caret in that term instead of writing a second one.
3. Window: the same through `LensRow` (add `icon`, `group`, `held`); icons are
   the ones `qbar.rs` already draws in capsule heads.
4. Terminal: group headings and the "in the query" mark, as text.
5. Reword the abouts where the mock is clearer (msgids + `.po`): "which disk,
   which folder", "looks inside the file, not at its name".

Done when: `lens_tests.rs` covers groups and the held mark; the page test
passes; one screenshot of each face's launcher is in the report.

**Not in this phase:** the live summary on the right of each row (mock §1).
It is five extra counts per opening. It is Phase G, behind a measurement.

## 3. Phase B — the place panel: disks first

The centre of the work. Data is already on the wire except per-disk file
counts, which `usage{path: <root>, top: 0}` gives (`root.files`); cache per
index revision, ask after the panel is drawn.

1. `lens.rs`: a pure function from `&[Disk]` (+ counts) to tiles: name, kind
   (`system`/`fixed`/`removable`/`network`/`optical`), filesystem, first
   root, size, state for display — `live` (indexed and `seen.live`), `polled`
   (indexed, not live), `walking` (`seen.walking`), `asking`
   (`Asking`/`Later`), `off` (`Off`/`Manual`). Tested with fixtures.
2. Page, top of the place lens, replacing the left column's disk list:
   - the **strip**: one segment per indexed disk, width by file count, a
     2% floor so a small stick is still visible; colour by kind (new palette
     entries `disk_system`, `disk_fixed`, `disk_removable`, `disk_network`,
     both themes, in `palette.rs`);
   - the **tiles**: icon by kind, name, `fs · root`, state with a dot, file
     count. For a disk whose sources are only some of its folders (state
     `manual`/`configured` with sources inside it — the owner's own machine),
     list those roots as small tags; get them from `sources`.
   - an `asking` tile carries its question in place: "Index it", "Not this
     time" (`postpone`), "Never" (`choose index:false`). These calls exist.
3. Choosing: a click on an indexed tile writes `under:<root>`; Ctrl+click
   adds another as `under:a|under:b` (one `|` group, no spaces, so it stays
   one run of terms). Clicking the only selected tile removes the term.
4. Below the tiles the folder list stays as it is, with two changes: the
   buttons read **Add** and **Exclude** in words instead of `+` and `!`, and
   a footer row ("Searching in: …") lists what the query's `under:` /
   `!under:` terms currently hold, each removable. That row is drawn *from
   the query text*, not kept beside it.
5. "Choose folder…" stays where it is.
6. Window: the same three parts through `PlaceLeft` → a new `DiskTile`
   struct in `types.slint`; the strip is a row of `Rectangle`s. Mind the
   Slint traps: a `Rectangle` given only `height` centres vertically (set
   `y`), and an elided `Text` shorter than its font vanishes.
7. Terminal: the disk list gains kind, state and count columns; no strip.

Done when: with a loop device mounted and unmounted against an isolated
service (`udisksctl loop-setup`), the tile appears as `asking`, "Index it"
turns it `walking` then `live`/`polled`, and unplugging removes it; two disks
chosen give the `|` query and the right count; screenshots of page and window.

**Not in this phase** (each needs something the service does not do today —
see §7): how full a disk is; a disk that is indexed but no longer mounted;
partitions that are not mounted at all.

## 4. Phase C — the scope chip

A small always-visible control at the left of the query bar: one dot per
indexed disk in its kind colour, and "Everywhere" or the names the query's
`under:` terms resolve to. A disk in `asking` state adds a ringed dot. A click
opens the place panel (writes `under:` as the launcher's Location row does).

Page, then window; the terminal already has a disk count in its status line
and keeps that. The chip is drawn from `volumes` + the query text; it holds no
state. This is a new always-on element: the owner approved it with the plan,
but show a screenshot before merging — it costs about 90 px of the bar.

## 5. Phase D — time, kind+ext, size

One component, three uses: preset buttons with a count each, an optional
histogram under them, a line that says the choice in words.

**D1 Time.** Presets today / yesterday / 7 d / 30 d / year with counts — one
`facets age{edges:[1,2,7,30,365]}` call, which the lens already makes for its
sparkline. Add a scale switch Day / Week / Month / Year (edges generated per
scale: 30 daily, 26 weekly, 24 monthly, 10 yearly) and a three-way
Between / Before / After. What it writes: a trailing window `dm:7d`; a
bounded band as dates `dm:A..B`; Before `dm:<DATE`; After `dm:>DATE`. Dragging
across bars selects a band (the page has `sparkRangeTerm`; extend it). The
window keeps its calendar under the histogram. The capsule says the range in
the user's locale ("12–25 Nov"); the text stays ISO.

**D2 Kind and extension, one panel.** Tiles for the kinds with count and
share (the `kind` facet). Selecting kinds shows below them the extensions
*within those kinds* (`facets ext{top:8}` over the query + the kind term).
Click toggles a value in the list term (`lens::list` already does this);
Alt+click excludes (`!kind:build`). `ext:` typed directly opens the same
panel with the extension row focused. `Lens::Kind` and `Lens::Ext` stay two
variants; they share a view.

**D3 Size.** Presets Empty (`empty:`) / under 100 KB / 100 KB–10 MB /
10 MB–1 GB / over 1 GB, with counts from five `count` calls as the lens does
for its choices now. **No histogram** unless a size facet is added — that is
a `scour-core` + index + `scour-mock` change and a separate decision.

Each sub-phase lands on the page and the window together; the terminal gets
the presets-with-counts list.

## 6. Phase E — capsules

Page first (E1–E4), then the window (E5), because the window measures text
widths in Rust (`width_of`, `measured`) and every inner part needs a width.

1. **Inner chips.** A list value (`kind:doc;code`, `ext:pdf;odt`) draws each
   value as its own small chip, separated by `·`; more than fit collapse to
   `+N` — the capsule never grows past its line. A path value draws as crumbs
   separated by `›`, last segment bold; a crumb click rewrites the term to
   that ancestor. Single-valued fields stay plain text. The letters of the
   term stay in the text layer at zero width, as headers do today, so every
   caret offset still has a place.
2. **The disk in the place capsule.** Head icon by the kind of the disk the
   path is on, and a state dot (live / polled / not reachable), from the same
   `volumes` answer the chip uses.
3. **Effect in the floating pill:** "18,420 → 212" — the count without this
   term against the count with it. One `count` on hover, cached per term and
   revision; never asked before the pointer rests on the capsule.
4. **The capsule that emptied the list** gets a small red mark. The "each
   filter left out on its own" counts already exist for the empty-list
   screen; reuse that answer, ask nothing new.
5. **An OR group** (`a|b` terms) is framed with a dashed outline and the word
   "or" between its capsules.
6. The window: the same five, with widths measured through the existing
   hidden-`Text` path.

Unchanged on purpose: capsule height (32 px in a 36 px line), the edit state
(raw value in a white well with a focus ring), the empty dashed capsule, the
negated red capsule, buttons only in the pill above.

## 7. Phase G — optional, each behind its own question

Do none of these without the owner saying so for that item.

| item | what it needs | why it waits |
|---|---|---|
| live summary on launcher rows | 5 counts per opening | measure first: alternate two binaries, typing-to-first-rows and launcher-open-to-drawn, numbers into `docs/MEASUREMENTS.md`; show them and stop |
| how full a disk is | `free` on `Volume` (`statvfs`, `GetDiskFreeSpaceExW`), wire + `AGENTS.md` | new field on the wire; cross-check Windows |
| a disk that went away | what the service does with its rows today is **not verified**; read `apps/scourd/src/live.rs` and report before designing | the mock's faded tile assumes rows are kept |
| partitions not mounted | discovery beyond `/proc/self/mounts` | `scour-volumes` sees mounted filesystems only |
| size histogram | `FacetBy::Size{edges}` in core, the index, `scour-mock::brute_force` | a contract change |
| bare `dm:2025`, `dm:mart` | parser change in `scour-query` | a language change |

### Where the mock was wrong

Its first version, and what was learnt checking it. The copy in
`docs/design/` is already corrected on these points.

- It says several disks need `under:a;b`. They do not: `under:a|under:b`
  works today. (`under:a;b` currently reads as "under a and path contains b".)
- It says `dm:<2025` exists. `dm:<2025-01-01` does; a bare year does not.
- Its "or type: 3 hafta · mart · 2025" line promises spellings the language
  does not have. Show only what parses (`3w`, `2026-03-01..2026-03-31`).

## 8. Order, and what to report

A → B → C → D1 → D2 → D3 → E. A and B are independent of the rest; E does
not depend on D. After each phase: what changed, the tests added, what was
looked at on which face, what was not verified, and any question for the
owner — then wait for the go-ahead before the next phase.
