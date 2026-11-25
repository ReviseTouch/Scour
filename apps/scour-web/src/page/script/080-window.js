  /* Not how many matched: a capped count is a floor, and sizing by it moved
   * the scrollbar 620,004 → 31,005 → 620,005 px in two tenths of a second.
   * Every row inside `REACH` is fetchable, counted or not. */
  const reach = () => {
    if (LIST.query === null) return 0;
    return LIST.exact ? Math.min(LIST.total, REACH) : REACH;
  };

  /* How many tiles fit across, read once a frame where the layout is settled.
   * `scrollbar-gutter: stable` cuts the loop: more across → fewer lines → no
   * scrollbar → wider scroller. */
  let PER_ROW = 1;
  function measurePerRow() {
    PER_ROW = grid()
      ? Math.max(1, Math.floor(scrollEl.clientWidth / TILE[MODE].w))
      : 1;
  }

  /* How many lines the list is, and how tall one of them is; in `detail` they
     are `reach()` and `ROW_H`. */
  const lines = () => Math.ceil(reach() / PER_ROW);
  const lineH = () => (grid() ? TILE[MODE].h : ROW_H);

  /* Pixels per logical line until the set is too tall to draw, then whatever
     fits. A painted row never feeds back into this. */
  const pitch = () => (lines() * lineH() > MAX_PX ? MAX_PX / lines() : lineH());

  /* The margin, in lines: `MARGIN` already counts lines where a line is a row. */
  const marginLines = () => (grid() ? GRID_MARGIN : MARGIN);

  function visibleRange() {
    const p = pitch();
    const m = marginLines();
    const rows = lines();
    const count = Math.ceil(scrollEl.clientHeight / p) + m * 2;
    const end = reach();
    const wanted = Math.max(0, Math.floor(scrollEl.scrollTop / p) - m);
    /* `end` can move before Chromium clamps the old `scrollTop`, and a range
       of `[19000, 1500]` pops past an empty pool for good. */
    const first = Math.min(wanted, Math.max(0, rows - count));
    /* Out in items: the multiplication is the only place the two coordinate
       systems meet. */
    return [first * PER_ROW, Math.min(end, (first + count) * PER_ROW)];
  }

  /* The nodes are kept and rewritten, never rebuilt: replacing the body's HTML
   * measured 18.5 ms a scroll against a 16.7 ms frame. The sizer makes the
   * scrollbar honest; the spacer places the drawn window in it. */
  let pool = [];
  const sizer = document.getElementById("sizer");
  const spTop = document.createElement("tr");
  const farRow = document.createElement("tr");
  spTop.className = "spacer";
  farRow.className = "far";
  spTop.innerHTML = "<td></td>";
  farRow.innerHTML = "<td></td>";

  function widen(node) {
    /* Age + shown columns + the picker. A short span makes the table grid
       reconcile a short row on every paint. */
    const span = SHOWN.length + 2;
    if (node.firstElementChild.colSpan !== span) {
      node.firstElementChild.colSpan = span;
    }
  }

  function makeRow() {
    const tr = document.createElement("tr");
    tr.tabIndex = 0;
    tr.draggable = true;
    tr.innerHTML = `<td class="age"><i></i></td>` + shownColumns().map(() => "<td></td>").join("");
    return tr;
  }

  /* Still a `<tr>`: selection, the menu and the keyboard walk all reach for
   * `closest("tr")`. The picture is `writeMark`'s, so the name is `__nm`. */
  function makeTile() {
    const tr = document.createElement("tr");
    tr.tabIndex = 0;
    tr.draggable = true;
    const td = document.createElement("td");
    td.className = "tile";
    const rule = document.createElement("i");
    const nm = document.createElement("div");
    nm.className = "nm";
    td.append(rule, nm);
    tr.append(td);
    tr.__nm = nm;
    return tr;
  }

  /* Which shape the pool is made of, so a change of shape can throw it away. */
  let poolShape = "detail";
  /* What the sizer and the spacer were last told, which is not what they
     would say if asked. See the note in `paintWindow`. */
  let sizedTo = -1;
  let spacedTo = -1;

  function paintWindow() {
    const [from, to] = visibleRange();
    const p = pitch();
    const cols = shownColumns();
    const parsed = parse(q.value);
    const tiles = grid();

    /* The shape first: one spacer with the right height, and enough rows after
       it. Only a resize or a column change gets past the `while`s. */
    if (body.firstElementChild !== spTop) {
      body.replaceChildren(spTop, farRow);
      pool = [];
    }
    /* The check below cannot tell a tile from a one-cell row. Thrown away
       rather than rewritten: a shape change is a click, not a scroll. */
    if (poolShape !== MODE) {
      for (const tr of pool) tr.remove();
      pool = [];
      poolShape = MODE;
    }
    while (pool.length < to - from) {
      const tr = tiles ? makeTile() : makeRow();
      pool.push(tr);
      body.insertBefore(tr, farRow);
    }
    while (pool.length > to - from) {
      pool.pop().remove();
    }
    /* How many across, said by the page: under `auto-fill` a browser fitting
       one more indexes every later tile wrongly, in silence. */
    if (tiles) {
      const across = `repeat(${PER_ROW}, minmax(0, 1fr))`;
      if (body.style.gridTemplateColumns !== across) {
        body.style.gridTemplateColumns = across;
      }
    } else if (body.style.gridTemplateColumns) {
      body.style.gridTemplateColumns = "";
    }
    if (!tiles && pool.length && pool[0].children.length !== cols.length + 1) {
      for (const tr of pool) {
        tr.innerHTML =
          `<td class="age"><i></i></td>` + cols.map(() => "<td></td>").join("");
        /* The cells are new even though the row object is not: a stamp from
           the old shape makes `writeRow` return and leaves them blank. */
        tr.__stamp = null;
        tr.__marks = null;
      }
    }
    /* Stated, not summed: a summed height moves as rows round, and a moving
     * scroll extent makes the browser correct its position, fire a scroll,
     * paint, and move it again. */
    /* Decided here: one more thing that must fit above the bottom. */
    const far = LIST.total > REACH && to >= reach();
    if (farRow.hidden === far) farRow.hidden = !far;

    [spTop, farRow].forEach(widen);
    const tall = Math.round(lines() * p);
    /* Remembered rather than read back: Chromium re-serialises a length to
     * six figures, so `1146724px` reads back as `1.14672e+06px`. */
    if (sizedTo !== tall) {
      sizedTo = tall;
      sizer.style.height = `${tall}px`;
    }
    /* Everything besides the spacer, generously, so the spacer stops short of
       what `.sizer` claimed. The end note measures against a row. */
    const drawn = Math.ceil((to - from) / PER_ROW);
    const inside =
      headPx + drawn * (linePx[MODE] + 0.5) + (far ? linePx.detail + 12 : 0) + 4;
    const above = Math.max(0, Math.min(tall - inside, Math.round((from / PER_ROW) * p)));
    // Remembered, for the reason the sizer's height is: the spacer runs to
    // seven figures at the far end of a grid too.
    if (spacedTo !== above) {
      spacedTo = above;
      spTop.firstElementChild.style.height = `${above}px`;
    }
    if (far) {
      const note = T(
        "the first {reach} rows · of {total} matches. Narrow the search to go further.",
        { reach: fmt(REACH), total: fmt(LIST.total) },
      );
      if (farRow.firstElementChild.textContent !== note) {
        farRow.firstElementChild.textContent = note;
      }
    }

    /* The rows are reused, so what is restored is which one holds the focus
       rather than the element itself. */
    const had = document.activeElement;
    const hadInBody = !!(had && body.contains(had));
    const keep = (hadInBody ? had.dataset.path : null) || wantFocusPath;

    // The marked letters move with the query, so a row whose file has not
    // changed is still rewritten when the terms have. Joined once, not per row.
    const marks = [parsed.text.join("\u0001"), (parsed.letters || []).join("\u0001"), (parsed.fuzzy || []).join("\u0001")].join("\u0002");
    for (let i = from; i < to; i++) {
      // Which row of the whole list this slot shows; a shift-range is measured
      // in these, and a slot shows a different one after every scroll.
      pool[i - from].dataset.i = String(i);
      writeRow(pool[i - from], LIST.rows.get(i), cols, parsed, marks);
    }
    // After the writes, never inside them: `writeRow` assigns `className`
    // outright and returns early when only the selection has changed.
    paintSelection();
    /* And the clock's next wake: scrolling changes which rows are on screen
       and so when any of them next reads differently. */
    scheduleClock();
    if (keep) {
      const back = pool.find((tr) => tr.dataset.path === keep);
      if (back) {
        wantFocusPath = null;
        if (document.activeElement !== back) back.focus({ preventScroll: true });
      } else if (hadInBody) {
        wantFocusPath = keep;
        had.blur();
      }
    }
  }

  /* One row, rewritten in place; none of it goes through HTML. A row with no
   * data yet is a ghost — the same node, emptied. */
  function writeRow(tr, f, cols, parsed, marks) {
    /* Every fact a visible cell can read, so a live refresh of unchanged rows
       costs nothing. A stamp short of a column leaves that column stale. */
    const under = f && f.under;
    const stamp = f ? [
      f.path, f.name, f.mtime, f.size, f.fresh || 0,
      f.is_dir ? 1 : 0, f.thumb ? 1 : 0, KEPT && KEPT.system_icons ? 1 : 0, f.ktoken || "", f.kind || "",
      f.ext || "", f.ctime || 0, f.atime || 0, f.perm || "",
      f.user || "", f.group || "", f.disk || 0, f.items || 0,
      under ? under.disk : "", under ? under.files : "",
      f.found && f.found.length ? JSON.stringify(f.found[0]) : "",
    ].join("\u0001") : "";
    if (tr.__stamp === stamp && tr.__marks === marks) {
      return;
    }
    tr.__stamp = stamp;
    tr.__marks = marks;
    if (!f) {
      if (tr.className !== "ghost") {
        tr.className = "ghost";
        tr.removeAttribute("data-path");
        tr.removeAttribute("title");
        tr.style.animationDelay = "";
        // From the second cell on: the first holds the freshness rule, which
        // is an element rather than text, and emptying it takes the rule away.
        if (tr.__nm) tr.__nm.textContent = "";
        else for (let i = 1; i < tr.children.length; i++) tr.children[i].textContent = "";
        // The glyph does go: a row waiting for its file must not wear the
        // picture of the one before it. The rule keeps its colour.
        writeMark(tr.firstElementChild, null);
      }
      return;
    }
    const full = f.path + "/" + f.name;
    // The preview panel wants the row, not the path: it already holds every
    // fact the panel shows.
    tr.__row = f;
    tr.className = f.fresh ? "fresh" : "";
    tr.style.animationDelay = f.fresh ? `-${Date.now() - f.fresh}ms` : "";
    if (tr.dataset.path !== full) {
      tr.dataset.path = full;
      tr.title = full;
    }
    // `firstElementChild`: a row's first cell holds the rule and the glyph,
    // and a tile's only cell holds those and the name.
    const dot = writeMark(tr.firstElementChild, f);
    dot.style.background = `var(${bandOf(f.mtime)})`;
    /* A tile is a picture and a name and no columns: which columns are shown
       says nothing about what a tile holds. */
    if (tr.__nm) {
      writeName(tr.__nm, f, parsed.text, parsed);
      return;
    }
    for (let c = 0; c < cols.length; c++) {
      cols[c].write(tr.children[c + 1], f, parsed);
    }
    writeFound(tr, f.found && f.found[0]);
  }

  /* Where a `content:` query found its text in the row's file, on the row's
     second line: the line's number, and the line with the find marked. Out
     of the table's flow, so it spans the row and takes no column. */
  function writeFound(tr, found) {
    if (!found) {
      if (tr.__found) tr.__found.hidden = true;
      return;
    }
    if (!tr.__found) {
      tr.__found = document.createElement("div");
      tr.__found.className = "found";
    }
    // Again whenever it is not there: a row's cells are rebuilt when the
    // columns change, and that takes it off with them.
    if (tr.__found.parentNode !== tr) tr.append(tr.__found);
    tr.__found.hidden = false;
    tr.__found.replaceChildren(...foundNodes(found));
  }

  /* One find as nodes, never HTML: the line is a file's own text. The part
     before the find keeps its last few dozen characters, so the find is in
     sight however far into a long line it is. */
  function foundNodes(found) {
    const n = document.createElement("span");
    n.className = "n";
    n.textContent = found.line != null ? String(found.line) : "";
    const out = [n];
    // Every find marked; what is ahead of the first cut to its last forty.
    lineSegs(found).forEach(([text, isFind], k) => {
      let t = text.replace(/[\t\r\n]/g, " ");
      if (k === 0 && !isFind) {
        t = t.trimStart();
        if (t.length > 40) t = "…" + t.slice(-40).trimStart();
      }
      if (isFind) {
        const mark = document.createElement("mark");
        mark.textContent = t;
        out.push(mark);
      } else out.push(t);
    });
    return out;
  }


  /* Ask at once and abandon what the screen has left behind: a page is a slice
   * of the kept ordering, 5.5 ms at row nineteen thousand and flat at every
   * depth, so at most `INFLIGHT` run, nearest the middle first. */
  const INFLIGHT = 4;

  /* How long a result set has to stand still before the page reads ahead into
     it: long enough that typing does not start a hundred fetches. */
  const SETTLED = 400;

  /* What a window may cost before even nearby speculation is too expensive,
     in the service's own milliseconds. */
  const READ_AHEAD_MAX = 25;

  /* Read a little beyond either visible edge: a bounded neighbourhood keeps
     the LRU cache from fetching its own evictions. */
  function nextMissing(from, to) {
    const end = Math.min(reach(), Math.ceil(to / WINDOW) * WINDOW + AHEAD_WINDOWS * WINDOW);
    const begin = Math.max(0, Math.floor(from / WINDOW) * WINDOW - AHEAD_WINDOWS * WINDOW);
    const here = Math.floor((from + to) / 2 / WINDOW) * WINDOW;
    for (let step = 0; step * WINDOW <= end - begin; step++) {
      const tries = step === 0 ? [here] : [here + step * WINDOW, here - step * WINDOW];
      for (const start of tries) {
        if (start < begin || start >= end) continue;
        /* `at` rather than `rows`: a window that came back empty is fetched,
           and `rows` would answer "missing" for ever. */
        if (!LIST.at.has(start) && !LIST.pending.has(start)) return start;
      }
    }
    return null;
  }

  function fillWindow() {
    if (document.hidden || HANDED_OVER || LIST.query === null) return Promise.resolve();
    const [from, to] = visibleRange();

    const first = Math.floor(from / WINDOW) * WINDOW;
    const wanted = [];
    for (let start = first; start < to; start += WINDOW) {
      if (LIST.pending.has(start)) continue;
      // On screen, out of date is as good as missing. Off screen it is not.
      if (LIST.at.get(start) === SHOWING) {
        keepWindow(start, SHOWING);
        continue;
      }
      wanted.push(start);
    }
    /* A list with nothing in it still has to be asked about: `reach()` is
       zero, so the range is `[0, 0]` and the loop above covers no window. */
    if (first >= to && LIST.at.get(0) !== SHOWING && !LIST.pending.has(0)) wanted.push(0);
    const eye = (from + to) / 2;
    wanted.sort((a, b) => Math.abs(a + WINDOW / 2 - eye) - Math.abs(b + WINDOW / 2 - eye));

    /* Except the window the reading-ahead started off screen on purpose, or
     * every other request is a page the service built and nobody read. */
    for (const [start, request] of LIST.pending) {
      const behind = start + WINDOW <= from || start >= to;
      if (request.ahead ? !wanted.length : !behind) continue;
      request.stop.abort();
      LIST.pending.delete(start);
    }

    /* Only once the query has settled and only while a window is cheap: one
     * window over 2,241,762 rows is 3.2 ms on `modified` against 2,463.6 ms
     * on `path`. The rows on screen are fetched at any price. */
    const affordable = LIST.cost <= READ_AHEAD_MAX;
    let ahead = null;
    if (affordable && Date.now() - LIST.since > SETTLED &&
        !wanted.length && LIST.pending.size === 0) {
      ahead = nextMissing(from, to);
      if (ahead !== null) wanted.push(ahead);
    }

    const started = [];
    for (const start of wanted.slice(0, Math.max(0, INFLIGHT - LIST.pending.size))) {
      const request = {
        generation,
        query: LIST.query,
        sort: LIST.sort,
        desc: LIST.desc,
        /* Recorded at the ask: a refresh starting mid-flight must leave this
           window out of date rather than stamped with a newer revision. */
        rev: SHOWING,
        ahead: start === ahead,
        stop: new AbortController(),
      };
      LIST.pending.set(start, request);
      started.push(SERVICE.search(request.query, request.sort, request.desc, WINDOW, start, request.stop.signal)
        .then((res) => {
          /* Clearing an old result set does not cancel its fetch: an old
             answer for window 400 must not delete a newer query's window 400. */
          if (LIST.pending.get(start) !== request) return;
          if (request.generation !== generation ||
              request.query !== LIST.query || request.sort !== LIST.sort ||
              request.desc !== LIST.desc) {
            LIST.pending.delete(start);
            return;
          }
          /* Convert before clearing the pending token, so a malformed row
             throws into a catch that can still name this request. */
          const rows = res.rows.map(fromService);
          LIST.pending.delete(start);
          // The service's own measure, not the round trip: a window queued
          // behind three others takes as long as all four.
          LIST.cost = res.took_us / 1000;
          /* A window already here is being refreshed, so what is in it now
             and was not a moment ago is a file that turned up. */
          const refreshing = LIST.at.has(start);
          putRows(start, rows, refreshing);
          keepWindow(start, request.rev);
          /* Only when the engine reached the end of the matching set: a capped
           * number snaps the scrollbar back to 1.000 on every change. */
          if (res.reading) tellReading(res);
          else if (!res.capped && (!LIST.exact || LIST.total !== res.total)) {
            setTotal(res.total, true);
          }
          if (refreshing) now = Math.floor(Date.now() / 1000);  /* "5 min ago" moves */
          repaint(true);
          /* A slot has come free and the screen may be elsewhere: this is what
             keeps a still list filling without waiting for the hand. */
          fillWindow();
        })
        .catch((e) => {
          if (LIST.pending.get(start) !== request) return;
          LIST.pending.delete(start);
          // Being scrolled past is the ordinary case, not a fault.
          if (e && e.name === "AbortError") return;
          // Not silent: a failed window stays a ghost until something retries
          // it, but it must not masquerade as a draw.
          console.error("scour: a window did not load:", e);
        }));
    }
    /* What this call set going, so `refreshRows` can charge itself for it.
       Every branch above is caught, so this settles rather than rejects. */
    return Promise.all(started);
  }

  /* One door, opening on a frame: an inline paint reads `scrollTop` while the
   * tree is dirty, and that forced layout cost 24 to 32 ms. */
  let paintQueued = false;
  let dirty = false;          /* the rows changed, though the range did not */
  let painted = "";
  /* Not a fixed tick: it wakes when the soonest row's text would change, and
   * not at all with no date column or a hidden window. Only the date cells are
   * rewritten — `writeRow` returns early when the file has not changed. */
  let clockTimer = null;
  const HOUR = 3600;

  /* When `Math.round(s / unit)` next changes; the boundary rounds up, because
   * early is one wasted rewrite and late is the bug. */
  const nextRound = (s, unit) =>
    Math.max(1, Math.ceil((Math.round(s / unit) + 0.5) * unit - s));

  /* One line per band of `ago`, and the two must stay in step. */
  function nextChange(s) {
    if (s < 0) return Math.max(1, -s);          // a timestamp in the future
    if (s < 60) return 1;                       // seconds, counted not rounded
    if (s < HOUR) return nextRound(s, 60);
    if (s < DAY) return nextRound(s, HOUR);
    if (s < 30 * DAY) return nextRound(s, DAY);
    if (s < 365 * DAY) return nextRound(s, 30 * DAY);
    return nextRound(s, 365 * DAY);
  }

  function dateCells() {
    const keys = new Set(["modified", "created", "accessed"]);
    return shownColumns()
      .map((c, i) => (keys.has(c.key) ? { at: i + 1, col: c } : null))
      .filter(Boolean);
  }

  /* The value a date column reads, without asking the column to write itself. */
  const stampOf = (f, key) =>
    key === "modified" ? f.mtime : key === "created" ? f.ctime : f.atime;

  /* One walk over `pool`, writing only what a second can change: the column's
   * own `write` rebuilds titles through `Intl` too, 3.9 ms a tick. */
  function clockPass(write) {
    const where = dateCells();
    const awake = where.length && !document.hidden &&
      !document.getElementById("view-search").hidden;
    if (!awake) return 0;
    let soonest = HOUR;
    for (const tr of pool) {
      const f = tr.__row;
      if (!f) continue;
      for (const { at, col } of where) {
        if (col.key === "accessed" && frozenAtime(f)) continue;
        const v = stampOf(f, col.key);
        if (!v) continue;
        soonest = Math.min(soonest, nextChange(now - v));
        if (!write) continue;
        const td = tr.children[at];
        if (!td) continue;
        const text = ago(v);
        if (td.textContent !== text) td.textContent = text;
        if (col.key === "modified") {
          // Guarded: an unconditional style write invalidates the cell.
          const c = "var(--ink-3)";
          if (td.style.color !== c) td.style.color = c;
        }
      }
    }
    return soonest;
  }

  function tickClock() {
    clockTimer = null;
    now = Math.floor(Date.now() / 1000);
    arm(clockPass(true));
  }

  function scheduleClock() {
    arm(clockPass(false));
  }

  /* Zero means nothing on screen can change, so nothing is armed. */
  function arm(soonest) {
    clearTimeout(clockTimer);
    clockTimer = null;
    if (soonest > 0) clockTimer = setTimeout(tickClock, soonest * 1000);
  }

  /* A shown window may have been hidden for an hour. */
  document.addEventListener("visibilitychange", () => {
    if (document.hidden) { clearTimeout(clockTimer); clockTimer = null; }
    else tickClock();
  });

  function repaint(rowsChanged) {
    if (rowsChanged) dirty = true;
    if (paintQueued) return;
    paintQueued = true;
    requestAnimationFrame(() => {
      paintQueued = false;
      /* Both reads happen here, where the layout is settled. Measuring once
         is wrong: the first paint has nothing in its rows. */
      measurePerRow();
      const sample = pool.find((tr) => tr.dataset.path) || pool[0];
      if (sample) {
        const h = sample.getBoundingClientRect().height;
        /* A clearance bound grows and never oscillates, and is never the pitch. */
        if (h > linePx[MODE]) { linePx[MODE] = h; painted = ""; }
      }
      const head = body.previousElementSibling;
      if (head) headPx = Math.max(headPx, head.getBoundingClientRect().height || 0);
      const now = visibleRange().join();
      if (now === painted && !dirty) {
        return;
      }
      /* Bookkeeping after the draw, or a row that throws wedges the window. */
      try {
        paintWindow();
      } catch (e) {
        dirty = true;
        console.error("scour: a window did not draw:", e);
        return;
      }
      painted = now;
      dirty = false;
      fillWindow();
      /* After the draw: `picturesSoon` restarts on every repaint, so a moving
         scroll never gets as far as asking. */
      picturesSoon();
    });
  }
  const onScroll = () => {
    toTop.hidden = scrollEl.scrollTop < 4;
    if (!toTop.hidden) toTop.textContent = `↑ ${T("To the top")}`;
    repaint(false);
  };
  /* A click, never a frame: the one other place the position is written. */
  const toTop = document.getElementById("to-top");
  toTop.addEventListener("click", () => { scrollEl.scrollTop = 0; toTop.hidden = true; });
  scrollEl.addEventListener("scroll", onScroll, { passive: true });
  addEventListener("resize", () => { charW = 0; painted = ""; repaint(true); });
  document.addEventListener("scour-search-shown", () => { painted = ""; repaint(true); });
  document.fonts.ready.then(() => { charW = 0; painted = ""; repaint(true); });

  /* Only the newest answer may draw: they return out of order. */
  let generation = 0;
  /* The search this keystroke started, so the next one can let it go. */
  let typingSearch = null;
  let lastMeter = null;

