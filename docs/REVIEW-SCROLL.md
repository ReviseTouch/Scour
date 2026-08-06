# Scroll review

Reviewed against `apps/scour-web/src/page.html` after `b007e69`. This review
starts from that commit's measured diagnosis; it does not repeat the spacer-sum
investigation.

`scripts/probe` could not reach the already-running window from this workspace:
the sandbox denied the loopback connection to port 9222 before the script could
read a target. No window was opened or restarted. The sequences below therefore
come from tracing every writer and asynchronous continuation in the page.

## Fixed

### 1. Chromium could still move `scrollTop` through scroll anchoring

The sizer fixes the extent, but not the browser's anchor. The table reuses the
same `<tr>` nodes for different indices and moves them by changing the top
spacer. Before this review the sequence could be:

1. Chromium chooses a pooled row as the scroll anchor.
2. A scroll selects a new range.
3. `paintWindow()` changes the top spacer and rewrites that same node as a
   different index.
4. Chromium corrects `scrollTop` to keep its anchor visually still.
5. The correction fires `scroll`, which queues another paint and another
   spacer move.

`.scroll` now has `overflow-anchor: none`. Recycled virtual rows are not valid
anchors; the native scrollbar is the authority for the position.

### 2. A painted row still fed back into the sizer

`repaint()` measured a row on every frame, assigned the result to `rowPx`, and
`pitch()` multiplied `rowPx` by the row count for `.sizer`. Thus the 30.6 versus
30.99 CSS-pixel variation already measured at fractional scale remained a
paint-to-scroll-extent feedback path even though the bottom spacer was gone.

The list now has a logical pitch of 31 CSS pixels. A row measurement is only a
monotonic upper bound used to keep the normal-flow table clear of the sizer's
bottom; it can grow that clearance but cannot change the logical pitch or
shrink again. At the current reach the sizer is therefore 620,000 CSS pixels
for every 20,000-row result, independent of where the painted rows landed on
device pixels.

### 3. `visibleRange()` could return an inverted range and wedge the pool

A concrete empty-list sequence remained:

1. A capped first answer makes `reach()` 20,000.
2. The thumb reaches approximately row 19,000 before an exact answer lands, or
   a live update removes most of the result tail.
3. `reach()` becomes 1,500 while Chromium still reports the old `scrollTop`.
4. The old calculation returns `[19000, 1500]`.
5. `paintWindow()` evaluates `while (pool.length > to - from)` with a negative
   target, pops every row, then calls `.remove()` on `undefined`.
6. The old painter had already set `painted` and cleared `dirty`, so another
   event in the same range could decide that the failed draw was complete.

The start is now clamped to the last full window inside `reach()`. The range is
always ordered and, where enough rows exist, keeps a constant pool size at the
bottom instead of removing one node per step.

Painter bookkeeping now commits only after `paintWindow()` succeeds. A draw
exception is logged and leaves `dirty` set, so a later event retries instead of
skipping a failed range. Promise continuations that draw the query reading or
sidebar also log instead of swallowing a rendering exception.

### 4. Stable scroll paints still made layout-affecting writes

Four writes ran on every paint even when their values had not changed:

- `.sizer.style.height`;
- the top spacer's cell height;
- `farRow.hidden` and the end notice's `textContent`;
- `widen()`'s `colSpan`.

They are now compare-before-write. `widen()` also spans the picker column; the
table has the age column, the shown columns, and the picker, not merely the
first two. A scroll that stays in the same range consequently does not dirty
the table grid.

### 5. A stale request could delete or populate a newer window

`LIST.pending` used only the integer window start. Two races followed.

First race:

1. Query A has window 400 pending.
2. Query B lands, clears pending, and requests its own window 400.
3. A's old continuation runs `pending.delete(400)` before checking its
   generation.
4. B now looks unrequested and can be requested repeatedly.

Second race:

1. `render()` increments the generation but leaves query A visible until query
   B's first answer arrives.
2. A scroll in that interval asks for an A window stamped with B's generation.
3. B's first answer replaces `LIST.query`.
4. The A window arrives, passes the generation-only check, and inserts A rows
   into B's cache.

Pending entries are now request objects in a `Map`. A continuation may delete
and apply only the identical object still stored at that start, and it must
also match the current query, sort, direction, and generation.

### 6. Live refreshes could arrive in reverse order and cancel another viewport

`atMostEvery()` limits starts, not concurrent requests. If a window took more
than 400 ms, refresh A and refresh B could overlap; B could land first, then A
could replace it with an older ordering. A refresh also called
`LIST.pending.clear()`, cancelling the bookkeeping for a different window the
user had scrolled to while the refresh was in flight.

Live row refreshes now carry both a monotonically increasing refresh identity
and the index revision at which they started. Only the newest refresh for the
still-current revision may apply. It supersedes pending requests only for the
starts it actually supplied.

