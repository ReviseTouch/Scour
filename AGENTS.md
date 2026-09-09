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
| Implementations | `scour-index-native`, `scour-source-fs`, `scour-config`, `scour-i18n` | one concrete technology each |
| Orchestration | `scour-engine` | `Box<dyn Source>`, `Box<dyn Index>` — no concrete types |
| Wiring | `apps/scourd` | **the only place concrete types are named** |
| Frontends | `apps/scour`, `apps/scour-mcp`, `apps/scour-web`, `apps/scour-gui`, `apps/scour-tui` | `scour-proto` only |

Consequences that are not negotiable:

- `scour-engine` must never contain the word `notify`, `ignore`, or the name
  of any index implementation.
- Swapping the search engine must be a one-line change in `apps/scourd/src/wire.rs`.
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

The verifier is not optional: `scour-index-native` checks its results against
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
| `facets` | | `query`, `by`: a **list** of `kind` / `ext{top}` / `dir{path,top}` / `age{edges}`, answered from one walk of the matching set — a sidebar wanting three used to walk it three times. `age` bands are the caller's, because a chart of twenty-four bars and a list of six periods want different edges out of the same rows |
| `tree` | | `path`, `depth`, `limit` — bounded *per level* |
| `stat` | | `path`; answered from the source, so a new file is never missing |
| `usage` | | `path` (empty for everything), `top` → what a subtree weighs, its heaviest children, and the age of its bytes. Agrees with `du` on the total and **deliberately not per directory**: a hard-linked file is charged `disk / links` to each of its names, where `du` charges the whole file to whichever name it meets first, so its per-folder numbers depend on traversal order and these do not change when a directory is renamed |
| `explain` | | `query`, optional `cursor` → the sentence, the coloured `spans`, and `completions` at the caret — without running it |
| `sources` | | what is indexed, and each source's `Caps` |
| `status` | | numbers and flags only, never a sentence |
| `stats` | | index size, segments, unsorted tail |
| `await` | | `since` (the last `revision` seen), `timeout_ms` → a `Status`, returned when the index would answer differently or when the wait runs out; the revision in it says which. The one request allowed to take its time, and what makes a live list one blocked thread rather than 86,400 searches a day spent discovering that a desktop was idle. It is also what tells the service somebody is looking, which is what makes a change worth committing sooner than it would be for nobody |
| `rescan` | ✓ | optional `path` to narrow it |
| `maintain` | ✓ | `flush` / `idle` / `compact` / `rebuild` — `idle` is separate from `flush` because they happen at different rates: flushing is what a burst of changes needs every second, giving back the write buffer is what a machine sitting overnight needs once |
| `duplicates` | | `under`, `min_size`, `read_budget`, `top` → files that share a size, and how many of those were **read** and proved identical. The two numbers always travel together: sharing a size is not being the same file, and a panel showing only the first would be telling somebody to delete database pages that happen to be the same length |
| `settings` | | what a person has chosen — columns and their order, widths, sort, the queries they meant, and `layout`: the shape the result list is drawn in, one of `detail` (a table of rows), `icons` (a grid of tiles) or `large` (a grid of big ones). Held here because the service is the only thing every frontend talks to, and because a browser loses `localStorage` when it is killed. `layout` is empty until somebody chooses, which is not the same as `detail`, and a word a frontend does not know degrades to its own default rather than refusing the file — the rule `language` already followed |
| `set-settings` | ✓ | a **change**, not the whole object: what it does not name, it does not touch. That is what lets a window and a terminal be open at once without each erasing the fields the other understands, and what lets a field be added without every frontend learning about it first |
| `thumbnails` | ✓ | `files` → which of them have a picture now, and `ran`: how many thumbnailer processes this started. The desktop's own `*.thumbnailer` commands do the work and the result goes in the freedesktop cache, so Files and Loupe find what Scour asked for and the reverse. **Here because the bound is about the machine**: at most four decoders at once, one number for the whole desktop, which three frontends each bounding themselves could not be. Fenced like `stat` — every path is `stat`ed first, and the modification time that comes back is what the standard requires be written into the picture. Mutating not because the index changes but because it starts programs, which is what keeps it out of the MCP server. Allowed to take its time, like `await` |
| `syntax` | | the query language reference, as text |
| `shutdown` | ✓ | |

