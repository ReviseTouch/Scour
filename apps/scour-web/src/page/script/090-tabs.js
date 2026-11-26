  /* ---------------- search tabs ----------------
   * Each tab its own query and order, kept by the service as `tabs`, so the
   * window and this page show the same ones. The keys are Alt's: a browser
   * keeps Ctrl+T and Ctrl+W for itself, and Ctrl+W closes this window. */
  let TABS = [];
  let TAB = 0;
  let TABS_READ = false;
  let tabsTimer = 0;
  const strip = document.getElementById("stabs");
  const freshTab = () => ({ query: "", sort: "modified", descending: true, content: "" });
  /* The content line's pattern is the tab's too, while the line is open. */
  const nowTab = () => ({
    query: q.value, sort: sortKey, descending: sortDesc,
    content: grepBar.hidden ? "" : grepInput.value,
  });
  const keepTabs = () => keep({ tabs: TABS, tab: TAB });

  function drawTabs() {
    if (!TABS.length) TABS = [nowTab()];
    const searching = document.getElementById("tab-report").getAttribute("aria-selected") !== "true";
    /* Always, not from two up: a strip that came with a second tab was never
       found. The only tab has no ×. */
    const off = !!(KEPT && KEPT.tabs_off);
    document.getElementById("tab-new").hidden = off;
    strip.hidden = !searching || off;
    const closable = TABS.length > 1;
    if (strip.hidden) return;
    const esc = (t) => t.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
    strip.innerHTML = TABS.map((t, i) => {
      /* Ordered other than newest first, a tab says by what and which way. */
      const col = COLUMNS.find((c) => c.key === t.sort);
      const sorted = t.sort !== "modified" || t.descending === false
        ? ` · ${col ? T(col.msgid) : t.sort} ${t.descending === false ? "▴" : "▾"}` : "";
      const name = esc((t.query.trim() || T("New tab")) + sorted);
      return `<div class="stab" role="tab" tabindex="0" data-i="${i}" aria-selected="${i === TAB}" title="${name}">`
        + `<span class="nm">${name}</span>`
        + (closable ? `<button type="button" class="x" data-close="${i}" aria-label="${esc(T("Close tab"))}">×</button>` : "")
        + `</div>`;
    }).join("") + `<button type="button" class="tabplus" aria-label="${esc(T("New tab (Alt+T)"))}" title="${esc(T("New tab (Alt+T)"))}">+</button>`;
  }

  /* What is on screen goes into the tab in front, and is kept a moment later. */
  function holdTab() {
    if (!TABS.length) TABS = [nowTab()];
    TABS[TAB] = nowTab();
    drawTabs();
    /* Not before the kept tabs are read: the first search would write one
       tab over all of them. */
    if (!TABS_READ) return;
    clearTimeout(tabsTimer);
    tabsTimer = setTimeout(keepTabs, 2000);
  }

  function showTab(t) {
    if (COLUMNS.some((c) => c.key === t.sort)) sortKey = t.sort;
    sortDesc = typeof t.descending === "boolean" ? t.descending : true;
    q.value = t.query || "";
    /* Before `render`, which holds what is on screen into this tab. */
    putGrep(t.content || "");
    render();
    q.focus();
  }

  function moved() {
    drawTabs();
    clearTimeout(tabsTimer);
    keepTabs();
    showTab(TABS[TAB]);
  }

  function tabNew() {
    holdTab();
    if (TABS.length >= 24) return;
    TABS.push(freshTab());
    TAB = TABS.length - 1;
    moved();
  }

  function tabSwitch(i) {
    if (i === TAB || i < 0 || i >= TABS.length) return;
    holdTab();
    TAB = i;
    moved();
  }

  /* The one to the right takes a closed tab's place, as a browser does. */
  function tabClose(i) {
    if (TABS.length <= 1 || i < 0 || i >= TABS.length) return;
    holdTab();
    const was = TAB;
    TABS.splice(i, 1);
    if (i < was || was >= TABS.length) TAB = Math.max(0, Math.min(was - 1, TABS.length - 1));
    if (i === was) { moved(); return; }
    drawTabs();
    keepTabs();
  }

  const tabStep = (by) => tabSwitch(((TAB + by) % TABS.length + TABS.length) % TABS.length);

  strip.addEventListener("click", (e) => {
    const x = e.target.closest("[data-close]");
    if (x) { tabClose(Number(x.dataset.close)); return; }
    if (e.target.closest(".tabplus")) { tabNew(); return; }
    const t = e.target.closest(".stab");
    if (t) tabSwitch(Number(t.dataset.i));
  });
  /* The middle button closes, the empty strip opens one on a double-click. */
  strip.addEventListener("auxclick", (e) => {
    const t = e.target.closest(".stab");
    if (t && e.button === 1) { e.preventDefault(); tabClose(Number(t.dataset.i)); }
  });
  strip.addEventListener("dblclick", (e) => { if (e.target === strip) tabNew(); });
  strip.addEventListener("keydown", (e) => {
    const t = e.target.closest(".stab");
    if (t && (e.key === "Enter" || e.key === " ")) { e.preventDefault(); tabSwitch(Number(t.dataset.i)); }
  });
  document.getElementById("tab-new").addEventListener("click", tabNew);
  document.addEventListener("keydown", (e) => {
    if (!e.altKey || e.ctrlKey || e.metaKey) return;
    if (KEPT && KEPT.tabs_off) return;
    const k = e.key.toLowerCase();
    if (k === "t") tabNew();
    else if (k === "w") tabClose(TAB);
    else if (e.key === "PageDown") tabStep(1);
    else if (e.key === "PageUp") tabStep(-1);
    else return;
    e.preventDefault();
    e.stopPropagation();
  }, true);

  /* The tabs the service keeps, once: a later settings answer is this page's
     own writes coming back. True when the front tab's query was put in. */
  function readTabs(s) {
    if (TABS_READ) return false;
    TABS_READ = true;
    if (!Array.isArray(s.tabs) || !s.tabs.length) { drawTabs(); return false; }
    TABS = s.tabs.slice(0, 24).map((t) => ({
      query: typeof t.query === "string" ? t.query : "",
      sort: typeof t.sort === "string" && t.sort ? t.sort : "modified",
      descending: typeof t.descending === "boolean" ? t.descending : true,
      content: typeof t.content === "string" ? t.content : "",
    }));
    TAB = Math.min(Math.max(0, Number(s.tab) || 0), TABS.length - 1);
    const f = TABS[TAB];
    if (COLUMNS.some((c) => c.key === f.sort)) sortKey = f.sort;
    sortDesc = f.descending;
    if (f.content) putGrep(f.content);
    drawTabs();
    if (f.query && f.query !== q.value) { q.value = f.query; return true; }
    return false;
  }

  function render() {
    /* A pasted line break is a space: the box wraps, the query does not. */
    if (/[\r\n]/.test(q.value)) {
      const c = q.selectionStart;
      q.value = q.value.replace(/\r\n?|\n/g, " ");
      q.setSelectionRange(c, c);
    }
    holdTab();
    const text = q.value;
    /* Asked now: once the search lands, a re-sort looks like a new query. */
    const queryChanged = LIST.query !== text;
    /* Everything that depends only on the text happens at typing speed. */
    paint(text);
    syncTriggers(text);
    refreshReading(text);
    showHints();
    /* The lens is help, never in the way: a fault in it must not stop the search. */
    try { lensSync(false); } catch (e) { console.error("scour: the lens did not open:", e); lensClose(); }
    syncHeads();
    if (queryChanged && !grepBar.hidden && grepInput.value) grepSoon(false);

    /* One search in flight: a window is around a hundred kilobytes, and six
     * are parsed before `generation` discards them. `generation` stays — what
     * may draw is not what is worth finishing. */
    const mine = ++generation;
    if (typingSearch) typingSearch.abort();
    typingSearch = new AbortController();
    SERVICE.search(text, sortKey, sortDesc, WINDOW, 0, typingSearch.signal)
      .then((res) => {
        if (mine !== generation) return;   // a newer keystroke won
        /* Facets and an exact total belong to the set, not its order: a sort
           does not recount, so its floor would stick at `1000+`. */
        const keepExact = !queryChanged && LIST.exact && res.capped;
        LIST.query = text; LIST.sort = sortKey; LIST.desc = sortDesc;
        setTall(READS.test(text));
        /* And so did the selection: a different query is different files. */
        if (queryChanged) {
          focusPath = null;
          wantFocusPath = null;
          clearSel();
        }
        clearRows();
        for (const request of LIST.pending.values()) request.stop.abort();
        LIST.pending.clear();
        /* Before `putRows`: `clearRows` has just emptied `LIST.at`. */
        if (!keepExact) setTotal(res.total, !res.capped);
        READING = res.reading || null;
        offerSimilar(res.total === 0 ? res.similar : null);
        offerRescue(text, res.total !== 0);
        if (queryChanged) narrowSoon(text, res.total);
        LIST.since = Date.now();
        /* The first window, and the first word on what the rest would cost. */
        LIST.cost = res.took_us / 1000;
        /* Unstamped, `fillWindow` refetches window zero a frame later. */
        SHOWING = REV;
        const rows = res.rows.map(fromService);
        putRows(0, rows, false);
        keepWindow(0, SHOWING);
        scrollEl.scrollTop = 0;
        FILES = rows;
        lastMeter = res;
        /* The new rows and the old counts must not describe two sets. */
        if (queryChanged) markSidebarStale();
        draw(text, parse(text), FILES, res);
        /* Not on this keystroke, and not for a sort. See `sidebarSoon`. */
        if (queryChanged) {
          sidebarCost = 0;
          sidebarSoon();
        }
      })
      .catch((e) => {
        // Abandoned by the next keystroke, not a service that went away.
        if (e.name === "AbortError") return;
        if (mine !== generation) return;
        offline(e);
      });
  }

