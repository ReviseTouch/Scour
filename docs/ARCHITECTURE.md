# Scour — how it is put together

This document describes where each part of Scour lives and where a new part
belongs. The reasoning behind individual decisions is in the code; this is the
map.

The governing rule: every crate does one complete job, and crates are joined
by data and contracts rather than by call chains.

---

## 1. At a glance

```
        ┌──────────┐   ┌──────────┐   ┌──────────┐   ┌──────────┐
        │   page   │   │  window  │   │ terminal │   │ command  │   ← four faces
        │scour-web │   │scour-gui │   │scour-tui │   │  scour   │
        └────┬─────┘   └────┬─────┘   └────┬─────┘   └────┬─────┘
             └──────────────┴───────┬──────┴──────────────┘
                                    │  one socket, one protocol
                              ┌─────▼──────┐
                              │  scourd    │   ← the only writer, the only decider
                              └─────┬──────┘
                    ┌───────────────┼───────────────┐
             ┌──────▼─────┐  ┌──────▼──────┐  ┌─────▼──────┐
             │scour-engine│  │scour-index- │  │scour-source│
             │  (search)  │  │   native    │  │   -fs      │
             └────────────┘  └─────────────┘  └────────────┘
```

Four faces, **not four programs**. All of them talk to the same service over
the same socket and get the same answer. What differs is only the drawing.

---

## 2. Layers, and the rule

Dependencies flow downwards and **never** upwards.

| layer | crate | knows | does not know |
|---|---|---|---|
| **data** | `scour-core` | what a row, a query and an error are | filesystems, sockets, interfaces |
| **language** | `scour-query` | how to read what was typed | how the index is stored |
| **storage** | `scour-index-native` | columns, trigrams, the directory table | where a query came from |
| **search** | `scour-engine` | which rows answer, in what order | who is asking |
| **source** | `scour-source-fs`, `scour-watch` | walking and watching the filesystem | the inside of the index |
| **reading files** | `scour-grep`, `scour-extract`, `scour-preview`, `scour-textcache` | what is inside a file, under bounds; documents' text kept | Scour: paths in, text out |
| **document readers** | `scour-doctext`, linked by `apps/scour-extractor` alone | PDF, legacy Office, e-mail: bytes in, text out; a word processor's document as it flows | files, the service; it runs in a process the service may kill |
| **flowing documents** | `scour-flow` | a word processor's document as paragraphs, tables and pictures, a spreadsheet's as sheets with their numbers' formats, a presentation's as slide cards, and their drawing as HTML | files and formats: `serde` alone, filled by the extractor, drawn by the faces |
| **transport** | `scour-proto`, `scour-ipc` | the shape of a question and an answer, framing | what either means |
| **client** | `scour-client` | a held connection, a service started when none answers, the requests whose wording is a decision | what a face draws, and which of its answers is stale |
| **shared presentation** | `scour-ui`, `scour-page`, `scour-i18n`, `scour-settings`, `scour-places`, `scour-volumes`, `scour-thumbs` | every question all faces must answer the same way | drawing tools |
| **faces** | `scour-web`, `scour-gui`, `scour-tui`, `scour` | drawing, keys, the pointer | the index, the filesystem, the engine |

**A face never links the engine.** All four learn everything they know over
the socket. `apps/scourd/tests/layers.rs` fails the build when one does. That is where the loose coupling lives: between the faces and the
service.

---

## 3. Shared presentation

Anything that all interfaces must answer identically is written once, in a
shared crate:

| crate | decides | why there |
|---|---|---|
| `scour-ui::format` | `5.356.281`, `1,44 MiB`, `2026-08-13 00:49`, the six age bands | punctuation belongs to the **language**, not the platform: an English window on a Turkish desktop prints `5,356,281` |
| `scour-ui::path` | leaf, folder, breadcrumb steps | three faces were cutting paths three different ways |
| `scour-ui::query` | how a pressed filter joins the typed text, and how pressing it again clears it | "one filter, appended" is a language rule, not a drawing rule |
| `scour-ui` (palette, columns, bands, kind colours) | `#111113`, the twelve columns and their widths, the time bands | CSS and `.slint` held two copies, and the focus colour had already drifted |
| `scour-ui::faces` | what each face has | §5 |
| `scour-page` | pages of 200 rows, an LRU of 32, an answer written at the offset *it* names, when a short page is the end | the paging rules; each was once wrong in one interface |
| `scour-settings` | columns and their order, widths, language, layout, **which face opens**, skip rules | the four faces' shared memory; `config.toml` stays the file written by hand |
| `scour-i18n` | the catalogue, and the **order of languages**: chosen → `config.toml` → desktop → English | `.po` files; English msgids in code. The order is written once, in `choose()` |
| `scour-places` | the desktop's own folders, which volumes record reads | a question about the machine, not about the index |
| `scour-volumes` | which disks the machine has, and whether each is the system's, built in, plugged in or on a network | the service decides what to index from it and the faces draw it; macOS answers nothing yet |
| `scour-thumbs` | where the thumbnail cache is, which kinds are never worth a look, who may make one | the one rule that says which row earns four `stat`s; the page and the window ask the same question |

Anything needed by two interfaces belongs here.

The same rule holds for paths: `Config::state_dir()` says where settings are.
It was written in three places once — service, window, terminal — and the day
one of them drifted, a language chosen in one face would vanish from the other.

And for words: every word a face shows comes from the catalogue. The terminal
has a test that reads its own source and looks up every msgid handed to
`say(...)` in the Turkish catalogue, because a hand-kept list of msgids is
correct on the day it is written.

---

## 4. The service: the only writer

`scourd` is the one process that writes the index; the faces only read and ask.

- **Three lanes.** `scour-ipc` carries one call at a time and `scourd` opens a
  thread per connection. So every face holds several connections: the one a
  keystroke waits on (search), the one that can wait (facets, rules, CSV), and
  the **long poll** (has the index moved) — the last holds its connection for
  thirty seconds and has to be on a lane of its own.
- **Generations.** Every search carries a number; a stale answer is never
  drawn. A slow answer to `re` landing on top of a fast answer to `rapor` is
  the worst thing a search box can do.
- **The answer carries the offset.** A page is written at the offset the
  **answer** names, not the one that was asked for.
- **Reading files is not the index's job.** What is inside a file is read
  when it is asked for. The engine asks the index which files, in the order
  wanted, and hands them to `scour-grep`, which knows nothing of Scour: paths
  and a pattern in, lines out. What is inside a ZIP-based document — its
  directory, an entry inflated, its XML made plain — is `scour-extract`'s,
  which the preview reads a document's text with too: one reader, so a new
  format is added once and both search and preview have it. It asks `surefoot` what each root stands on, as
  the walk does, and reads a spinning disk on one thread (a large set where each
  file lies on the platter, by `FIEMAP`, on Linux; by path elsewhere), a solid one on half the cores up to sixteen, a network one on
  eight, an unnamed one on four; a FUSE or ZFS volume on four at most. A disk that
  says it spins is asked again, on Linux, before a large read: flash behind a
  USB bridge says so too, and is read by two.
- **An NTFS volume may be read raw.** Where the process may open `\\.\X:` —
  the Windows service — a spinning disk's large set is read from the volume
  itself (`scour-grep/src/raw`): places from the MFT, read once, the files'
  clusters in platter order 32 requests deep, no file opened. Only what the
  normal way would answer: the asker's token checked against each file's
  descriptor in `$Secure`, files the change journal says changed in the last
  five minutes or during the read left to the normal way, and every file
  found to hold the pattern read again the normal way — the raw read only
  says what is not there.