### 7. Layout could grow without asking for the newly visible rows

Several elements sit in `auto` grid rows above the scroller. They did not
change `scrollHeight`, but they could change `clientHeight` after the last
paint:

- the asynchronous query description and wrapping chips;
- the wrapping status/meter sentence;
- a local font finishing selection;
- a column change adding or removing the horizontal scrollbar;
- returning from the report tab after the search view was resized while
  hidden.

Each now requests a coalesced repaint. Font completion and window resize also
invalidate the cached query-character width. Column selection, drag release,
and reset repaint after their final geometry is known. Returning to the search
tab emits a private event consumed by the list painter.

Column selection had a second empty-row path. Rebuilding a pooled row's cells
left its old `__stamp` and `__marks` on the `<tr>`; `writeRow()` saw the same
file and query, returned early, and left the new cells blank until the new
search completed. Rebuilding a row shape now invalidates both caches before it
is written.

### 8. Initial and offline states retained invalid virtual-list state

`applySide()` queues a paint before the first search has landed. Previously an
unfinished list meant a provisional reach of 20,000, so that paint could draw
20,000 rows' worth of ghosts and call `search` with null query/sort state.
`reach()` is now zero, and `fillWindow()` is inert, until `LIST.query` names a
landed result set.

Likewise, `offline()` cleared only the `<tbody>`. The old sizer, row cache, and
pending windows survived, so scrolling the apparently empty error state could
rebuild stale rows. It now clears the virtual state, reduces the sizer on the
next frame, and leaves the error notice as the only content.

## State machine after the fixes

The range key is the string `"from,to"`. `dirty` means row content or surrounding
layout changed even if that key did not.

| State | Event and guard | Action | Next state |
|---|---|---|---|
| Idle | `repaint(false)`, no frame queued | queue one animation frame | Queued |
| Idle | `repaint(true)` | set `dirty`, queue one frame | Queued dirty |
| Queued | another repaint | only merge `dirty`; do not queue a second frame | Queued |
| Frame | measured clearance grows | invalidate the range key; never alter pitch | Drawing |
| Frame | key equals `painted` and not dirty | no DOM write and no fetch | Idle |
| Frame | key changed or dirty | run `paintWindow()` | Drawing |
| Drawing | draw throws | log; retain dirty | Idle, retryable |
| Drawing | draw succeeds | commit `painted`, clear dirty, ask for missing windows | Idle / Waiting |
| Waiting | the identical current request succeeds | store rows, call `repaint(true)` | Queued dirty |
| Waiting | request was superseded | do nothing, including no pending deletion | Idle |
| Waiting | current request fails | delete its pending token and log | Idle with ghosts |
| Live wait | revision changes while visible | throttle row and count refresh separately | Waiting |
| Live rows | generation, refresh identity, or revision moved | drop the answer | Idle |
| Live rows | answer is current | replace or merge only its windows, repaint, fill current range | Queued dirty / Waiting |

`paintWindow()` does not call `repaint()`. The only route by which a paint can
schedule another paint is a browser `scroll` event caused by changed geometry.
The fixed logical sizer, disabled scroll anchoring, ordered fixed-size range,
and compare-before-write rules remove the ordinary closed paths. A fetched
window, status answer, font completion, resize, or live revision can schedule a
new frame, but each is an external state transition rather than a paint
calling itself.

## Found, not fixed

### 1. Exact count settlement can still move a thumb being dragged

**Likelihood: high for “release leaves it travelling”; it is one correction,
not the old infinite loop.**

Concrete sequence for the saved `rapor` query:

1. The first search is capped, so `reach()` deliberately exposes 20,000 rows
   and the sizer is 620,000 CSS pixels.
2. The user grabs the native thumb before the facet/count request completes
   and drags to row 15,000.
3. The exact total lands as roughly 4,864.
4. The sizer becomes 150,784 CSS pixels while the compositor still owns the
   drag.
5. Chromium clamps the held position to the new maximum. The range guard now
   prevents an exception and fills the landing window, but the thumb still
   visibly travels after the ground underneath it changed.

The smallest fix is to freeze the virtual extent while a native thumb drag is
active, then apply the exact total while preserving either the top row or the
thumb fraction. That needs a product decision: a native scrollbar does not
provide a reliable cross-platform “thumb drag” event distinct from a pointer
inside the content, and freezing makes the scrollbar knowingly overstate the
result for the duration. Choosing top-row preservation versus fraction
preservation also gives different answers.

### 2. The table is kept inside the sizer by an estimate, not containment

**Likelihood: medium for a loop confined to the bottom of the list; highest
remaining route by which a paint can change `scrollHeight`.**

The table remains in normal flow. Near the bottom, `paintWindow()` caps the top
spacer using:

