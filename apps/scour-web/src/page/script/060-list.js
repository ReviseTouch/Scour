  /* Only what is on screen is drawn, with spacers sized in pixels: it rests on
   * every row being 31px and a 200-row window landing in milliseconds. */
  /* Measured: paging happens after the merge, so a window at offset N costs one
   * built row per row skipped — 3 ms at 0 against 1,373 ms at 750,000. */
  const REACH = 20_000;

  /* Logical and deliberately not measured: at 1.667 scale the same row draws
   * as 30.6 or 30.99 CSS pixels, and a paint must not size the scroller. */
  let ROW_H = 31;
  /* A `content:` query's rows are two lines: the file, and where it holds the
     text. Every scroll and window reads `ROW_H`, so the switch is this. */
  const TALL_H = 52;
  function setTall(on) {
    const h = on ? TALL_H : 31;
    if (h === ROW_H) return;
    ROW_H = h;
    linePx.detail = h;
    document.body.classList.toggle("tall", on);
  }

  /* The three shapes are one arithmetic, written in lines: `PER_ROW = 1` and
   * `lineH() = ROW_H` give the single-column case back. `visibleRange` answers
   * in items, so nothing that fetches, caches or counts changes shape. */

  /* Decisions, not measurements. `w` is a minimum and the count across is
   * stated, since `auto-fill` could fit one more and index every later tile
   * wrongly. `h` is the pitch; a test fails when it drifts from `--tile-h`. */
  const TILE = {
    icons: { w: 132, h: 104 },
    large: { w: 204, h: 172 },
  };
  const MODES = ["detail", "icons", "large"];
  /* Which of the three is showing; the service holds it (see `applySettings`)
     and it is `detail` until that answer lands. */
  let MODE = "detail";
  const grid = () => MODE !== "detail";

  /* The largest line each shape has drawn: a clearance bound, never the
     logical pitch. Per shape, or a 31px row shares a 172px tile's maximum. */
  const linePx = { detail: ROW_H, icons: TILE.icons.h, large: TILE.large.h };
  let headPx = ROW_H;        /* largest heading seen; likewise */
  /* A browser stops at about 33.5 M pixels and 2.1 M rows at 31px want 66 M,
     so above it the rows pack into whatever fits. */
  const MAX_PX = 33_000_000;
  const WINDOW = 200;        /* rows fetched at a time */
  /* Extra rows either side of the viewport. Sixty rebuilt 137 rows a scroll,
   * 18.5 ms against a 16.7 ms frame; this covers one frame's travel. */
  const MARGIN = 20;

  /* The same, in a grid, counted in nodes rather than pixels: two lines is
   * `2 * PER_ROW`, the same order as forty rows at any width. */
  const GRID_MARGIN = 2;

  const LIST = {
    query: null, sort: null, desc: null,
    total: 0,                /* what the scrollbar is sized by */
    rows: new Map(),
    /* Every path the cache holds, and how many rows hold it: constant time
     * over twenty thousand entries. Counted, since a file that moves down is
     * briefly at two indices. */
    seen: new Map(),         /* path -> how many rows hold it */
    pending: new Map(),      /* start -> the exact request in flight */
    exact: false,            /* has the exact count landed yet */
    since: 0,                /* when this result set landed */
    /* What one window costs the service, in its own measure. Read by the
     * reading-ahead; see `READ_AHEAD_MAX`. */
    cost: 0,
    /* Which revision each window came from: positions move, so only visible
     * ones are refetched — replacing the map discarded twenty thousand rows
     * 1.26 times a second. An entry also means fetched, empty or not. */
    at: new Map(),           /* window start -> the revision it was fetched at */
  };

  /* Every write goes through `putRows` and every removal through `dropRows`,
   * so `LIST.seen` cannot drift from what the cache holds. */

  const fullPath = (f) => f.path + "/" + f.name;

  /* One window, replacing those indices. Only a refresh can hold an arrival,
   * or the whole list flashes when the reading-ahead first runs. */
  function putRows(start, rows, mark) {
    const at = Date.now();
    if (mark) {
      /* Aged out here as well as on the timer, which only runs when the
         refreshes stop: rescheduling it held rows lit for 7.8 s. */
      for (const [p, when] of arrivals) if (when <= at - FRESH_MS) arrivals.delete(p);
      /* Asked before the swap below, while `seen` still describes the list
         as it was a moment ago. */
      for (const f of rows) {
        const p = fullPath(f);
        if (!LIST.seen.has(p) && !arrivals.has(p)) arrivals.set(p, at);
      }
    }
    /* The whole window, not only what came back: a short answer means the
       list has shrunk, and the rows past the new end are not rows. */
    dropRows(start, WINDOW);
    rows.forEach((f, i) => {
      const p = fullPath(f);
      /* The moment it arrived, not a flag: the row is redrawn while it is
         highlighted and the fade has to carry on rather than restart. */
      if (arrivals.has(p)) f.fresh = arrivals.get(p);
      LIST.rows.set(start + i, f);
      LIST.seen.set(p, (LIST.seen.get(p) || 0) + 1);
    });
    if (mark && arrivals.size) armFresh();
  }

  function dropRows(start, count) {
    for (let i = start; i < start + count; i++) {
      const f = LIST.rows.get(i);
      if (!f) continue;
      LIST.rows.delete(i);
      const p = fullPath(f);
      const n = LIST.seen.get(p);
      if (n > 1) LIST.seen.set(p, n - 1);
      else LIST.seen.delete(p);
    }
  }

  function clearRows() {
    LIST.rows.clear();
    LIST.seen.clear();
    LIST.at.clear();
  }

  /* Rows past the end, once the exact count says where it is: their paths
     would otherwise sit in `seen` and suppress a later arrival. */
  function dropBeyond() {
    const end = reach();
    for (const i of [...LIST.rows.keys()]) if (i >= end) dropRows(i, 1);
    for (const s of [...LIST.at.keys()]) if (s >= end) LIST.at.delete(s);
  }

  /* A short window means the list ended inside it, so a count saying otherwise
   * makes it out of date — without this the screen reads `1 / 2.235.938` over
   * an empty list. Only the stamp goes, which `fillWindow` refetches. */
  function dropShort() {
    /* A capped count is a floor, not a length, and contradicts nothing. */
    if (!LIST.exact) return;
    const end = reach();
    for (const start of [...LIST.at.keys()]) {
      if (start >= end) continue;              /* dropBeyond's half */
      const last = start + Math.min(WINDOW, end - start) - 1;
      if (!LIST.rows.has(last)) LIST.at.delete(start);
    }
  }

  const KEEP_WINDOWS = 32;     /* 6,400 rows, independent of scroll distance */
  const AHEAD_WINDOWS = 2;     /* at most two windows beyond either edge */

  function keepWindow(start, revision) {
    LIST.at.delete(start);
    LIST.at.set(start, revision);
    if (LIST.at.size <= KEEP_WINDOWS) return;
    const [from, to] = visibleRange();
    for (const old of [...LIST.at.keys()]) {
      if (LIST.at.size <= KEEP_WINDOWS) break;
      if (old < to && old + WINDOW > from) continue;
      if (LIST.pending.has(old)) continue;
      dropRows(old, WINDOW);
      LIST.at.delete(old);
    }
  }

  /* The one way to say how long the list is: `empty.hidden`, `dropBeyond` and
   * `dropShort` all follow from it. */
  function setTotal(total, exact) {
    LIST.total = total;
    LIST.exact = exact;
    if (grepReady) drawGrepMeter(false);
    empty.hidden = LIST.total !== 0;
    if (LIST.total !== 0) similarBox.hidden = true;
    dropBeyond();
    dropShort();
  }

  const scrollEl = document.querySelector(".scroll");

  /* What a search that found nothing nearly found, when the service offers
     it: a count, and the query that shows them — its words written `fuzzy:`. */
  const similarBox = document.getElementById("similar");
  let SIMILAR = "";

  /* The narrowing strip: what the results hold, each a term a press adds —
     the commonest kinds and extensions, the folders under home that hold the
     most, and the last week. One `facets` walk and one `usage`, after the
     search has landed; nothing for a query that reads files. The usage names
     every child: it ranks them by bytes, the strip by files. */
  const ctxRow = document.getElementById("ctxrow");
  const chipsBox = document.getElementById("chips");
  const narrowLbl = document.getElementById("narrow-lbl");
  const narrowToggleBtn = document.getElementById("m-narrow");
  let narrowGen = 0, narrowStop = null, narrowTimer = 0, CHIPS = [];
  let NARROW_PINNED = false, NARROW_HIDDEN = false;
  function syncTriggers(text) {
    const ts = terms(text);
    const hasField = (names) => ts.some(([a, b]) => {
      const sp = fieldSplit(text.slice(a, b).replace(/^!/, ""));
      const f = sp && LENS_FIELDS[fold(sp[0])];
      return f && names.includes(f);
    });
    document.getElementById("q-loc").classList.toggle("on", hasField(["under", "parent"]));
    document.getElementById("q-time").classList.toggle("on", hasField(["dm"]));
    document.getElementById("q-fuzzy").classList.toggle("on", hasField(["fuzzy", "letters"]) || !!(KEPT && KEPT.typos === "always"));
    document.getElementById("q-grep").classList.toggle("grep-on", !grepBar.hidden || hasField(["content"]));
    drawScopeChip(text);
  }
  function drawNarrow() {
    const emptyQ = !q.value.trim();
    const wrong = qread.classList.contains("warn") || !!qread.querySelector(".fix");
    const allowStrip = wrong || (!NARROW_HIDDEN && (!emptyQ || NARROW_PINNED));
    const show = !wrong && CHIPS.length > 0 && (!emptyQ || NARROW_PINNED);
    chipsBox.hidden = !show;
    narrowLbl.hidden = !show;
    qread.style.display = show ? "none" : "";
    chipsBox.innerHTML = show ? CHIPS.map((c, i) =>
      (i === 0 || CHIPS[i - 1].g !== c.g ? '<span class="cg" aria-hidden="true">' + (FIELD_ART[c.g] || "") + "</span>" : "") +
      '<button type="button" data-i="' + i + '" title="' + escape_(c.term) + '">' + escape_(c.label) +
      "<b>" + fmt(c.n) + "</b></button>").join("") : "";
    document.getElementById("narrow").title = qread.textContent || "";
    const hasRead = !emptyQ && !qread.hidden && !!qread.textContent.trim();
    ctxRow.hidden = !allowStrip || (!show && !wrong && !hasRead);
    if (narrowToggleBtn) narrowToggleBtn.setAttribute("aria-pressed", String(!ctxRow.hidden));
  }
  document.getElementById("ctx-close").addEventListener("click", () => {
    NARROW_PINNED = false;
    NARROW_HIDDEN = true;
    drawNarrow();
  });
  narrowToggleBtn.addEventListener("click", () => {
    if (!ctxRow.hidden) {
      NARROW_PINNED = false;
      NARROW_HIDDEN = true;
      drawNarrow();
    } else {
      NARROW_HIDDEN = false;
      NARROW_PINNED = true;
      narrowSoon(q.value, LIST.total || 100);
      drawNarrow();
    }
  });
  const NARROW_SIZES = [["<100kb", "under 100 KB"], [">1mb", "over 1 MB"], [">100mb", "over 100 MB"], [">1gb", "over 1 GB"]];
  function narrowSoon(text, total) {
    clearTimeout(narrowTimer);
    const mine = ++narrowGen;
    if (narrowStop) narrowStop.abort();
    if ((!text.trim() && !NARROW_PINNED) || NARROW_HIDDEN || READS.test(text) || total < 2 || (KEPT && KEPT.narrow_off)) {
      CHIPS = [];
      drawNarrow();
      return;
    }
    narrowTimer = setTimeout(() => {
      narrowStop = new AbortController();
      const signal = narrowStop.signal;
      const has = (f) => terms(text).some(([a, b]) => new RegExp("^" + f + ":", "i").test(text.slice(a, b)));
      const sized = has("size") ? [] : NARROW_SIZES;
      Promise.all([
        SERVICE.get("facets", { q: text, by: "kind,ext,age", edges: "1,7,30,365" }, signal).catch(() => null),
        HOME && !has("under") && !has("parent") ? SERVICE.get("usage", { path: HOME, top: 4294967295, q: text }, signal).catch(() => null) : null,
        Promise.all(sized.map(([term]) =>
          SERVICE.get("count", { q: text + " size:" + term, cap: 1000000 }, signal).then((r) => (r.capped ? 0 : r.total)).catch(() => 0))),
      ]).then(([f, u, counts]) => {
        if (mine !== narrowGen || q.value !== text) return;
        const out = [];
        const all = f ? f.total : total;
        const group = (by) => ((f && f.groups.find((g) => g.by === by)) || { facets: [] }).facets;
        if (!has("kind")) for (const x of group("kind").slice(0, 5)) {
          if (x.count > 0 && x.count < all) out.push({ g: "kind", label: KIND_KEY[x.key] || x.key, n: x.count, term: "kind:" + x.key });
        }
        if (!has("ext")) for (const x of group("ext").slice(0, 6)) {
          if (x.key && x.count > 0 && x.count < all) out.push({ g: "ext", label: "." + x.key, n: x.count, term: "ext:" + x.key });
        }
        if (!has("dm")) {
          /* The bands are apart: a chip says "within", so each counts the ones before it too. */
          const bands = group("age");
          let within = 0;
          for (const [key, label, term] of [["1", "last 24 hours", "dm:24h"], ["7", "last 7 days", "dm:7d"],
                                            ["30", "last 30 days", "dm:30d"], ["365", "last year", "dm:1y"]]) {
            const b = bands.find((x) => x.key === key);
            within += b ? b.count : 0;
            if (within > 0 && within < all) out.push({ g: "dm", label: T(label), n: within, term });
          }
        }
        sized.forEach(([term, label], i) => {
          if (counts[i] > 0 && counts[i] < all) out.push({ g: "size", label: T(label), n: counts[i], term: "size:" + term });
        });
        if (u) for (const c of u.children.filter((c) => c.files > 0 && c.files < all).sort((a, b) => b.files - a.files).slice(0, 4)) {
          out.push({ g: "under", label: homeForm(c.path), n: c.files, term: "under:" + quoteIf(homeForm(c.path)) });
        }
        CHIPS = out;
        drawNarrow();
      });
    }, 250);
  }
  chipsBox.addEventListener("wheel", (e) => {
    if (chipsBox.scrollWidth <= chipsBox.clientWidth) return;
    e.preventDefault();
    chipsBox.scrollLeft += e.deltaX + e.deltaY;
  }, { passive: false });
  /* A press adds the term, or puts it in place of the one of the same field;
     with Alt held it excludes instead. */
  chipsBox.addEventListener("mousedown", (e) => {
    const b = e.target.closest("button");
    const c = b && CHIPS[+b.dataset.i];
    if (!c) return;
    e.preventDefault();
    const term = (e.altKey ? "!" : "") + c.term;
    const field = c.term.slice(0, c.term.indexOf(":") + 1);
    const text = q.value;
    const same = !e.altKey && terms(text).find(([a, x]) => text.slice(a, x).toLowerCase().startsWith(field));
    let next;
    if (same) next = text.slice(0, same[0]) + term + text.slice(same[1]);
    else next = (text.trim() ? text.replace(/\s+$/, "") + " " : "") + term;
    q.value = next;
    q.focus();
    q.setSelectionRange(next.length, next.length);
    render();
  });

  /* How every word is read, said and switched at the status bar's end. */
  const typosBtn = document.getElementById("m-typos");
  const TYPOS_SAID = { "": "offer it", always: "always", off: "off" };
  function drawTypos() {
    const v = (KEPT && KEPT.typos) || "";
    typosBtn.textContent = T("Similar spelling: {how}", { how: T(TYPOS_SAID[v] || "offer it") });
  }
  typosBtn.addEventListener("click", () => {
    const v = (KEPT && KEPT.typos) || "";
    const next = { "": "always", always: "off", off: "" }[v];
    KEPT = { ...(KEPT || {}), typos: next };
    keep({ typos: next });
    drawTypos();
    // Read again under the new rule once the service has it.
    setTimeout(render, 150);
  });
  document.getElementById("m-disk").addEventListener("click", () => {
    const b = document.querySelector('.set-nav button[data-sec="disks"]');
    document.getElementById("settings-btn").click();
    if (b) b.click();
  });

  /* Nothing found: each filter, left out on its own, and what would then be
     found. Counted, never searched, and only for this query. */
  const rescueBox = document.getElementById("rescue");
  let rescueGen = 0;
  function offerRescue(text, found) {
    const mine = ++rescueGen;
    rescueBox.hidden = true;
    rescueBox.innerHTML = "";
    if (found || !text.trim() || READS.test(text)) return;
    // A field still being typed (`dm:`) is not a filter yet.
    const filters = terms(text).filter(([a, b]) =>
      /cap/.test(termClass(text.slice(a, b))) && !/:"?"?$/.test(text.slice(a, b)));
    if (!filters.length) return;
    const asks = filters.slice(0, 4).map(([a, b]) => {
      const without = (text.slice(0, a) + text.slice(b)).replace(/\s+/g, " ").trim();
      return SERVICE.get("count", { q: without, cap: COUNT_CAP })
        .then((r) => ({ term: text.slice(a, b), without, n: r.total, capped: r.capped }))
        .catch(() => null);
    });
    Promise.all(asks).then((got) => {
      if (mine !== rescueGen || q.value !== text) return;
      const useful = got.filter((g) => g && g.n > 0);
      if (!useful.length) return;
      rescueBox.innerHTML = "<span>" + escape_(T("Left out, one of these would find something:")) + "</span>" +
        useful.map((g, i) => '<button type="button" data-i="' + i + '"><code>' + escape_(g.term) + "</code>" +
          escape_(T("without it: {n} results", { n: fmt(g.n) + (g.capped ? "+" : "") })) + "</button>").join("");
      rescueBox.__offers = useful;
      rescueBox.hidden = false;
    });
  }
  rescueBox.addEventListener("click", (e) => {
    const b = e.target.closest("button");
    const g = b && rescueBox.__offers && rescueBox.__offers[+b.dataset.i];
    if (!g) return;
    q.value = g.without;
    render();
    q.focus();
  });

  let similarGen = 0;
  function offerSimilar(similar) {
    const mine = ++similarGen;
    const rowsEl = document.getElementById("similar-rows");
    similarBox.hidden = !similar;
    if (rowsEl) rowsEl.innerHTML = "";
    if (!similar) { SIMILAR = ""; return; }
    SIMILAR = similar.query;
    document.getElementById("similar-said").textContent =
      T("Similar: {n} files —", { n: fmt(similar.total) + (similar.capped ? "+" : "") });
    document.getElementById("similar-q").textContent = similar.query;
    SERVICE.search(similar.query, sortKey, sortDesc, 3, 0)
      .then((r) => {
        if (mine !== similarGen || similarBox.hidden || !rowsEl) return;
        const hits = (r.rows || []).slice(0, 3);
        rowsEl.innerHTML = hits.map((h, idx) =>
          '<button type="button" class="sim-row" data-si="' + idx + '">' +
          '<span class="sn">' + escape_(h.name) + '</span>' +
          '<span class="sp">' + escape_(homeForm(h.dir)) + "</span></button>"
        ).join("");
        rowsEl.__hits = hits.map(fromService);
      })
      .catch(() => {});
  }
  document.getElementById("similar-show").addEventListener("click", () => {
    if (!SIMILAR) return;
    q.value = SIMILAR;
    render();
    q.focus();
  });
  document.getElementById("similar-always").addEventListener("click", () => {
    KEPT = { ...(KEPT || {}), typos: "always" };
    keep({ typos: "always" });
    drawTypos();
    setTimeout(render, 120);
    q.focus();
  });
  document.getElementById("similar-rows").addEventListener("click", (e) => {
    const b = e.target.closest(".sim-row");
    const rowsEl = document.getElementById("similar-rows");
    const f = b && rowsEl.__hits && rowsEl.__hits[+b.dataset.si];
    if (!f) return;
    setPeek(true, false);
    drawPeek(f);
  });

