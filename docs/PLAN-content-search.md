# Content search: implementation plan

For whoever carries it out. Written 2026-11-24 from the owner's design
("Scour içerik arama: uygulama planı", same date — called **the design**
below) and from a reading of this repository as it stands at `c7ac089`.
The design was written without the repository in view; a good part of it is
already built. This plan says what exists, what is missing, and in what order
to build the rest.

`AGENTS.md` is the contract and wins over this file. The design's §2
decisions are binding **except** where §2 below lists a conflict with the
code — those five are the owner's to settle before the phase they touch.

## 0. What is already true

Content search exists and is used: `crates/scour-grep` (a leaf: paths in,
finds out), `crates/scour-extract` (ZIP directory, inflate, XML made plain),
`scour-engine/src/grep.rs` (the bounded `grep` request) and
`scour-engine/src/content.rs` (`content:` in a query, answered as files are
read). Faces: `scour grep`, MCP `scour_grep`, `content:` in all three
interactive faces with a reading indicator.

**The index is the advantage.** Every candidate arrives with its path, size,
mtime and kind before a single file is opened, so the reader can refuse,
order and group files for free: skip a video by its kind, skip what is over
the size limit, send each disk its own files, start with the newest. No other
grep tool has that; every step below should use it before it opens anything.

| the design asks for | state | where |
|---|---|---|
| text, regex, hex | done | `scour-grep/src/pattern.rs`, `expr.rs` |
| fixed buffer, overlap at the seam | done: 1 MiB chunk, 64 KiB carried | `read.rs` (`CHUNK`, `KEEP`) |
| UTF-8 + UTF-16LE in one pass over binaries | done, UTF-16BE too | `pattern.rs` (`Wide.any`) |
| Turkish case variants in the pattern | done | `pattern.rs` (`cases`, `spelled`) |
| printable neighbourhood of a binary find | done | `excerpt.rs` (`around`) |
| no access-time change, no pipe opened | done on Unix | `read.rs` (`open_quietly`) |
| one lane a disk, readers by medium | done, walk's numbers, unmeasured for reading | `ground.rs`, `scan.rs` |
| spinning disk read in an order | path order only | `scan.rs:131` |
| ZIP, DOCX/XLSX/PPTX, ODT, EPUB, UDF | done, **streamed, no cache**, 1.2–1.4 GiB/s | `scour-grep/src/zip.rs`, `scour-extract` |
| read as the asking user (Windows service) | code exists, never run on PC2 | `apps/scourd/src/as_owner.rs`, `Enter` |
| budgets: bytes, time, files, resume | done | `budget.rs`, `engine/grep.rs` |
| classifier by signature | **missing**: a NUL in 8 KiB is the only test | `excerpt.rs` (`Kind::sniff`) |
| "header only" kinds never read | **missing** | — |
| Windows-1254 fallback | **missing** | — |
| hex wildcard `??`, "at file start" | **missing** | — |
| shared normalisation (NFC, soft hyphen join) | **missing** | — |
| PDF, DOC/XLS/PPT, RTF, e-mail | **missing** | — |
| worker process, limits, restart | **missing** | — |
| text cache (zstd blocks, offsets, states) | **missing** | — |
| physical-order hint, low I/O priority, MB/s ceiling | **missing** | — |
| cloud placeholders skipped | **missing** | — |
| cold cache, spinning disk, network, Windows numbers | **missing** | `docs/MEASUREMENTS.md` says so itself |
| a benchmark that writes to the repository | **missing**: `examples/zipcost`, `examples/scan` are the seeds | — |

## 1. Rules for the whole job

- One phase, one branch, one PR, in this working copy — **no new worktree,
  no subagent rounds**. The PR text answers each acceptance line with the
  measurement that meets it. No `Co-Authored-By` line in commits.
- Every build through the queue: `derle cargo check -p …`, `derle cargo test
  -p …`; `derle bash scripts/check` once before a PR. Nothing that only runs
  a built binary goes through `derle`.