```
headPx + row_count * (rowPx + 0.5) + far_row_allowance + 4
```

That is deliberately generous on the measured 1.667 display, but it is still
an estimate from one sampled row plus a fixed half CSS pixel per row. A concrete
failure requires the estimate to be low:

1. The bottom range displays `farRow` and clamps the top spacer to
   `sizer height - estimated inside height`.
2. A different scale, font metric, accessibility font setting, or table
   rounding distribution makes the actual header, pooled rows, and padded end
   notice taller than that estimate.
3. The normal-flow table ends below `.sizer`; it, rather than the sizer, now
   determines `scrollHeight`.
4. The browser corrects a bottom position and fires `scroll`.
5. The next range has a different row-height distribution or row count, so the
   table falls inside the sizer again; the extent shrinks and another
   correction is possible.

The smallest proof-based fix is structural: put the table in a list-height
wrapper whose overflow is clipped, so no descendant can extend the scrolling
overflow beyond the sizer. That changes the containing block for the sticky
header and the native horizontal scrollbar. Both must be retested; doing it
here would be the list-structure and sticky-header design change the review was
asked not to make.

### 3. A failed or hung window has no recovery policy

**Likelihood: medium for “sometimes empty,” especially on a quiet index.**

On rejection, `fillWindow()` now deletes the exact pending token and logs, but
the visible nodes remain ghosts. `painted` still correctly names the drawn
range. Nothing schedules another fetch merely because a request failed, so a
transient failure remains blank until a scroll, resize, layout change, or index
revision invokes `fillWindow()` again. A fetch that never settles is worse: its
pending token remains forever and every later fill skips that start.

The smallest fix is an abort deadline plus bounded exponential retry and a
visible per-window failure state. The risks are retrying into a bridge that is
already overloaded and deciding whether old rows or ghosts are the honest
failure display. That is fetch/error strategy, so it is documented rather than
chosen here.

### 4. The first search is gated on the taxonomy request settling

**Likelihood: low to medium for a completely blank launch.**

Startup calls `/api/kinds` and invokes `render` only from that promise's
`finally`. A normal rejection settles and still renders. A connection that
opens but never finishes does not settle:

1. `applySide()` paints the intentional zero-reach initial state.
2. `/api/kinds` remains pending.
3. `render()` is never called.
4. No list request, empty notice, or offline notice appears.

The smallest fix is to start `render()` independently and populate the rail
when kinds arrive. It changes startup request ordering and permits the first
rows to be mapped before kind labels exist; those rows would need relabelling
when taxonomy lands. That is a fetch/startup decision.

### 5. One cached row stands for a complete 200-row window

**Likelihood: low alone, higher when the exact-count request also fails.**

`fillWindow()` skips a window when `LIST.rows.has(start)` is true. It does not
record how many rows the request supplied. If a concurrent index change yields
a short page while the list still has provisional reach, the first row marks
the whole window present and the remaining slots stay ghosts. The later exact
count normally removes a genuine tail; if that request fails, no code can tell
a short cache entry from a complete one.

The smallest fix is window metadata: start, requested length, returned length,
result identity, and whether the response proved end-of-results. That changes
the cache/fetch strategy and its invalidation rules.

### 6. Horizontal overflow can change `clientHeight` during a column drag

**Likelihood: low; it cannot sustain a loop once the drag stops.**

A header width crosses the table's horizontal-overflow threshold, the native
horizontal scrollbar appears, and `.scroll.clientHeight` loses the scrollbar's
thickness. If the vertical thumb is also at its maximum, Chromium may correct
the vertical position. Pointer release now repaints the final geometry, but
pointer moves intentionally do not repaint every column-width pixel.

Eliminating the correction means either reserving horizontal-scrollbar space
or removing horizontal overflow. Both change the column/scrollbar design and
waste space or hide columns.

## Scale-factor audit

The dangerous scale assumption was using a sampled painted row as the virtual
pitch; that is fixed. The remaining scale-sensitive pieces are separated by
whether they can affect scrolling:

- `inside` still uses `rowPx + 0.5` as an arithmetic containment allowance.
  This is the structural risk in finding 2; half a CSS pixel is not a proof
  about arbitrary device-pixel grids.
- `scrollTop` is not rounded. It may be fractional and is divided by a logical
  CSS-pixel pitch. `clientHeight` is read in the same CSS coordinate system and
  rounded upward only when converting a viewport to a row count. This is sound.
- `MAX_PX` is stated in CSS pixels, but with `REACH = 20,000` the largest list
  is 620,000 pixels, so the 33-million-pixel packing branch is unreachable in
  this page today.
