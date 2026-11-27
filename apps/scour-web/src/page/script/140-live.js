  /* Kept, because `applyMarkup` blanks `#path-note` back to its msgid and the
   * next `/api/wait` reply can be twenty-five seconds away. */
  let LAST_STATUS = null;
  function applyStatus(s) {
    LAST_STATUS = s;
    disksMoved(s.asking || 0);
    TOTAL = s.entries;
    TOTAL_KNOWN = true;
    let moved = false;
    const note = document.getElementById("path-note");
    if (note) {
      const mb = MB.format(s.index_bytes / 1048576);
      // A singular msgid: most machines have one or two sources.
      const text = (s.sources === 1
        ? T("index {mb} MB · 1 source · {watching} watched", { mb, watching: s.watching })
        : T("index {mb} MB · {sources} sources · {watching} watched", {
            mb, sources: s.sources, watching: s.watching,
          }));
      if (text !== note.textContent) { note.textContent = text; moved = true; }
      /* The same msgid the window and the terminal draw. */
      const walking = document.getElementById("scanning");
      const sep = document.getElementById("scanning-sep");
      const said = s.unwritten ? T("the index cannot be saved — is the disk full?")
        : s.scanning ? T("scanning {n}", { n: fmt(s.scanned || 0) }) : "";
      walking.textContent = said;
      walking.hidden = !said;
      sep.hidden = !said;
    }
    /* `LIST`, not the capped reply: a heartbeat would put `1.000+` back. */
    if (LIST.query !== null) {
      const shown = LIST.exact ? fmt(LIST.total) : fmt(LIST.total) + "+";
      document.getElementById("count").textContent = withIndexTotal(shown);
    }
    /* The status sentence wraps in an `auto` row, so a longer one takes a line
     * from the rows. Guarded: this runs for every `/api/wait` reply. */
    if (moved) repaint(true);
  }

  /* Nothing polls: `/api/wait` answers when the index would, so an idle
   * desktop costs one round trip every twenty-five seconds. What arrives is a
   * revision number; the answer to a change is to ask again. */
  let REV = 0;                 /* the newest revision the service has told us of */
  /* The whole leash: `fillWindow` refetches a window whose stamp is not this,
     and only `refreshRows` moves it up to `REV`, so a missing window is
     fetched on the frame and an out-of-date one waits. */
  let SHOWING = 0;
  let missedWhileAway = false; /* something changed while the window was hidden */
  let freshTimer = null;
  /* What turned up and when, bounded by the highlight's own length. */
  const arrivals = new Map();
  const FRESH_MS = 1600;       /* as long as the window's */

  /* How many times its own cost a background refresh waits: on 1.35 M matches
   * the three-second count refresh took 930 ms each time, and at ten it can
   * never take more than about a tenth of a machine. */
  const COST = 10;

  /* Run `fn` once the calls stop for `ms`, lengthened to what the last cost
   * and capped at `PATIENCE`: one walk's length, so two never overlap. */
  const PATIENCE = 1500;
  function afterQuiet(fn, ms) {
    let timer = null, floor = ms;
    return () => {
      clearTimeout(timer);
      timer = setTimeout(() => {
        timer = null;
        const began = Date.now();
        const out = fn();
        if (out && typeof out.then === "function") {
          const charge = () => {
            floor = Math.min(PATIENCE, Math.max(ms, Date.now() - began));
          };
          out.then(charge, charge);
        }
      }, floor);
    };
  }

  function atMostEvery(fn, ms) {
    let last = -Infinity, timer = null, floor = ms, running = false, pending = false;
    const schedule = () => {
      if (running || timer !== null || !pending) return;
      const wait = Math.max(0, floor - (Date.now() - last));
      if (!wait) { call(); return; }
      timer = setTimeout(() => { timer = null; call(); }, wait);
    };
    const call = () => {
      pending = false;
      running = true;
      last = Date.now();
      const done = () => {
        floor = Math.max(ms, (Date.now() - last) * COST);
        running = false;
        schedule();
      };
      // The cost is unknown until completion, so one trailing call is kept.
      try {
        const out = fn();
        if (out && typeof out.then === "function") out.then(done, done);
        else done();
      } catch (error) {
        done();
        console.error("scour: a refresh failed:", error);
      }
    };
    return () => { pending = true; schedule(); };
  }

  /* The highlight marks the arrival, so it comes off on its own clock. */
  function armFresh() {
    clearTimeout(freshTimer);
    freshTimer = setTimeout(() => {
      const cut = Date.now() - FRESH_MS;
      let any = false;
      for (const [p, when] of arrivals) if (when <= cut) { arrivals.delete(p); any = true; }
      for (const f of LIST.rows.values()) {
        if (f.fresh && !arrivals.has(fullPath(f))) { delete f.fresh; any = true; }
      }
      if (any) repaint(true);
    }, FRESH_MS);
  }

  function rowVisibleAt(idx) {
    if (idx < 0) return false;
    measurePerRow();
    const p = pitch();
    const h = lineH();
    const line = Math.floor(idx / PER_ROW);
    const head = grid() ? (headPx || 0) : (headPx || 33);
    const vis = Math.max(h, scrollEl.clientHeight - (SEL.size ? 30 : 0));
    return line * p + h > scrollEl.scrollTop && head + line * p < scrollEl.scrollTop + vis;
  }

  function rowFullyVisibleAt(idx) {
    if (idx < 0) return false;
    measurePerRow();
    const p = pitch();
    const h = lineH();
    const line = Math.floor(idx / PER_ROW);
    const head = grid() ? (headPx || 0) : (headPx || 33);
    const vis = Math.max(h, scrollEl.clientHeight - (SEL.size ? 30 : 0));
    return line * p >= scrollEl.scrollTop && head + line * p + h <= scrollEl.scrollTop + vis;
  }

  /* `fillWindow`'s job and nobody else's: a second fetcher asks for the same
   * windows a frame later and aborts the first. */
  async function refreshRows() {
    if (document.hidden) { missedWhileAway = true; return; }
    if (LIST.query === null) return;           /* the first search has not landed */
    const pin = focusPath || (SEL.size ? [...SEL.keys()].at(-1) : null);
    const hadAt = pin ? indexOfPath(pin) : -1;
    const follow = !!(pin && (
      (hadAt >= 0 && rowVisibleAt(hadAt)) ||
      (document.activeElement && body.contains(document.activeElement))
    ));
    SHOWING = REV;
    await fillWindow();
    if (SEL.size) {
      for (const p of SEL.keys()) {
        const idx = indexOfPath(p);
        if (idx >= 0) {
          SEL.set(p, LIST.rows.get(idx));
          SEL_AT.set(p, idx);
        }
      }
      drawSelBar();
    }
    if (!follow || LIST.query === null) return;
    let at = indexOfPath(pin);
    if (at < 0 && hadAt >= 0) {
      const base = Math.floor(hadAt / WINDOW);
      for (let step = 1; step <= 2 && at < 0; step++) {
        const next = (base + step) * WINDOW;
        if (next >= reach() || LIST.at.get(next) === SHOWING) continue;
        try {
          const res = await SERVICE.search(LIST.query, LIST.sort, LIST.desc, WINDOW, next);
          if (LIST.query === null) return;
          putRows(next, res.rows.map(fromService), true);
          keepWindow(next, SHOWING);
          at = indexOfPath(pin);
        } catch {
          break;
        }
      }
    }
    if (at < 0) return;
    measurePerRow();
    const p = pitch();
    const h = lineH();
    const line = Math.floor(at / PER_ROW);
    const head = grid() ? (headPx || 0) : (headPx || 33);
    const vis = Math.max(h, scrollEl.clientHeight - (SEL.size ? 30 : 0));
    if (head + line * p + h > scrollEl.scrollTop + vis) {
      scrollEl.scrollTop = Math.ceil(head + line * p + h - vis);
    } else if (line * p < scrollEl.scrollTop) {
      scrollEl.scrollTop = Math.floor(line * p);
    }
    repaint(true);
  }

  /* How dear a sidebar refresh may be and still repeat unasked. `COST` alone
   * settles at a fixed share of the machine — 8.14% of a core on an untouched
   * window — so a dear sidebar keeps its numbers until the query changes. */
  const SIDEBAR_LIVE_UNDER_MS = 40;

  function reviseCounts() {
    if (document.hidden) { missedWhileAway = true; return; }
    if (LIST.query === null) return;
    if (sidebarCost > SIDEBAR_LIVE_UNDER_MS) return;
    // Returned so the throttle above can charge for what it cost.
    return sidebar(LIST.query);
  }

  const rowsSoon = atMostEvery(refreshRows, 400);
  const countsSoon = atMostEvery(reviseCounts, 3000);

  function indexMoved() {
    /* A hidden window is remembered and refetched once, on its return. */
    if (document.hidden) { missedWhileAway = true; return; }
    rowsSoon();
    countsSoon();
    if (!settingsBox.hidden && SECTION === "index") loadIndex();
  }

  document.addEventListener("visibilitychange", () => {
    if (document.hidden) return;
    if (missedWhileAway) { missedWhileAway = false; indexMoved(); }
    watch();                                  /* and start waiting again */
  });

