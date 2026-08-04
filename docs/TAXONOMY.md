# File kinds

`Kind` decides what `kind:` matches, what the facet rail lists and how the disk
report groups bytes. It is also a `u8` in an on-disk column, which makes it the
one enum in this codebase that cannot be tidied up later.

Everything below was measured on 1,644,841 entries and 473.8 GB under
`/home/hasan`, against the live filesystem rather than a name list — so
`is_dir`, `mode` and size are exact.

## What is wrong today

| | unknown by count | unknown by bytes |
|---|---|---|
| `kind_of` as it stands | 892,931 — **54.29%** | 341.4 GB — **72.05%** |
| the taxonomy below | 103,796 — **6.31%** | 3.9 GB — **0.82%** |

One change does most of that: **on a developer's machine half of all files are
build output** — 891,060 entries (54.17%) and 311 GB (65.67%), every one of them
currently `Kind::File`.

A correction to an earlier note in this repository: the "18.5% of files have no
extension" figure counted directories. Extensionless *files* are **7.41% of
files**, 5.95% of entries. The extensionless problem is a third the size it
looked.

## The kinds

**Discriminants 0–7 keep their numbers and their meaning. Nothing is renumbered,
nothing is reused.** New kinds are appended from 8.

| # | identifier | token | msgid | what belongs in it | count | bytes |
|---|---|---|---|---|---|---|
| 0 | `File` | `file` | File | Nothing matched. The honest unknown. | 6.31% | 0.82% |
| 1 | `Dir` | `folder` | Folder | `is_dir`, decided before anything else. | 12.88% | — |
| 2 | `Code` | `code` | Code | Source a person wrote: languages, shells, templates, stylesheets, shaders, IDL. | 11.39% | 0.92% |
| 3 | `Image` | `image` | Image | Raster, vector, camera raw, layered editor documents. | 3.54% | 0.14% |
| 4 | `Archive` | `archive` | Archive | A container of other files: archives, OS packages, disk and VM images. | 0.46% | 3.59% |
| 5 | `Doc` | `doc` | Document | Something a person reads: office, PDF, plain text, markup prose, ebooks, mail, subtitles. | 3.07% | 0.20% |
| 6 | `Exec` | `exec` | Executable | Machine code the OS can load and run. | 1.56% | 27.42% |
| 7 | `Media` | `media` | Media | **Retired.** Never produced again; still decoded from old indexes. | — | — |
| 8 | `Audio` | `audio` | Audio | Sound without video, plus playlists. | 0.01% | 0.02% |
| 9 | `Video` | `video` | Video | Moving pictures. | 0.00% | 0.01% |
| 10 | `Build` | `build` | Build output | Machine-produced and tool-regenerable. Deleting it costs time, not information. | **54.17%** | **65.67%** |
| 11 | `Data` | `data` | Data | Structured machine-readable content: serialisation, tabular data, schemas, databases, model weights, logs. | 5.12% | 1.14% |
| 12 | `Config` | `config` | Configuration | Settings that change how a program behaves, plus certificates and keys. | 1.40% | 0.03% |
| 13 | `Font` | `font` | Font | Typefaces. | 0.09% | 0.04% |

Fourteen values means four bits instead of three in the packed column: **+205 KB
at 1.6 M rows**, against a `cols` blob already tens of megabytes. Not a
consideration.

### `from_name` has to return a set

Splitting `Media` breaks the current signature, because `kind:media` must keep
matching rows in old indexes that literally hold 7:

```rust
pub fn from_name(folded: &str) -> Option<&'static [Kind]>
```

`media` → `[Audio, Video, Media]`, `text` → `[Code, Data, Config, Doc]` as a
group alias with no discriminant of its own, and every token accepted today
keeps working. `Match::Kind` in `scour-query` carries a slice or a small bitset.
This is the only non-additive API change here, and Windows `System.Kind` is
multi-valued for the same reason.

## Why these lines and not others

**`Build` is a format, not a provenance.** GitHub Linguist is right that
generated-ness should be a flag rather than a category — it keeps exactly four
types and three orthogonal booleans — and freedesktop says the same in its spec:
*"Subclassing is about the format, rather than the category of the data."* That
argument does not reach `.o`. An ELF relocatable object *is* a distinct kind of
thing, the way a PNG is. A `.rs` file inside `target/` is a Rust file that
happens to be generated, and *that* is provenance.