Queries cross as **text**, not as a parsed tree: the service parses, so the
language means one thing rather than three.

Search `took_us` includes the engine's parsing, cache lookup and folder-size
enrichment as well as the index call. It excludes transport and frontend
rendering. Prepared pages are valid only for the current revision **and** the
current parsed query: a relative date cutoff changes while the disk is quiet.
Preparation is speculative: only nonempty pages inside its first 20,000 hits
can request it, and only after a page takes at least 20 ms to answer.
Successful explicit flushes publish a revision; shutdown wakes blocked
`await` callers before joining the engine workers.

`Response` is internally tagged, which constrains what a variant may hold — a
struct or its own named fields, never a bare string or a sequence. serde reports
the violation when the message is *sent*, and the client sees it as a connection
closing for no reason. `every_response_round_trips` is the guard.

### CLI — `scour`

A client and nothing else: no index, no filesystem, does not link the engine.

```
scour [-n N] [-s SORT] <query>       search, ordered by relevance (the default)
scour search <q> --sort --limit --offset --ascending --count-cap
scour count <q>                      scour facets <q> --by kind|ext|<dir>
scour tree <path> --depth --limit    scour stat <path>
scour du [path] --top
scour explain <q>                    scour syntax
scour sources                        scour status
scour stats                          scour rescan [path]
scour maintain flush|compact|rebuild
scour where                          scour mcp-config
```

On the bare form **everything after the query is part of the query, flags
included** — a filename can contain `--` and a tool that refuses to look for it
is broken. The sharp edge is that `scour rapor -n 100` searches for three words
and returns a silent zero, so `-n` and `-s` are accepted *before* the query and
`--help` says so. `scour search` takes them anywhere.

`--json` and `--socket` are global. `--json` prints the protocol type
serialised directly — the same bytes the service sent — on every command that
reaches the service; `--socket` points at one other than the configured one.
`where` and `mcp-config` are the two that reach no service, because both are
what you reach for when the service is what is not working, and they return
before `--json` is looked at.

### MCP — `scour-mcp`

Nine tools, all read-only: `scour_search`, `scour_count`, `scour_facets`,
`scour_tree`, `scour_stat`, `scour_disk_usage`, `scour_explain`, `scour_syntax`,
`scour_sources`.

Two rules that are not negotiable:

* **Every answer is bounded**, and says so when it was cut. A model that cannot
  tell a full listing from a truncated one draws confident wrong conclusions.
* **Nothing mutating is exposed**, and that is enforced rather than described.
  Every tool goes through one `call`, and anything `Request::is_mutating`
  answers `true` for is refused there before it reaches the socket — so the
  server is read-only because writes are stopped, not because the four tools
  that could write were never written. `rescan`, `maintain`, `set-settings` and
  `shutdown` stay out of the tool list too, but the list is not what keeps the
  promise: the day somebody adds a tenth tool, the guard already knows the
  answer.

Tool descriptions say what a tool is *for*, not what it does — a model choosing
between `scour_search` and `scour_tree` is making the same decision a person
does, and the descriptions exist to make it easy.

### Browser — `scour-web`

A bridge and only a bridge: one page and seventeen JSON routes, holding no index
and linking no engine. One of them answers without the service — `/api/icon`,
because a thumbnail somebody already made is a file, and reading it is a `stat`
and a `read` rather than a question about an index. **No HTTP framework and no
async runtime** — axum would bring tokio, hyper and about a hundred crates to do
what two hundred lines of `std::net` do, and `scour-mcp` is a separate binary
precisely so the rest of the workspace stays free of one.

`/api/thumb` is the other half of that pair and deliberately not in the same
place: it asks the service to *make* the pictures nothing has made yet, because
how many image decoders may run at once is a fact about the machine rather than
about a browser. It carries no bytes — it answers with which paths have a
picture now, and the page fetches those from `/api/icon` as before. It is
`POST`, it is refused by `--no-thumbnails`, and it opens its own connection to
the service like `/api/wait`, because it is allowed to take seconds and nothing
that takes seconds may sit in the shared pool in front of a keystroke.