- **Documents it cannot stream go to another process.** PDF, `.doc`, `.xls`,
  `.ppt` and `.eml` are handed to `scour_grep::Texts`, which `scourd` answers
  (`src/documents.rs`): their text kept in `scour-textcache` (zstd blocks,
  keyed by path, size and time to the nanosecond, in a folder of its own
  that a folder of somebody's files is never taken for), and read on a miss by `scour-extractor`
  processes — 30 s and 2 GiB each, killed and started again when one dies or
  hangs, ended after 20 s idle. The reading thread opens the file itself,
  under the asker's identity, and pipes its bytes over: the extractor opens
  nothing. A kept text is used only after a `stat` says the file is unchanged
  and `access` says the asker may read it.
- **`content:` is a search like any other, answered as it is read.** A term
  standing alone is taken out of the query; the rest names the candidates and
  a thread of its own reads them (`scour-engine/src/content.rs`). A page is
  what has been found so far, `capped` until every candidate is read, and the
  revision moves as rows arrive — so a face waiting on `await` asks again and
  its list grows, with nothing in the face written for it. The reading goes
  on for fifteen seconds past what was asked for, and again when a later page
  is. `!content:` and `a | content:` are still refused; the rail counts the
  candidates, not the finds.
- **Where an index of contents would join.** `holding()` in
  `scour-engine/src/content.rs` is the one place a batch of candidate rows
  becomes the rows whose files hold the pattern. An index of contents answers
  that question first and leaves to the read-through what it cannot; its
  trait belongs in `scour-core` and its concrete type is named only in
  `scourd/src/wire.rs`. Nothing implements one yet, so no trait is written.

---

## 5. What the faces have: the chart is in the code

`scour-ui::faces` holds every feature and its state in all four faces;
`scour features` prints it:

```
feature         page          window        terminal      command line
search          yes           yes           yes           yes
language        yes           yes           yes           yes
thumbnails      yes           no            no            —
```

`no` and `—` differ: one is "not yet", the other "cannot be there" (thumbnails
on a command line). Both have to say **why** — a test checks that.

**A new feature starts by adding a row to the chart.** A face that lags stays
there as `no`; nothing goes quietly missing. A document can go stale; the code
cannot.

---

## 6. The faces: what each does for itself

| face | drawing | its own |
|---|---|---|
| **page** (`scour-web`) | HTML/CSS/JS in parts under `src/page/`, joined by `build.rs` into one document the bridge serves | runs in a browser; `POST` routes are fenced by a token, the origin and `--no-launch` |
| **window** (`scour-gui`) | Slint, software renderer; `ui/` holds the window and its parts (`settings.slint`, `menu.slint`, `report.slint`, `widgets.slint`, `types.slint`) | the model is built once and updated in place; every callback is bound in `src/wiring/`, a file a section, and an answer from the service is drawn by `apply` or `src/answers.rs` |
| **terminal** (`scour-tui`) | ratatui, immediate mode | `--once`, `--press`, `--click`: the only way to check the screen and a click |
| **command line** (`scour`) | text and `--json` | one question, one answer; no paging |

The common rule: **the place that draws and the place that hits use the same
arithmetic.** The window lost two days to breaking it (a panel drawn in one
place and tested in another); in the terminal `spot_at` and `draw` share their
constants.

---

## 7. Checking tools

A screenshot does not verify an interface; a trace and a synthetic event do.

| tool | what it does |
|---|---|
| `SCOUR_GUI_SNAP=/x.png` | the window photographs itself |
| `SCOUR_GUI_QUERY/PANEL/SCROLL/CLICK/HOVER/RESIZE` | puts the window in a state, sends a synthetic event |
| `scour-tui --once WxH` | prints the frame **as text** |
| `scour-tui --press`, `--click` | presses a key, a point |
| `SCOUR_TUI_TRACE=/tmp/log` | writes what happened to a file (not the screen) |
| `scripts/tuishot` | photographs a terminal program without a screen: a pseudo-terminal, the ANSI replayed, drawn by a browser |
| `scripts/bench` | nine queries, the service's CPU/RSS, the terminal's first frame |
| `examples/*.rs` (index) | `rankcheck`, `reachcost`, `pathcost` — measure before claiming |
| `tests/smoke.rs` (index) | every query shape against **brute force** |

The measuring rule is at the top of `docs/MEASUREMENTS.md`: run two binaries
**alternately**; never compare a morning against an afternoon.

---

## 8. Adding something

1. Add the row to `scour-ui::faces` — with its state in all four faces.
2. Where does the meaning belong? Needed by two faces: the shared crate. A
   drawing peculiar to one face: that face.
3. If the service has to answer, add the question and answer to `scour-proto`
   — fields `#[serde(default)]`, so an old client can read a new answer.
4. If there is a measurable claim, measure first (`examples/`, `scripts/bench`)
   and write it to `docs/MEASUREMENTS.md` with the command.
5. If there is a correctness claim, compare against brute force.

---

## 9. Where to start reading

- **What it does:** `README.md`, then `scour features`.
- **The language:** `crates/scour-query/src/syntax.rs` — the reference itself.
- **A query's path:** `apps/scourd/src/handle.rs` → `scour-engine` →
  `scour-index-native/src/search.rs`.
- **A face's path:** `apps/scour-tui/src/main.rs` is the shortest, and its loop
  is explained at the top.
- **Why it is this way:** `docs/MEASUREMENTS.md`.

---

## 10. The preview panel — a third column

Beside the list, showing the selected row. **A mode, not a glance**: it stays
open and follows wherever the arrows go — Everything's preview pane. The window
and the page draw the same panel:

| | from |
|---|---|
| which facts, in what order | `scour_ui::preview::FACTS` — the labels are the column headings' own msgids |
| width and its limits | `scour_ui::preview::PANEL_WIDE/MIN/MAX` |
| *what* the file is | the service (`Request::Preview`) — the decision needs the file's first eight kilobytes |
| the facts | the service (`Request::Stat`) — four of them are in no column |
| whether it is open | `Settings::preview` — a panel pinned in one face opens in the other |

**What is shown of a file**, decided once, by `scour_preview::shape_of` in the
service (`Request::Preview`), and drawn by each face as it can. This table is
the rule; `scour-preview`'s tests check that it names every extension the
code draws.

| file | shown as | where it depends on the machine |
|---|---|---|
| a folder | its facts | — |
| png jpg jpeg jfif gif webp avif bmp ico svg | the picture, thumbnail first | — |
| pdf | its pages | the window draws them with CoreGraphics (macOS), `Windows.Data.Pdf` (Windows) or the first of Poppler, MuPDF, Ghostscript on the path (Linux); with none, its text. The page leaves a PDF to the browser |
| mp3 m4a aac oga ogg opus wav flac · mp4 m4v webm ogv mov mkv | played | the page only, by the browser, which decides the codec; the window shows the facts |
| Word, Excel, PowerPoint (`office::is_office`) | its pages, where LibreOffice or Microsoft Office turns it into a PDF; else drawn by Scour (`office_view`: drawn) or its text | pages only on Windows and macOS: on Linux LibreOffice is never used, a document's seconds being no preview's |
| OpenDocument (odt ods odp), RTF | its pages where a converter is, as Word's; else its text | as Word's |
| anything else whose first 8 KB has no NUL | text: UTF-8; UTF-16 with its mark; else, with almost no control bytes, Windows-1254 as content search reads it | — |
| anything else | its facts | — |

Space opens the desktop's own preview where there is one: `qlmanage`,
`sushi`, `gnome-sushi`, `nemo-preview`, `gloobus-preview`, `QuickLook.exe`, or
`scour-web --quicklook CMD`. The settings show, under the list's section, what
this machine has (`scour_preview::means`).

Pictures are **thumbnail first**: a 380-pixel panel does not decode forty
megapixels. Failing that, the service is asked (the same door as the grid, the
same four-at-a-time limit); a file under 512 KB is drawn directly — icons,
screenshots and the thumbnail cache's own files fall in that class.

**Two Slint traps, both found by measuring.** Binding a child's
`preferred-height` to its parent's `height` is a cycle, and Slint's answer is
to draw nothing. And a list whose own minimum is wider than the window squeezes
its sibling to fifty pixels: `min-width: 0px` on the list means "may be cut on
the right", and it is the only way the panel can take its width.

---

## 11. Where the next features go

| feature | where | joins at |
|---|---|---|
| index of contents | a crate of its own, implementing a trait in `scour-core` | `holding()` in `scour-engine/src/content.rs`, named in `scourd/src/wire.rs` |
| a new document format | `scour-extract` | its container and text layers; grep and the preview both read it from there |
| richer preview | `scour-preview`, reading document text through `scour-extract` | `Request::Preview`, new fields `#[serde(default)]` |
| an OpenDocument file drawn | its own reader of `content.xml` filling `scour-flow`'s model; office_oxide reads only Microsoft's six formats | `Request::Flow`; `Look::flow` says it is there |
| AI-written queries | a crate depending on `scour-query` only: a sentence in, a query and its reading out | a request the service answers, so every face has it and one place holds the key and the consent |
| a plugin | a process of its own, linking `scour-client` | the socket, as `scour-mcp` does |

The tables a plugin would add rows to are already tables: extractors
(`scour-extract`), actions (`scour_ui::menu`), query fields
(`scour-query/src/fields.rs`). A loader comes when there is a second author.