So both mechanisms are needed and they are different:

* **`Kind::Build`** — format-intrinsic, cheap, in `kind_of`. This document.
* **A `derived` bit** — location-intrinsic, set by the scanner on entering
  `target/`, `node_modules/`, `build/`, `__pycache__/`, `.gradle/`, `dist/`.
  A separate column and a `derived:` predicate. Recommended, not costed, not
  here.

The clearest evidence they are different: the largest single lump of unknown
bytes on this disk is **10.3 GB of files named `build-script-build`**, plus some
40 GB of hashed test binaries. Those are ELF executables — `Exec` is the correct
answer for *what they are*. Only a location rule can also say they are
throwaway.

No product surveyed has a build-output kind: not Everything, not Windows
`PerceivedType` or `System.Kind`, not UTI, not FSearch. The data says be first.

**Splitting `Media` is unanimous prior art.** Apple splits
`public.audio` / `public.movie`; Windows `PerceivedType` has both; `System.Kind`
has three; Everything ships `audio:` and `video:` separately; freedesktop has
`audio/*` and `video/*`. On *this* machine Audio is 129 files and Video is 19 —
that is evidence the corpus is a developer's disk, not evidence against the
split. A taxonomy where `.mp3` and `.mkv` share a bucket cannot answer "how much
music do I have", which is one of the questions the report screen exists for.

**`.json` and `.xml` go to `Data`, not `Code`.** Linguist types JSON as `data`;
`UTTypeJSON` conforms to `UTTypeText` and the documentation goes out of its way
to say it does *not* conform to `UTTypeJavaScript`. The practical argument is the
histogram: leaving them in `Code` is why `kind:code` is 27.41% of this machine,
more than half of it not source. Moving them out drops `Code` to 11.39% and
everything left is something a person wrote. `.yaml`/`.toml`/`.ini` go to
`Config` instead — the least certain call in the table, and the reverse is
defensible.

**`.html` goes to `Doc`, and generated documentation goes to `Build`.** Of
189,555 HTML files here, **158,567 (83.7%) are rustdoc or javadoc item pages**
(`struct.*`, `enum.*`, `fn.*`, `package-summary`) and most of the remainder is
dartdoc. Linguist calls HTML markup, not programming; Everything puts it in
`doc:`. So: `.html` → `Doc`, plus a name rule on `.html`/`.htm` that fires
before the extension lookup and yields `Build`. Worth **+9.8 percentage
points** — `Doc` falls from 12.89% (78% of it useless) to 3.07%.

That rule is a heuristic and should be labelled one: it hard-codes two
toolchains' conventions, misses dartdoc, and would misfire on a hand-written
`fn.html` — requiring two dots in the prefix form shrinks that surface. Linguist
itself catches rustdoc by *path* rather than by name. When the `derived` flag
lands, delete the rule.

### Considered and rejected

| candidate | verdict | why |
|---|---|---|
| `Db` | fold into `Data` | Even Apple's `public.database` is a parentless abstract root. No one distinguishes "a database" from "a data file" while searching. |
| `Disk` (ISO, VMDK, VHD) | fold into `Archive` | **The closest call.** An ISO and a ZIP are both containers you do not open directly. But a 200 GB VM disk deserves its own line in a report; if `Archive` ever shows up dominated by one `.vmdk`, split it — the discriminant is free. |
| `Model` (3D/CAD) | leave out | **`.obj` collides with MSVC object files**, and object files outnumber Wavefront meshes by five orders of magnitude on any machine that has both. A kind whose flagship extension is a landmine is not worth having. |
| `Book`, `Sheet`, `Slides` | fold into `Doc` | Sub-kinds. If wanted, a second facet column — not a second discriminant. |
| `Log`, `Cert`, `Key` | fold into `Data` / `Config` | Real intent, wrong mechanism: `ext:log` already answers it. |
| `Temp`, `Backup` | leave out | `foo.rs.orig` is still Rust. A *state*, not a type — same axis as `derived`. |
| `Link` | leave out | A symlink to a PNG is both. `mode_string()` already renders the `l`; this is a predicate on the `Mode` column. |

