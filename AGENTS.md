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
| Implementations | `scour-index-tantivy`, `scour-source-fs`, `scour-config`, `scour-i18n` | one concrete technology each |
| Orchestration | `scour-engine` | `Box<dyn Source>`, `Box<dyn Index>` — no concrete types |
| Wiring | `apps/scourd` | **the only place concrete types are named** |
| Frontends | `apps/scour`, `apps/scour-mcp`, later the GUI | `scour-proto` only |

Consequences that are not negotiable:

- `scour-engine` must never contain the word `tantivy`, `notify`, or `ignore`.
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

The verifier is not optional: `scour-index-tantivy` checks its results against
`scour-mock::brute_force`. Three real bugs — a capped count that bounded the
value but not the work, a segment tail cut at page size, and deleted documents
reappearing — were invisible to timing and only caught by that comparison.

## Surfaces

### CLI — `scour`

*(filled in at M10)*

### MCP — `scour-mcp`

*(filled in at M11)*

### Wire — `scour-proto`

*(filled in at M8)*