- A number is measured with two binaries run alternately, command and table
  in `docs/MEASUREMENTS.md`. Where a number in this plan or in the design
  disagrees with a measurement, the measurement wins; change the constant and
  say why in the commit.
- **Never the owner's live service.** Reflink a copy of the index, start a
  `scourd` on a socket of its own, and wait until `status` says it is not
  scanning before timing anything (an isolated service walks at start even
  with `on_start = false`, and an empty `--state` resets the skip rules).
- **The corpus is private.** It is lists of paths taken from the index
  (`scour --json … | jq -r '.hits[].path'`) kept outside the repository.
  Nothing from a document — no path, no excerpt — goes into the repository,
  a commit message or a PR. Results are counts and times.
- A new dependency is reported before it is merged: licence (MIT, Apache-2.0
  or BSD only without the owner's word), what it pulls in (`cargo tree`),
  what it adds to each binary. A new crate changes `docs/ARCHITECTURE.md` §2
  and, if it must build elsewhere, `PORTABLE` in `scripts/check`.
- A change to the wire, the CLI, the MCP surface or a `scour-core` trait
  changes `AGENTS.md` in the same commit.
- Stop and ask the owner when: a decision conflicts with a measurement, a
  library does not open what it was chosen for, an acceptance line cannot be
  met. Do not widen the scope and do not swap a library alone. Nothing of
  Phase 4 starts without his word.
- Idle CPU of the service is measured at the end of every phase
  (`scripts/bench` reports it) and must not have risen.

## 2. Where the design and the code disagree — the owner decides

Each has a recommendation; carry on with the recommendation **only for
measurement**, and have the answer before building on it.

**D1 — ZIP-based documents are already searched without a cache.** The
design routes DOCX/XLSX/PPTX through `office_oxide` and ODT/EPUB through
Xberg, into the cache. The code inflates and searches them in the reading
thread at 1.2–1.4 GiB/s, 70,000–250,000 files a second. A cache cannot beat
that by enough to pay for its disk and its staleness. *Recommended:* keep the
streamed path for everything ZIP+XML (DOCX, XLSX, PPTX, ODT, ODS, ODP, EPUB,
UDF); the worker and the cache serve only what cannot be streamed — PDF,
OLE2 `DOC`/`XLS`/`PPT`, RTF, e-mail. `office_oxide` is then needed for the
three OLE2 formats only, and Xberg for RTF and e-mail only (HTML is text and
is read as text). Phase 0 measures the claim: the set of files each path
finds, in-house against `office_oxide`, on the same 5,000 DOCX.

**D2 — the physical-order hint and the index.** The design stores eight
bytes a file in the index. The native index is keyed by a path digest and a
row number and holds no such column; adding one is a format change for a gain
the design itself says may be under 20 % and be deleted. *Recommended:*
gather the hint at reading time first — open the lane's files in path order,
ask `FIEMAP` on each descriptor, sort, read — and measure it. Only if that
wins and the gathering itself is what costs does a column follow, as its own
PR.

**D3 — the cache key.** The design says `file_id + mtime + size`. A `Hit`
carries no inode; the acceptance line "the second search touches no original
file" forbids an `fstat`. *Recommended:* the key is the path with the size
and mtime of the index row. A renamed document is extracted again — rare and
harmless. The hole: where the watcher is deaf the row itself can be stale;
on such volumes one `stat` (metadata, not content) confirms the row before a
cached text is used. On exFAT/FAT mtimes are compared in two-second steps.

**D4 — where the extractor lives.** *Recommended:* a binary of its own,
`apps/scour-extractor`, the only thing that links `pdf_oxide`, `office_oxide`
and Xberg, through a new leaf crate. `scourd` stays as small as it is and
its idle memory does not move; the packages (deb, rpm, tarball, Windows)
carry one more file.

**D5 — the parallel reader.** The design names `io_uring` and overlapped
I/O. The code reads with a thread pool and blocking reads and manages 6,800
files a second partly cold. *Recommended:* measure threads beyond eight
first; `io_uring` is built only if an A/B on a cold solid disk shows it a
fifth faster, and then only behind the same `Reader` interface.

Also to confirm, smaller: the buffer stays 1 MiB (the design says 2 MB) until
a measurement says otherwise; archives stay as they are today — ZIP is
already entered, two levels deep, under bounds; TAR and 7Z remain unread.

## 3. How it fits the crates

```
index ──candidates──▶ scour-engine (grep.rs, content.rs)
                          │  rows: path, size, mtime, kind
                          ▼
                     scour-grep ── class.rs      which of five classes, from kind then signature
                          │        ground.rs     lane a disk, readers a medium, order within a lane
                          │        read.rs       text / binary / hex, streamed ZIP documents
                          │        Texts (trait) "give me this document's text" ── None: not cached
                          ▼
            scour-engine implements Texts over two things named in scourd/src/wire.rs:
                 scour-textcache   zstd blocks, offsets, states      (new leaf crate)
                 extractor client  spawn, limits, restart            (in scourd)
                          ▼
                 apps/scour-extractor ── scour-doctext (new leaf: pdf_oxide, office_oxide, Xberg)
```

- `scour-grep` stays a leaf: it gains a `Texts` hook in the manner of
  `Enter` (`scan.rs`) — given a `File`, return the plain text, "no text", or
  "not here". It never learns what a cache or a worker is.
- If the engine needs the cache or the worker as a trait object, the trait
  goes in `scour-core` (`AGENTS.md`: add the trait, not the dependency).
  `holding()` in `content.rs` and `read()` in `grep.rs` are the only two
  places a batch of rows becomes outcomes; both go through `Expr::scan`, so
  the hook is passed once.
- Normalisation is one function in `scour-extract` (`normal.rs`), used when
  text is written to the cache and when a pattern is compiled against cached
  text — nowhere else, so raw files keep being matched byte for byte.
- Disk facts belong to `surefoot::probe` (a repository of its own,
  `ReviseTouch/surefoot`, pinned by tag): anything new there — an encrypted
  volume, a placeholder file, a FUSE driver's name — is a release of
  surefoot first, then a tag bump here. Ask before cutting a release.

## 4. Phases

In order; a phase's acceptance is met before the next begins.

### Phase 0 — a measuring rig, and the libraries on trial

Nothing a user sees changes.

1. `scripts/content-bench LABEL`: one line to run; takes path lists, runs
   `scour-grep`'s `scan` example cold and warm, and writes
   `docs/bench/content-LABEL.txt`: wall time, MiB/s, files/s, peak RSS, CPU
   seconds, counts by outcome. Cold without root: `posix_fadvise(DONTNEED)`
   on each file before the run, in the example itself; say in the output
   when a file would not leave the cache.
2. Path lists, outside the repository: a source tree (plain text), a few
   thousand PDFs, DOCX/XLSX/PPTX, legacy DOC/XLS/PPT, UDF, files with Turkish
   letters in several encodings; and a small made-up set **inside** the
   repository under `crates/…/tests/data` for broken, encrypted and
   bomb-shaped files, generated by a test helper where possible.
3. A scratch crate outside the workspace's default members that builds
   `pdf_oxide`, `office_oxide` and Xberg (default features off; `pdf`,
   `office`, `excel` off). Report: `cargo tree`, licences, whether tokio
   arrives, added binary size, build time.
4. Run each extractor over its list: time a file, failures, crashes, hangs,
   peak memory, and for Turkish text a sample of thirty documents where a
   known phrase must be found.
5. Write an answer under each "to be verified" item of the design's §6 —
   API stability and upkeep of `office_oxide` 0.1.x, Xberg without tokio and
   without its own cache, whether UDF `content.xml` is one run of plain text
   (the code already assumes CDATA: `scour_extract::udf_cdata`), `FIEMAP` on
   single-device Btrfs, on `ntfs3` and on `exfat`. The macOS items are
   recorded as "no machine"; none of the code depends on them yet.
6. Measure D1 (see §2).

*Accepted when:* the bench is one line and its output is in the repository;
the dependency report and the §6 answers are in the PR; D1–D5 have the
owner's answers.

### Phase 1 — classes, encodings, hex, parity with ripgrep

1. `scour-grep/src/class.rs`: five classes — text, document, binary, header
   only, archive. First from what the caller knows (`File` gains an optional
   class hint, which the engine fills from the row's `Kind`; see
   `docs/TAXONOMY.md`), so a video is refused **before it is opened**; then
   from the signature in the first read, which overrules the extension.
   `Kind::sniff` becomes one input to it. New `Skip::HeaderOnly`.
2. Header-only files are read only for a `Needle::Bytes`; documents that
   cannot be streamed and have no `Texts` answer are reported under a new
   `Skip::NoExtractor`, not as binary.
3. Windows-1254: a text file whose bytes are not valid UTF-8 is matched
   against the pattern spelled in 1254 as well — one more alternative beside
   the UTF-16 ones in `Pattern`, compiled only when the needle has a letter
   outside ASCII. The excerpt is decoded the same way.
4. Hex: `??` for any byte and a leading `^` for "at offset zero"
   (`Needle::Bytes` becomes a small sequence of byte-or-any; `raw()` already
   builds byte regexes). The wire's `hex` mode and the CLI take both;
   `AGENTS.md` in the same commit.
5. `scour-extract/src/normal.rs`: lower case with the Turkish pairs, NFC,
   a hyphen at a line's end joined to the next line. Tests for İ/ı, I/i,
   Ş/ş, Ğ/ğ, Ç/ç, Ö/ö, Ü/ü, composed and decomposed. NFC needs a table:
   report the dependency like any other.
6. Windows: `std`'s `File::open` already shares read, write and delete; add
   the sequential-scan hint, and tell a sharing violation apart as
   `Skip::Locked`, counted beside `binary` and `too_big` in `Grepped`.
7. One file identity read once in a search: the engine drops rows whose
   `EntryId` repeats; a test with a hard link and one with a symlink loop.

*Accepted when:* on the source-tree list, text and regex find the same set
of files and lines as `rg --no-ignore --hidden -a`-equivalent settings (write
the exact flags down; differences are listed and explained, none
unexplained) in at most 1.5× its time; a 300 MB file without one newline is
searched with peak RSS under the readers' buffers plus a fixed allowance; a
word stored as UTF-16LE inside a Windows executable is found with `--binary`;
the Turkish tests pass.

### Phase 2 — what each disk can bear

1. `readers()` in `ground.rs` gets numbers of its own, measured: solid at
   4/8/12/16/cores, network at 4/8/16, spinning at 1/2. The table from the
   design's §3 is the starting point, the measurement the result.
2. Small candidate sets (start: under 2,000 files or 64 MiB, known from the
   rows) are read directly, unsorted, whatever the medium.
3. Spinning lanes: the hint of D2, behind one function
   `order(files, medium)` so a column in the index could replace it later.
   Fall back to the path order there is today. **If the cold gain over path
   order on more than 10,000 files is under 20 %, delete the code.**
4. Low priority for a long reading: `ioprio_set` idle on Linux — note that
   it does nothing under the `kyber` scheduler this machine uses, so also an
   optional MiB/s ceiling in `Budget`; background mode on Windows. A reading
   is "long" once it has run past the first page.
5. Placeholders are refused before opening: Windows
   `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS`, macOS `SF_DATALESS`; FUSE and
   network roots are not read for content unless the volume is switched on
   for it — a per-volume setting beside `volumes_on`, default off for
   `Medium::Network`. Wire and `AGENTS.md` in the same commit.
6. Snapshot directories and subvolumes: check what the walk already keeps
   out of the index; add nothing if they never become candidates.
7. Fill the gaps `docs/MEASUREMENTS.md` names: cold cache alone, a spinning
   disk (ask the owner which), an SMB share, Windows on PC2 including
   reading as the owner under the SYSTEM service.

*Accepted when:* the spinning-disk line above is decided by a number;
parallel reading on a solid disk scales against one thread, with the curve
recorded; the desktop stays responsive during a full cold reading (record
how that was judged); every threshold in `ground.rs` carries a measured
number.

### Phase 3 — documents that cannot be streamed, and their cache

1. `crates/scour-doctext` (leaf): `fn text(bytes, format, sink)` over
   `scour_extract::Sink`, with `pdf_oxide`, `office_oxide` (per D1) and
   Xberg behind it. No fallback from one to another: a file the chosen
   library cannot open is `failed`. A PDF with no text layer is `no text`.
2. `apps/scour-extractor`: started once by the service, length-prefixed
   requests over its standard streams, one file at a time a process (several
   processes for background extraction). It opens files itself, read-only,
   under the same identity rule as the readers.
3. Limits, enforced from outside: 30 s a file (kill and restart), an address
   space limit (`setrlimit` / a job object), 256 MiB of output, 100:1
   against the file's size. A crash or a kill marks that file `failed` and
   the service goes on.
4. `crates/scour-textcache` (leaf): append-only zstd blocks of 1–4 MiB at
   level 19, a table of key → block, offset, length, state (`ok`,
   `no text`, `failed(library version)`). Failed entries are tried again
   only when the library version changes. Dead space over 30 % rewrites the
   blocks in the background. Files are created `0600`, the directory `0700`,
   under the state directory by default.
5. The `Texts` hook (§3): a cached text is searched as text with the
   normalised pattern; a document not cached is extracted first, inside the
   same budget, and the reading indicator counts it. A search decompresses
   blocks on the readers' threads.
6. Settings through the existing `settings` request: cache on/off, largest
   size, place, extract ahead when idle (low priority, half the cores), and
   "clear the cache". Faces get the controls in the Index section of
   Settings; wording goes through `scour-i18n`, English msgids.
7. The preview may read PDF text through the same worker afterwards; not in
   this phase.

*Accepted when:* on the document list the second search opens no original
file (shown with `strace -e openat` or a counter in the test build); a
changed document's old text is never returned; a broken file is tried once
and takes nothing down; cache size and search time are in
`docs/MEASUREMENTS.md`; the hit rate of multi-word phrases in PDFs is
measured on a sample.

### Phase 4 — only with a measurement and the owner's yes

Unchanged from the design's table: raw NTFS reads, resident MFT files,
inline Btrfs files, TAR/7Z, section-aware binaries, a trigram prefilter, a
second PDF engine. For each: the measurement first, then the question.

## 5. Rules that speed never bends — one test each

| rule | test |
|---|---|
| A reader that may not read a file gets neither its lines nor its cached text, MCP included | a file `0600` of another user; a cached text asked for by someone who cannot read the original |
| Cache files are the owner's alone | mode bits after a write |
| Text of a document on an encrypted volume is not written to a plain one | a faked volume table: not cached, extracted each time |
| A stale entry is not used | change size, change mtime, each alone |
| Bounds on entry size, output size, ratio | a generated bomb for ZIP and for the extractor's output |
| Links and repeats | a symlink loop; two names of one file read once |
| No side effects | atime unchanged after a search; no file opened for writing (`strace` in a test script); a placeholder not opened |

The first rule needs care on Linux: find out whether `scourd` ever runs as
another user than the one asking (`packaging/scourd.service`,
`scour@.service`, `50-scour-service.rules`) and how a request's sender is
known; `as_owner.rs` is the Windows half.

## 6. Order of work, in one line

Phase 0 and the five answers → Phase 1 (no new dependency but NFC) → Phase 2
(numbers, mostly) → Phase 3 (three libraries, two crates, one binary).
