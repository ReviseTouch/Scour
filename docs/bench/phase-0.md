# Content search, Phase 0: what was found

The answers `docs/PLAN-content-search.md` asks of Phase 0. Measured
2026-11-24 on this machine (20 threads, NVMe, btrfs and ntfs3), other work
running beside it. Corpus: random samples of paths taken from the index,
kept outside the repository; only counts and times are written here.

## The bench

`scripts/content-bench LABEL LISTS_DIR` runs `scour-grep`'s `scan` example
over every list, cold (each file asked out of the page cache with
`posix_fadvise(DONTNEED)`, and how many would not leave is counted) and
warm, and writes `docs/bench/content-LABEL.txt`. The first run is
`content-baseline.txt`: the code as it stands, a needle that is nowhere.

What it already says about today's reader:

- **Text is not the cost.** 20,000 source files, 226 MiB: 0.23 s cold,
  0.016 s warm, 16 MiB peak.
- **Most ZIP documents are cheap.** DOCX 0.06 s warm for 3,000 (75 MiB),
  ODT 0.017 s, UDF 0.03 s, PPTX 0.023 s for 889.
- **Two are not.** XLSX: 3,000 files, 329 MiB, **3.0 s and 23.8 CPU
  seconds, 274 MiB peak**, cold or warm alike — inflating shared strings and
  sheets, not reading the disk. EPUB: 1,690 files, 1.25 GiB, 1.4 s and
  10.8 CPU seconds. These two are where a cache would pay, not DOCX.
- **What is not read at all today:** 2,609 of 3,000 PDFs, 2,994 DOC, 2,989
  XLS and all 1,036 PPT are skipped as binary after their first megabyte; the
  391 "clean" PDFs are ones without a NUL in their first 8 KiB, searched as
  bytes — compressed streams, so a clean result there means nothing.
- RTF and EML are read as text and come back clean: control words and
  base64 hide the words. A word with a Turkish letter is written `\'fd` in
  RTF and is never found.
- 655 of 3,000 executables would not leave the cache: they are mapped by
  running programs. A cold number for them is a floor.

## Libraries

Built together in a scratch crate outside the workspace:
`pdf_oxide 0.3.78` and `office_oxide 0.1.17` with default features off,
`xberg 1.5.1` with default features off and `email` on.

