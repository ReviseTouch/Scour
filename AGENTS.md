# Scour — working agreement

The contract. A change to the CLI surface, the MCP surface, the wire protocol
or a `scour-core` trait changes this file in the same commit. When this file
and the code disagree, the code is a bug.

## The one architectural rule

**Nothing but `scour-core` may be depended on by more than one layer.** Core
is `types/` and `traits/`, with no I/O and no dependency beyond `serde` and
`bitflags`. Everything else is an implementation in its own crate:

| layer | crates | knows about |
|---|---|---|
| contract | `scour-core` | nothing |
| language | `scour-query` | the syntax, no backend |
| wire | `scour-proto`, `scour-ipc` | the message shape, the socket |
| implementations | `scour-index-native`, `scour-source-fs`, `scour-config`, `scour-i18n`, … | one technology each |
| orchestration | `scour-engine` | `Box<dyn Source>`, `Arc<dyn Index>` — no concrete types |
| wiring | `apps/scourd` | **the only place concrete types are named** (`src/wire.rs`) |
| faces | `scour`, `scour-mcp`, `scour-web`, `scour-gui`, `scour-tui` | `scour-proto` only |

If an implementation crate needs another implementation crate, the trait it
needs is missing from core. Add the trait, not the dependency. `scour-engine`
never contains the word `notify`, `ignore`, or the name of an index.

## Language

Identifiers, comments, tests, commits: **English**. User-facing text is an
English msgid translated through `scour-i18n`; the core returns typed errors,
never sentences. Turkish *behaviour* stays: case folding (`İ`/`ı`), the Turkish
query aliases, locale-aware ordering.

## Layout

`lib.rs` is `mod` and `pub use` only. A file is named after the one concept in
it — no `utils.rs`. About 300 lines a file, 40 a function; exceed on purpose,
not by accident. Tests in `tests/smoke.rs` against the public API; inline
`#[cfg(test)]` for internals. New feature, new test.

## Measurement

A performance claim carries the command that produced it, in
`docs/MEASUREMENTS.md`, with a before and an after from two binaries run
alternately. `scour-index-native` checks every query shape against
`scour-mock::brute_force`; three fast wrong answers were caught only there.

## Comments

For the reader who did not write the code, and short. A doc comment says
**what**, in one or two lines. An inline comment states an **invariant or a
number** the code cannot show, in one line. **No history** — git is the
archive. No Turkish. `scripts/comments` reports density; about one comment line
per ten of code, and no block longer than three lines.

## Surfaces

### Wire — `scour-proto`

Newline-delimited JSON over a local socket; a reply carries the `id` of its
call. Queries cross as **text**: the service parses, so the language means one
thing. `Response` is internally tagged, so a variant holds a struct or named
fields, never a bare string; `every_response_round_trips` guards it.

| op | writes | |
|---|---|---|
| `search` | | `query`, `sort`, `descending`, `page{offset,limit,count_cap}` → rows, `rows_visited`, `rows_built` |
| `count` | | `query`, `cap` — a floor when `capped` |
| `facets` | | `query`, `by`: a list of `kind`/`ext{top}`/`dir{path,top}`/`age{edges}`, one walk for all of them; any `age` group lifts the 200,000-row sampling cap |
| `tree` | | `path`, `depth`, `limit` — bounded per level |
| `stat` | | `path`, answered from the source |
| `usage` | | `path`, `top` → what a subtree weighs; hard links charged `disk / links` per name |
| `explain` | | `query`, `cursor` → the sentence, the coloured spans, completions — without running it |
| `duplicates` | | `under`, `min_size`, `read_budget`, `top` → same-size groups and how many were **read** and proved identical |
| `sources`, `status`, `stats`, `syntax`, `places`, `rules` | | facts, never sentences |
| `settings` / `set-settings` | ✓ | what a person chose — columns, widths, sort, language, layout, which face opens. A change names only what it touches |
| `await` | | `since`, `timeout_ms` → a `Status` when the index would answer differently. One blocked thread instead of a poll; also what tells the service somebody is looking |
| `thumbnails` | ✓ | starts the desktop's own thumbnailers, at most four at once — bounded here because the bound is the machine's |
| `rescan`, `maintain`, `shutdown` | ✓ | `maintain` is `flush` / `idle` / `compact` / `rebuild` |

`took_us` covers parsing, cache lookup, the index call and folder-size
enrichment; not transport or rendering.

### Command line — `scour`

A client only. On the bare form everything after the query is the query, flags
included; `-n` and `-s` go before it. `--json` prints the protocol type as the
service sent it; `--socket` picks another service. `where` and `mcp-config`
reach no service. At a terminal five rows are shown, into a pipe forty, and the
summary says how to get more.

### MCP — `scour-mcp`

Read-only tools, all bounded, and each says when it was cut. Mutating requests
are refused in the one `call` every tool goes through — `Request::is_mutating`
— not merely left off the list.

### Browser — `scour-web`

One page and its JSON routes; no framework, no async runtime. **127.0.0.1
only**, a token per run, `Origin` checked, `GET` reads and `POST` acts.
`/api/open` runs executables through the index's own `stat` fence; `--no-run`
reveals the folder instead, `--no-launch` removes the route, `--no-thumbnails`
removes `/api/thumb`. `rescan` and `maintain` are not routed.

### Window — `scour-gui`, terminal — `scour-tui`

Faces like the CLI: no index, no engine. The window never waits — every call is
on a worker lane and comes back as an event; a stale answer is dropped by its
generation, never drawn. Neither parses queries: what a term means is
`explain`'s answer, with two narrow exceptions — `terms_of` (which words to
highlight) and `scour_query::without` (a rail dropping its own term).

### Service — `scourd`

`src/wire.rs` names the concrete index and source; nothing else does. Change
feeds are hints: every source gets a full reconciliation pass
(`reconcile_interval_secs`, default 1800; `poll_interval_secs`, default 60,
for a source with neither a watch nor a pulse), resting at least twenty times
the previous pass's cost. The privileged `scour-watch` helper places the
`fanotify` marks, drops privilege and execs the daemon.