Two findings that shaped the shape of this: **Everything has no source-code
filter at all** — its `doc:` macro puts `c;cpp;h;java;py;json;xml` in the same
bucket as `docx;pdf;xlsx` — so having `Code` and `Data` separate is a plain win
over the product Scour is measured against. And **disk-usage tools mostly do not
categorise**: WinDirStat's colours are top-N-by-size, WizTree and `dust` group by
raw extension, `ncdu` has no notion of type. Only TreeSize has named groups and
does not publish the list. Grouping the report by `Kind` is a differentiator,
which raises rather than lowers the bar on getting `Build` right.

## Classification order

```
1.  is_dir                            -> Dir
2.  ext = ext_str(name)
    2a. ext is html|htm and the name matches the generated-doc pattern
                                      -> Build
    2b. EXT_TABLE[ext]                -> that kind
    2c. non-empty but unknown         -> fall through to 4
3.  ext is empty
    3a. NAME_TABLE[fold(name)]        -> that kind    (exact, includes dotfiles)
    3b. STEM_PREFIX[fold(name)]       -> that kind    (readme*, license*, …)
4.  mode & 0o111 != 0                 -> Exec         (unix; mode is 0 on Windows)
5.                                    -> File
```

Four properties of that order, each of which matters:

* **`is_dir` first, always.** On macOS `.app`, `.xcodeproj`, `.framework`,
  `.bundle`, `.lproj` and `.xcassets` are *directories* — this corpus has 196
  `.xcodeproj` and 204 `.lproj`. Classifying them by extension would file
  directories under `Exec`.
* **The exec bit is last and never overrides an extension.** Not theoretical:
  **17,073 files here carry `u+x`, among them 3,408 `.png`, 268 `.xml`, 62
  `.ttf` and 46 `.rs`** — the residue of NTFS mounts and bad umasks. The current
  code already gets this right; do not "improve" it.
* **The exec bit is worth less than it looks.** Only 5% of extensionless files
  carry it — but those 5% are 98 GB. Low recall, high value.
* **On Windows `mode` is 0**, so step 4 never fires and no false `Exec` appears.
  The cost is an extensionless ELF in a WSL tree being unclassifiable there,
  which is honest and small.

Names are compared through `DefaultFolder`, not `to_lowercase` — otherwise
`LİCENSE` misses in a Turkish locale. The histogram this was derived from
contains `lıcense-mıt` and `meta-ınf` precisely because it was built with
`awk tolower` under `tr_TR`, which is the bug this rule exists to avoid.

### What is deliberately left unrecognised

The top extensionless files here are Cargo fingerprints — `stderr` (2,411),
`root-output`, `output`, `build-script-build` (1,823), and some 40,000 of the
form `lib-<crate>` / `dep-lib-<crate>`. Together roughly 40% of all extensionless
files. Recognising them needs prefix rules on `lib-`, `dep-`, `run-build-script-`
and bare `stderr`/`output`, and every one of those is a plausible real filename.
`.cargo-ok` is exact and safe; **the rest stay `File`.** One rule at the
directory `target/` would catch all forty thousand with no false positives, which
is the clearest argument in this document for the `derived` flag.

### A pathology worth a test

`ext_str` takes the rightmost dot, so a directory named `2.2.20` has extension
`20` and `license-apache-2.0` has `0`. Purely numeric extensions are **7,163
entries (0.458%)** and every one is a version number. They land in `File`, which
is correct by accident. Leave it — `ext_str`'s contract is "text after the last
dot" and it is shared with the `ext:` term — but write the test, because a facet
list reading `0: 2,031` will confuse someone.

## Migration — **done**, and this is what it took

Nothing crashes, and that is the problem. Ship the new binary against an old
index and `from_u8` still resolves 0–7, so no row is unreadable — but every row
that should be `Build` is stored as `File`, `kind:build` returns nothing, and
the sidebar reads `File: 54%`, which looks like a broken feature. Worse, every
`.mp3` is stored as `Media`: `kind:media` still finds it and `kind:audio` finds
none, silently.

**Nothing is corrupt and everything is wrong.** So:

* **`FORMAT` is 5.** (This said 3 to 4 when it was written; the per-directory
  distance byte took 4 first.) `NativeIndex::open_or_create` refuses a
  mismatched index and `scourd` discards and rescans, which is 2 seconds here.
* **`Error::IndexOutdated { found, expected }` already exists**, added with that
  bump, and `apps/scourd/src/wire.rs` acts on it by discarding and rescanning.
  A stale index is not a corrupt one, and the difference matters when the
  rebuild is unattended and takes minutes: the frontend should say
  "reindexing", not "your index is damaged". Nothing is needed here but the
  bump itself.
* A lazy migration is not possible — recomputing `Kind` needs the name *and* a
  rewrite of the block-packed column for every segment, which costs what a
  rebuild costs without picking up everything else that changed on disk.
* A compatibility shim mapping old `File` to `Build` cannot work: old `File`
  genuinely contains both.

And whatever else is decided, put the rule in the source where it will be read:

```rust
/// The numeric values are part of the on-disk index format.
///
/// **Never renumber a variant and never reuse a discriminant.** A retired kind
/// keeps its number forever and stays decodable; new kinds are appended. When
/// the *meaning* of an existing number changes, `FORMAT` in
/// `scour-index-native` is bumped and every index is rebuilt.
```

Seven new msgids for `lang/tr/LC_MESSAGES/scour.po`; `"Media"` stays so old
indexes still render. `crates/scour-query/src/syntax.rs` lists the accepted
`kind:` values inline and must change in the same commit.

## Where `File` is the honest answer

6.31% is not a failure; it is the residue after everything knowable is known.

1. **Extensionless files not on a name list** — 92,368, the bulk of it. Cargo
   fingerprints, Git objects. Location-determined; fixing them by name is
   guessing.
2. **False extensions** — `4.1.93.final`, `d4e342018b23d58be902a60e67105aa1`.
   A dot with no type behind it.
3. **Genuinely private formats** — `.colpan`, `.propcol`, `.mca`, `.dfa`.
   Adding them would be curve-fitting to one machine. `ext:colpan` answers it.
4. **Test fixtures** — `.stderr` *with* an extension (2,609) is `trybuild`'s
   expected compiler output sitting next to the `.rs` that produces it:
   authored, not generated, and the exact opposite of the extensionless `stderr`
   in `target/`. Same word, two meanings, resolvable only by location.
5. **Backups and merge leftovers** — `.orig` (2,130), `.bak`, `.swp`. A state,
   not a type.
6. **Below the measurement floor.** The top 200 extensions cover 80.88% of
   entries; all 1,811 cover 81.46%. The 1,611-extension tail is worth 0.58
   points. The tables stop where the evidence stops.

The rule to keep: **an extension goes in a table when it means one thing.** `.o`
means one thing. `.bin` means three, and it is in `Build` only because 60,123 of
them were checked by name and were all caches — an exception to be documented as
one, not a pattern to repeat.

## Least certain

* **`Config` as a kind** — justified by intent rather than mass, and its border
  with `Data` is genuinely fuzzy. First to cut.
* **`Font`** — clean and cheap, but 0.09% and no competitor has it.
* **Disk images inside `Archive`** — right by count, possibly wrong by bytes
  once someone with virtual machines runs the report.
* **The generated-HTML rule** — worth 9.8 points today, and an acknowledged
  interim.
* **`.mts` → Code over Video**, and **`bat`/`cmd` → Exec while `sh`/`ps1` →
  Code.** Coin-flips resolved by frequency, not principle.
* **The corpus is one developer's Linux machine.** Every number here is real and
  none of it is representative of a photographer, a musician or an office user.
  The `Audio`, `Video`, `Doc` and `Image` tables are sized for *them*, on prior
  art rather than on this histogram, and that is on purpose.

## Sources

freedesktop.org shared-mime-info (spec §2.11, 1,040 types) · Apple Uniform Type
Identifiers · GitHub Linguist `languages.yml` and `generated.rb` · Everything
1.5 `Filters.csv` defaults · FSearch `fsearch_filter.c` · Windows
`PerceivedType` (11 values) and `System.Kind` (23, multi-valued, not extensible)
· TreeSize File Groups · GNOME Files `nautilus-mime-actions.c` · WinDirStat,
WizTree, ncdu, dust.