What is behind the port is an index of every file the user owns, so four things
hold and none is a preference: **127.0.0.1 only**, with no flag to change it; a
**token** generated per run and printed with the URL, without which every route
answers 403; **`Origin` checked** on every request, because a page on the
internet can make a browser send one here; and `GET` for reading against `POST`
for the routes that do something, so a link, an image or a prefetch cannot reach
them. `rescan` and `maintain` are not routed at all — a page in a browser does
not get to make the service work.

`/api/open` is the first of those, and **it runs executables**. That was a
refusal once, and is not any more, because a search box that finds a program and
sends you elsewhere to start it has not finished the job. The fence is the
index: the path is `stat`ed through the service first, so a path no source owns
cannot be opened. `--no-run` reveals the folder instead; `--no-launch` removes
the route.

`/api/thumb` is the second, and it runs programs too — the ones
`/usr/share/thumbnailers` declares. Same fence and for a better reason: every
path is `stat`ed through the service before a thumbnailer sees it. Its `GET`
form would have looked completely harmless, which is exactly why it is a `POST`.
`--no-thumbnails` removes it; reading pictures that already exist is not behind
that flag, because reading them starts nothing.

The result cache keeps at most 32 windows of 200 rows in ordinary viewports,
evicting offscreen windows and their path references together. Speculation
reaches at most two windows beyond the viewport; it does not traverse the
20,000-row reach while the user is stationary. Visible and pending windows
are protected from eviction. Hidden or handed-over pages start no window
fetches. Each periodic refresh lane has at most one running operation and one
coalesced follow-up, regardless of how slowly the service answers.

### Window — `scour-gui`

A frontend, exactly like `apps/scour`: no index, no filesystem walk, does not
link the engine. Three rules hold it up.

Both result layouts use Slint's `ListView` with a paged model. Initial
population resets an empty model so Slint 1.16.1 does not allocate a placeholder
for every result through `row_added(0, count)`; subsequent length changes use
incremental notifications to preserve the viewport. Optional live refresh
rests after completion for at least 500 ms and ten times the last page cost.
Missing viewport pages and explicit new queries bypass that refresh rest.

* **The window never waits.** Every call is on a worker thread and comes back
  as an event. Two lanes — interactive and background — because `scour-ipc` is
  one call at a time and a 20 ms facet count must not sit in front of a
  keystroke.
* **A stale answer is dropped, not shown.** Every request carries the keystroke
  that caused it. A slow reply to `re` landing after a fast one to `rapor` is
  the most noticeable defect a search-as-you-type box can have.
* **The frontend does not parse queries.** What a term means is `explain`'s
  answer. Two narrow exceptions: `terms_of`, which decides *which words* to
  highlight and nothing about what they mean, and `scour_query::without`,
  which the rails call to drop their own term — the parser's judgement,
  borrowed rather than reimplemented.

Slint's `TextInput` has no range colouring — upstream #9560 puts editable text
out of scope. It has no substring either, which is why a highlighted name
arrives as three strings.

### Service — `scourd`

`src/wire.rs` is the only file outside the crates that define them which builds
a `NativeIndex` or an `FsSource`. If a second one ever does, something above
has stopped being written against its trait. `scour-engine`'s tests are the
deliberate exception: they wire a real index to a source the engine cannot
tell from a filesystem, which is the half of the rule worth testing.

Change feeds and source pulses are hints; neither is proof that a quiet source
is unchanged. `[service] poll_interval_secs` defaults to 60 for sources with
neither complete watch coverage nor a pulse. `reconcile_interval_secs` defaults
to 1800 for a full safety pass on every source. Wiring clamps both to at least
one second. Deadlines are checked every two seconds and run on the worker;
they are scheduling intervals, not an end-to-end freshness guarantee. Normal
periodic recovery rests at least twenty times the preceding full scan's wall
duration, in addition to its configured floor. Partial subtree activity cannot
postpone a full safety pass. Failed or incomplete reconciliation is retried
with backoff measured after completion and at least twenty times the full
scan cost, preserving rows under unreadable paths. The engine joins both
its mutation worker and its prepared-query worker on shutdown.

The optional Linux service installer keeps the privileged `scour-watch` helper
under root-owned `/usr/local/libexec/scour`. The shipped host-specific unit and
polkit rule permit only `hasan` to start, stop and restart `scour.service` without
authentication. The helper uses a fresh private directory under `/run` for its
short-lived mount and drops its identity before executing the user-owned daemon.
The rule must never be installed with a user-writable privileged executable.
