# Scour — working agreement

This file is the contract. If a change alters the CLI surface, the MCP surface,
the wire protocol, or a `scour-core` trait, this file changes in the **same
commit**. When this file and the code disagree, the code is a bug.

## The one architectural rule

**Nothing but `scour-core` may be depended on by more than one layer.**

`scour-core` contains only `types/` (shared vocabulary) and `traits/`
(interfaces). It has no I/O, no filesystem knowledge, no search-engine
knowledge, and no dependency beyond `serde` and `bitflags`.

Everything else is an *implementation* and lives in its own crate:

| Layer | Crates | Knows about |
|---|---|---|
| Contract | `scour-core` | nothing |
| Language | `scour-query` | the query syntax, no backend |
| Wire | `scour-proto`, `scour-ipc` | the request/response shape, the socket |
| Implementations | `scour-index-native`| one concrete technology each |
| Orchestration | `scour-engine` | `Box<dyn Source>`, `Box<dyn Index>` — no concrete types |
| Wiring | `apps/scourd` | **the only place concrete types are named** |
| Frontends | `apps/scour`, `apps/scour-mcp`, later the GUI | `scour-proto` only |

Consequences that are not negotiable:

- `scour-engine` must never contain the word `notify`, `ignore`, or the name
  of any index implementation. It had a dead `scour-index-tantivy` dependency
  in its manifest for a while, which nothing caught because nothing checked.
- Swapping the search engine must be a one-line change in `apps/scourd/src/main.rs`.
- If an implementation crate needs another implementation crate, the abstraction
  it actually needs is missing from `scour-core`. Add the trait; do not add the
  dependency.

## Language

- **Identifiers, comments, doc comments, log messages, test names, commit
  messages: English.** A new Turkish comment is a regression.
- **User-facing text: English msgid, translated through `scour-i18n`.** The core
  never composes a human sentence — it returns a typed error or code, and the
  frontend turns it into words in the user's language. This is what lets the
  CLI, MCP and GUI all speak correctly without duplicating strings.
- Turkish *behaviour* is load-bearing and stays: case folding (`İ`/`ı`),
  the Turkish query-field aliases, locale-aware sorting.

## Layout conventions

- `lib.rs` and `mod.rs` contain `mod` + `pub use` only.
- `types/` holds structs and enums; `traits/` holds traits.
- A file is named after the single concept inside it. No `utils.rs`,
  `helpers.rs`, `misc.rs`, `common.rs`.
- Target ~300 lines per file and ~40 per function. Exceeding it is allowed when
  splitting would hurt; exceeding it by accident is not.
- Tests go in `tests/smoke.rs` against the **public** API. Inline `#[cfg(test)]`
  is for internals that have no public surface.
- New feature = new test.

## Measurement

Performance claims in this repository are measurements, not opinions. Numbers
live in `docs/MEASUREMENTS.md` with the command that produced them. A change
that claims to be faster cites a before and an after.

The verifier is not optional: `scour-index-native`
`scour-mock::brute_force`. Three real bugs — a capped count that bounded the
value but not the work, a segment tail cut at page size, and deleted documents
reappearing — were invisible to timing and only caught by that comparison.

## Surfaces

Changing any of these changes this section, in the same commit.

### Wire — `scour-proto`

Newline-delimited JSON over a local socket. One message per line; a reply
carries the `id` of the call it answers. Requests:

| op | mutating | notes |
|---|---|---|
| `search` | | `query`, `sort`, `descending`, `page{offset,limit,count_cap}`; the reply carries `rows_visited` and `rows_built`, the second being what makes a deep page's cost visible |
| `count` | | `query`, `cap` — the total is a floor when `capped` |
| `facets` | | `query`, `by`: `kind` / `ext{top}` / `dir{path,top}` |
| `tree` | | `path`, `depth`, `limit` — bounded *per level* |
| `stat` | | `path`; answered from the source, so a new file is never missing |
| `usage` | | `path` (empty for everything), `top` → what a subtree weighs, its heaviest children, and the age of its bytes. Matches `du` byte for byte — including that a hard-linked file is counted once |
| `explain` | | `query`, optional `cursor` → the sentence, the coloured `spans`, and `completions` at the caret — without running it |
| `sources` | | what is indexed, and each source's `Caps` |
| `status` | | numbers and flags only, never a sentence |
| `stats` | | index size, segments, unsorted tail |
| `rescan` | ✓ | optional `path` to narrow it |
| `maintain` | ✓ | `flush` / `compact` / `rebuild` |
| `syntax` | | the query language reference, as text |
| `shutdown` | ✓ | |

Queries cross as **text**, not as a parsed tree: the service parses, so the
language means one thing rather than three.

`Response` is internally tagged, which constrains what a variant may hold — a
struct or its own named fields, never a bare string or a sequence. serde reports
the violation when the message is *sent*, and the client sees it as a connection
closing for no reason. `every_response_round_trips` is the guard.

### CLI — `scour`

A client and nothing else: no index, no filesystem, does not link the engine.

```
scour <query>                        search (the default)
scour search <q> --sort --limit --offset --ascending
scour count <q>                      scour facets <q> --by kind|ext|<dir>
scour tree <path> --depth --limit    scour stat <path>
scour du [path] --top
scour explain <q>                    scour syntax
scour sources                        scour status
scour rescan [path]                  scour maintain flush|compact|rebuild
scour where                          scour mcp-config
```

`--json` on every command prints the protocol type serialised directly — the
same bytes the service sent. `where` and `mcp-config` answer without a service,
because both are what you reach for when the service is what is not working.

### MCP — `scour-mcp`

Nine tools, all read-only: `scour_search`, `scour_count`, `scour_facets`,
`scour_tree`, `scour_stat`, `scour_disk_usage`, `scour_explain`, `scour_syntax`,
`scour_sources`.

Two rules that are not negotiable:

* **Every answer is bounded**, and says so when it was cut. A model that cannot
  tell a full listing from a truncated one draws confident wrong conclusions.
* **Nothing mutating is exposed.** `rescan`, `maintain` and `shutdown` exist in
  the protocol and stay out of the tool list.

Tool descriptions say what a tool is *for*, not what it does — a model choosing
between `scour_search` and `scour_tree` is making the same decision a person
does, and the descriptions exist to make it easy.

### Service — `scourd`

`src/wire.rs` is the only file in the workspace that names `TantivyIndex` or
`FsSource`. If a second one ever does, something above has stopped being written
against its trait.