- Row geometry is still duplicated as `--row: 30px`, a one-pixel collapsed
  row border, `ROW_H = 31`, and `.ghost { height: 31px }`. A stylesheet change
  can make visual rows drift relative to the logical coordinate, although it
  can no longer make a painted row resize the sizer. Reading the computed row
  would recreate the forbidden feedback; consolidating the logical pitch into
  one non-measured configuration value belongs with a list-structure change.
- Query term actions use measured average glyph width over 100 characters.
  That cache is now invalidated on font completion and resize. The remaining
  hard-coded 17/19/38-pixel button geometry can misalign a hit target under
  unusual font settings, but it is absolutely positioned and cannot alter
  list layout.
- Column drag rounds a width to an integer CSS pixel. It can make a divider
  jump by a fraction of a device pixel, but only the horizontal-scrollbar
  threshold in finding 6 connects it to list height.

CSS pixels themselves are not the error. Mixing requested CSS geometry with a
measurement that has already been snapped to device pixels, then feeding it
back into scroll geometry, is the error.

## Ruled out

### The icon `<img>` loading late

`writeName()` creates the image before assigning `src`, and `.ico` gives it an
18-by-18 CSS-pixel box immediately. Success changes pixels inside that box;
failure changes `visibility`, which preserves the box. Rows are nowrap and the
cell height is larger than the icon. Image decode/load therefore cannot change
row height. Toggling icons is different: it removes the nodes deliberately and
already goes through `draw()` and `repaint(true)`.

### Text overflow and long paths

The table uses `table-layout: fixed`; body cells are `white-space: nowrap`,
`overflow: hidden`, and `text-overflow: ellipsis`. The path also has a fixed
column and an inner max width. Text changes paint and clipping, not row height
or table width. `<mark>` adds horizontal padding only and cannot wrap.

### Query shadow, actions, hints, and menus

`.qshadow`, `.qacts`, and `.hints` are absolutely positioned. Row and column
menus are fixed-position children of `body`. They can be shown, filled, and
removed without contributing to the app grid or `.scroll` overflow. The query
description and chips are not absolute; their real `clientHeight` path is fixed
above.

### `syncFacets()` and the rail

Facet replacement changes the contents of `.rail`, which has its own
`overflow-y: auto` inside a fixed `minmax(0, 1fr)` body row. It does not grow
the body or results column. The exact total assigned by the same sidebar reply
does change `.sizer`; that separate one-shot path is finding 1.

### Sidebar visibility and responsive placement

`applySide()` changes app width and explicitly repaints. Window resize covers
the 1040- and 720-pixel media-query transitions. Wrapping query chrome and a
possible horizontal scrollbar are settled before the queued frame reads
`clientHeight`. No paint toggles the sidebar, so this path cannot call itself.

### `farRow` and the empty notice

`farRow` is nowrap and its visibility and text are now compare-before-write. It
can affect `scrollHeight` only through the arithmetic-containment risk already
listed.

The empty notice is normal flow after the table and can be taller on a very
short window, but it is shown only when total is zero or the service is offline.
Scrolling does not toggle it, so it cannot close a paint/scroll loop. The
offline path now also reduces virtual reach to zero instead of leaving an old
sizer behind it.

### Sticky header, focus restoration, and the arrival animation

Sticky positioning changes where the header is painted, not its normal-flow
height. Header height is tracked as a monotonic clearance bound. Reused-row
focus is restored with `{ preventScroll: true }`. The arrival animation changes
background colour only. None of the three changes scroll geometry on a paint.

### Direct scroll-position writes

There is one `scrollEl.scrollTop = 0`, in the successful first-window answer
for a newly rendered query. It is not reachable from `paintWindow()`, a window
fill, or a live refresh. The page has no `scrollIntoView`, `scrollTo`, or CSS
smooth-scroll rule. The only focus inside recycled rows uses
`{ preventScroll: true }`; query and fixed-menu focus do not target the list.

### `painted` / `dirty` coalescing

There is one animation-frame door. `dirty` is merged while a frame is pending;
it is cleared only after a successful draw. A clean identical range performs
no paint and no fetch. A range cannot alternate merely because it reaches the
tail: `visibleRange()` now anchors the last full pool inside the end. The live
path cannot overwrite a newer generation, revision, refresh, or pending window
identity. These checks remove the bookkeeping wedges and two-value oscillation
paths present before this review.

## Verification

`scripts/check --quick` completed format, workspace Clippy, the extracted
page's `node --check`, and all tests before `scour-ipc`. It then stopped because
the execution sandbox denied Unix-socket creation: six IPC tests failed at
`Server::bind` with `PermissionDenied` for their fresh `/tmp/.tmp*/s.sock`
paths; the one IPC test that does not bind passed. The same sandbox denial also
prevented the read-only CDP probe. This is an environment failure rather than a
failure in the page change, but it means the requested command did not exit
successfully in this session.

Independent final checks run here are `git diff --check`, extraction plus
`node --check`, and `cargo test -p scour-web`.
