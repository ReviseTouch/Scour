//! The language, written down.
//!
//! This text is served verbatim by the `scour_syntax` MCP tool and printed by
//! `scour syntax`. It lives next to the parser so that a change
//! to one is visibly a change to the other; the smoke tests check that every
//! example in it actually parses to what it claims.

/// Reference for the Scour query language.
pub const SYNTAX: &str = r#"# Scour query syntax

A query is a list of terms separated by spaces. Every term must match.

## Structure

| Syntax | Meaning |
|---|---|
| `a b` | both must match (AND) |
| `a|b` | either may match (OR) |
| `a;b` | the same thing — `;` and `|` are one operator |
| `!a` | must not match (NOT) |
| `"two words"` | exact phrase; wildcards inside are literal characters |
| `*` | any run of characters |
| `?` | exactly one character |
| `(a | b) c` | the same as `a|b c`, written with room to breathe |

Parentheses group **only when there is a `|` inside them**. A parenthesis is an
ordinary character in a filename and a common one — `rapor (1).pdf` — so
anywhere else they are text. Nesting is not supported: a query is groups
AND-ed together and a group is alternatives OR-ed, which is one level by
construction. `(a|b) (c|d)` works; `(a (b|c))` would need a tree.

A bare word matches any part of the file name. A pattern containing `*` or `?`
matches the name **end to end**: `*.rs` matches `main.rs` but not `main.rst`.

## Fields

| Syntax | Meaning |
|---|---|
| `ext:rs` | extension is `rs` |
| `ext:rs;toml;md` | extension is any of these |
| `ext:rs ; toml` | the same — spaces around a `;` inside a field's value are closed up |
| `path:src/api` | the full path contains this |
| `under:/home/u/Projeler` | anywhere below this folder |
| `parent:/home/u` | directly inside this folder, one level down |
| `file:` | files only |
| `folder:` | folders only |
| `size:>1mb` | larger than a megabyte |
| `kind:code` | one of: folder, code, doc, image, data, config, archive, exec, audio, video, font, build, file |
| `dm:7d` | modified in the last 7 days |
| `dc:2026-01-31` | created on that day |
| `da:>2026-01-01` | accessed after that day |
| `content:invoice` | document contents contain this (only if content indexing is on) |

## What the filesystem knows

The columns an index has always held and no query could name. This is what
`find` gets reached for, and it answers from the index instead of from a walk.

| Syntax | Meaning |
|---|---|
| `node:l` | what the filesystem made it: `f` file, `d` folder, `l` symlink, `s` socket, `p` fifo, `b` block, `c` char |
| `perm:644` | permissions are exactly these |
| `perm:-200` | has **all** of these bits |
| `perm:/222` | has **any** of these bits |
| `suid:` `sgid:` `sticky:` | the three special bits, by name |
| `ww:` | world-writable — `perm:/002`, spelled the way it is asked about |
| `user:root` `user:1000` | owned by this user, by name or number |
| `group:wheel` `group:0` | owned by this group |
| `items:=0` | how many entries a folder holds |
| `depth:3` | the path has exactly three components; `depth:<=4`, `depth:>8` |
| `regex:^[0-9]{4}-` | the name matches this regular expression |

`depth:` counts `/` from the root: `/home` is 1 and `/home/u/a.rs` is 3. A bare
number means **exactly** — unlike `size:`, where a bare number means "at
least", because nobody looks for a file of exactly one megabyte and everybody
means "three deep" by `depth:3`.

`regex:` is anchored nowhere, exactly as `grep -E` is: `^main` and `\.rs$` both
say what they look like. It runs against the folded name, so it is
case-insensitive like everything else — including Turkish dotted and dotless
i, which no `(?i)` flag gets right. **Nothing narrows it**: a pattern says
nothing a trigram index can read, so put a cheaper term beside it when the set
is large.

`kind:` and `node:` are different questions and both are worth asking.
`kind:` is what a file **is** — a document, an image, code. `node:` is what
the filesystem **made** it, and it is the only one that can tell a symlink
from what it points at.

Names are resolved on the machine holding the index, from `/etc/passwd` and
`/etc/group`, because that is where the answer to "who is `root`" lives.

**The permission bits are only as true as the filesystem.** A mount that
supplies them — NTFS, the FAT family — has them withheld at index time rather
than stored, so `perm:` and `suid:` find nothing on such a volume instead of
finding everything. `node:` is unaffected.

`;` means "any of these" wherever it appears — inside a field's value and
between words alike. `ext:rs;toml` is "extension is rs or toml" and
`opus;sonnet` is "named opus or sonnet"; one mark, one meaning. Inside quotes
it is an ordinary character: `"a ; b"` looks for that text.

## Sizes

Binary units: `b`, `kb`, `mb`, `gb`, `tb`. Operators `>`, `>=`, `<`, `<=`, `=`.
Without an operator, `size:1mb` means "at least 1 MB".

## Dates

`dm:` modified, `dc:` created, `da:` accessed.

* Relative: `24h`, `7d`, `2w`, `6m`, `1y`, or `today`, `yesterday`, `week`,
  `month`, `year`. These mean "within the last N", counted back from now.
* Absolute: `YYYY-MM-DD` in UTC. Without an operator it means *on* that day;
  with one (`dm:>2026-01-01`) it means what it says.

## Matching

Search is case-insensitive. Turkish `i`, `ı`, `I` and `İ` are treated as the
same letter, so `ISTANBUL`, `İstanbul` and `ıstanbul` all find each other.

`under:` and `parent:` are the exception: they take a path and compare it the
way the filesystem stores it. They are also much faster than `path:`, because
the index holds every ancestor folder as a term — prefer them for scoping.

Terms shorter than three characters cannot be answered by the index and are
rejected: it is built on trigrams.

## Notes for automated callers

* The parser never fails. An unrecognised field, or one whose value will not
  parse, is searched for as literal text — `size:abc` looks for the string
  "size:abc", and so does `boyut:1mb`, because there is no `boyut` field. If a
  query returns something unexpected, ask for its description to see how it was
  actually read.
* Field names are case-insensitive and folded like everything else, so `EXT:`,
  `Ext:` and `ext:` are one field, and `TÜR:` is `tür:`.
* `C:/Users` and `http://example` are not fields: a field name is two or more
  letters, of any alphabet, and nothing else.
* Result counts are capped by default. A response marked `capped` means "at
  least this many", not "exactly this many".

## Examples

    rapor ext:pdf dm:30d          PDFs with "rapor" in the name, last 30 days
    *.log size:>100mb             log files over 100 MB
    folder: node_modules          folders named like node_modules
    path:src ext:rs !test         Rust files under src, excluding tests
    kind:image dm:today           images touched today
    under:/home/u/Projeler *.rs   Rust files anywhere in one project tree
    "annual report" ext:docx;pdf  an exact phrase, in two formats
"#;