| | |
|---|---|
| crates pulled in | 301 in all: `pdf_oxide` 170, `office_oxide` 53, `xberg` 224 (shared ones counted in each) |
| tokio | **does not arrive** with these features |
| binary | 37.0 MB, 28.0 MB stripped, for a program that does nothing else |
| build | 1 min 34 s release, from nothing, in the queue |
| licences outside MIT/Apache/BSD | `option-ext` MPL-2.0 (via `dirs`; already in Scour's lock through `directories`), `tiny-keccak` CC0, `blake3` CC0 OR Apache, `finl_unicode` + Unicode-DFS, the Unicode-3.0 tables. All permissive; MPL-2.0 is file-level copyleft and already shipped |
| upkeep | `office_oxide`: 18 releases since 2026-04-28, last 2026-11-11, 17 open issues. `pdf_oxide`: 83 releases, last 2026-09-08, 131 open issues. `xberg`: 1.x stable line plus a 5.0 release-candidate line, 5 open issues |

## The design's §6, answered

- **`office_oxide` 0.1.x** — eighteen releases in seven months, several a
  month: active, and not stable. Pin exactly (`=0.1.17`) and treat every
  bump as a change to measure.
- **Xberg with `pdf`, `office`, `excel` off** — opens e-mail (`email`) and
  HTML (`html`, pulled in by `email`). **It does not open RTF, ODT or EPUB:
  all three are behind `office`** (`src/extractors/mod.rs`). The design's
  assumption fails for RTF; ODT and EPUB are streamed anyway (D1). See
  "Asked of the owner" below.
- **Xberg without tokio, synchronously** — yes: `tokio-runtime` is a
  default feature and is off; `xberg::extract` is still `async` and is driven
  with a one-line executor (`pollster`). Its file reading falls back to
  `std` when tokio is off (`src/core/io.rs`).
- **Xberg's own cache** — `ExtractionConfig::use_cache`, default off, and
  it also reads environment variables (`core/config/extraction/env.rs`): the
  worker must start with a cleared environment so a variable cannot turn a
  cache or a feature on.
- **UDF `content.xml`** — one `<![CDATA[…]]>` holding the whole text as
  plain lines, in every sample opened; some archives carry a `sign.sgn`
  beside it. What `scour_extract::udf_cdata` assumes holds.
- **`FIEMAP`** — answers on single-device btrfs (a small file shows as one
  `encoded` extent: compressed, its offset still orders) and on `ntfs3`
  (`/mnt/depo`). No exFAT volume was mounted to try; recorded as untried.
- **macOS `F_LOG2PHYS_EXT`** — no machine. Nothing depends on it.
- **The report's unsourced figures** (share of machine code in binaries,
  ripgrep-all's cache settings, Agent Ransack defaults) — not used by the
  plan; left unverified.

## Each extractor on its own formats

One child process a file, as the worker will be: 30 s, 2 GiB of address
space, killed past either. `found` is files holding `mahkeme` after Turkish
case folding; `Turkish` is files whose text has at least one Turkish letter,
`mojibake` those with `\uFFFD`, `Ã`, `Ä±` or `Å` in it. Times are a file's
extraction inside the child, not the spawn.

| lib | format | files | ok | failed | timeout | crash | empty | Turkish | mojibake | found | p50 ms | p99 ms | max ms |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| pdf_oxide | PDF | 3,000 | 2,985 | 12 | 3 | 0 | 1,134 | 1,268 | 2 | 343 | 4.9 | 130 | 8,044 |
| office_oxide | DOC | 3,000 | 2,989 | 11 | 0 | 0 | 5 | 2,908 | 0 | 2,201 | 1.9 | 7.0 | 44 |
| office_oxide | XLS | 3,000 | 2,929 | 71 | 0 | 0 | 7 | 1,977 | 3 | 946 | 1.2 | 52 | 182 |
| office_oxide | PPT | 1,036 | 1,003 | 33 | 0 | 0 | 46 | 61 | 12 | 11 | 2.7 | 13.7 | 77 |
| office_oxide | DOCX | 3,000 | 2,992 | 8 | 0 | 0 | 38 | 1,191 | 27 | 597 | 1.3 | 10 | 85 |
| office_oxide | XLSX | 3,000 | 2,958 | 42 | 0 | 0 | 0 | 2,033 | 2 | 992 | 1.5 | 227 | 1,149 |
| office_oxide | PPTX | 889 | 873 | 16 | 0 | 0 | 244 | 113 | 0 | 32 | 1.5 | 39 | 80 |
| xberg | RTF | 3,000 | 0 | **3,000** | 0 | 0 | — | — | — | — | — | — | — |
| xberg | EML | 373 | 373 | 0 | 0 | 0 | 8 | 85 | 26 | 0 | 0.9 | 3.4 | 7.5 |

- **No panic and no crash** in 21,298 files. Three PDFs ran past 30 s;
  the worker's limit is needed, and is enough.
- **Failures are honest**: encrypted files (8 PDF, 47 OLE, 28 OOXML), files
  that are not what their extension says (`bad magic`: 46 OLE files are
  something else), truncated archives. None of them is a library bug that a
  second library would fix.
- **38 % of PDFs have no text layer** (1,134 of 2,985): scans, marked
  `no text` as the design says. Of the rest, 343 hold the needle.
- **The legacy formats are the gain.** 2,201 of 3,000 DOC and 946 of 3,000
  XLS hold the needle; today all of them are skipped as binary. That is the
  largest single blind spot of today's content search.
- PPT and EML show a little mojibake (12 of 1,003, 26 of 373). Not chased
  further here; Phase 3 samples them by eye.
- RTF: `Unsupported format: application/rtf` for every file — see below.


## D1, measured

The streamed path against `office_oxide`, on the same samples, needle
`mahkeme` with Turkish folding:

| format | streamed (today) | office_oxide | both | only streamed | only office_oxide |
|---|---:|---:|---:|---:|---:|
| DOCX (3,000) | 597 | 597 | 597 | 0 | 0 |
| XLSX (3,000) | 988 | 992 | 988 | 0 | 4 |
| PPTX (889) | 31 | 32 | 31 | 0 | 1 |

The five it misses: in four workbooks the word is only in a sheet's name
(`xl/workbook.xml`) or a cell comment (`xl/comments1.xml`); in the
presentation it is only in charts (`ppt/charts/*.xml`). The reader is not
wrong, it leaves those parts out on purpose. Adding them is a Phase 1 item.

Cost, same files: streamed DOCX 0.46 s, XLSX 2.4 s (17.8 CPU s), PPTX
0.09 s, all threads; `office_oxide` p50 1.3–1.5 ms a file on one thread,
before a process is spawned or anything is cached.

**D1 holds**: the streamed path finds the same files, minus three kinds of
part, an order of magnitude faster than extraction would be on a first
search, with nothing to keep fresh. XLSX is the expensive one either way
(the streamed path spends 6 ms of CPU a workbook); if a cache is ever wanted
for a ZIP format, it is that one, and the measurement above is the case for
it.

## Asked of the owner

**RTF.** The design has RTF from Xberg with `office` off; Xberg puts RTF
behind `office`. Per the working rules this stops here. The two ways on:

1. Turn Xberg's `office` on. It brings biblatex, typst-syntax, org, dbase,
   unicode-normalization and more for one format; to be measured.
2. **Recommended:** read RTF in `scour-extract`, streamed like UDF, no
   cache: control words dropped, groups such as `\fonttbl`, `\pict`,
   `\*\…` skipped, `\'hh` decoded through the document's `\ansicpg`
   (1254 for nearly every file here) and `\uN` as Unicode. A few hundred
   lines; 3,000 RTFs total 51 MiB, so speed is not a question.

With (2), Xberg is left with e-mail alone (EML; MSG and PST were not in the
sample): 224 crates for one format. The same question then stands for it —
`mail-parser` is what Xberg itself uses for EML, and is the one crate
needed. That is a change of library and is not made without the owner.
