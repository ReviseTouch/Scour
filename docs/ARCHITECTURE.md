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
| **transport** | `scour-proto`, `scour-ipc` | the shape of a question and an answer, framing | what either means |
| **shared presentation** | `scour-ui`, `scour-page`, `scour-i18n`, `scour-settings`, `scour-places`, `scour-thumbs` | every question all faces must answer the same way | drawing tools |
| **faces** | `scour-web`, `scour-gui`, `scour-tui`, `scour` | drawing, keys, the pointer | the index, the filesystem, the engine |

**A face never links the engine.** All four learn everything they know over
the socket. That is where the loose coupling lives: between the faces and the
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
| `scour-ui` (palette, columns, bands, kind colours) | `#0d1117`, the twelve columns and their widths, the time bands | CSS and `.slint` held two copies, and the focus colour had already drifted |
| `scour-ui::faces` | what each face has | §5 |
| `scour-page` | pages of 200 rows, an LRU of 32, an answer written at the offset *it* names, when a short page is the end | the paging rules; each was once wrong in one interface |
| `scour-settings` | columns and their order, widths, language, layout, **which face opens**, skip rules | the four faces' shared memory; `config.toml` stays the file written by hand |
| `scour-i18n` | the catalogue, and the **order of languages**: chosen → `config.toml` → desktop → English | `.po` files; English msgids in code. The order is written once, in `choose()` |
| `scour-places` | the desktop's own folders, which volumes record reads | a question about the machine, not about the index |
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
| **page** (`scour-web`) | HTML/CSS/JS in one file (`page.html`), served by the bridge | runs in a browser; `POST` routes are fenced by a token, the origin and `--no-launch` |
| **window** (`scour-gui`) | Slint, software renderer | the model is built once and updated in place |
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
| `SCOUR_GUI_QUERY/PANEL/SCROLL/CLICK/HOVER` | puts the window in a state, sends a synthetic event |
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

Pictures are **thumbnail first**: a 380-pixel panel does not decode forty
megapixels. Failing that, the service is asked (the same door as the grid, the
same four-at-a-time limit); a file under 512 KB is drawn directly — icons,
screenshots and the thumbnail cache's own files fall in that class.

**Two Slint traps, both found by measuring.** Binding a child's
`preferred-height` to its parent's `height` is a cycle, and Slint's answer is
to draw nothing. And a list whose own minimum is wider than the window squeezes
its sibling to fifty pixels: `min-width: 0px` on the list means "may be cut on
the right", and it is the only way the panel can take its width.
