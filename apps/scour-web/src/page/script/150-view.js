  let waiting = false;
  /* Set when this face has handed over; everything that asks reads it. */
  let HANDED_OVER = false;
  async function watch() {
    if (waiting || HANDED_OVER) return;
    waiting = true;
    try {
      /* A hidden window stops waiting: waiting puts the service on the burst
         clock rather than batching. */
      while (!document.hidden && !HANDED_OVER) {
        let s;
        try {
          s = await SERVICE.wait(REV, 25000);
        } catch {
          /* Gone: try again slowly rather than in a tight loop. */
          await new Promise((r) => setTimeout(r, 3000));
          continue;
        }
        applyStatus(s);
        if (s.revision !== REV) {             /* `!=`: a restart counts back from zero */
          REV = s.revision;
          indexMoved();
          /* The person's beat, `refresh_every`: waiting again at once
             re-read the rows in sight every time the index moved. */
          const every = (KEPT && KEPT.refresh_every) || 2;
          await new Promise((r) => setTimeout(r, every * 1000));
        }
      }
    } finally {
      waiting = false;
    }
  }
  /* Pictures, remembered under `view.web`. Off is a real off: the rows are
   * drawn without the `<img>` rather than hiding it, so nothing is fetched. */
  let ICONS = true;
  const iconBtn = document.getElementById("icons");
  function applyIcons() {
    iconBtn.setAttribute("aria-pressed", String(ICONS));
    document.body.classList.toggle("no-icons", !ICONS);
    draw(q.value, parse(q.value), FILES, lastMeter || { total: LIST.total, capped: !LIST.exact, took_us: 0, rows_visited: 0 });
  }
  iconBtn.addEventListener("click", () => {
    ICONS = !ICONS;
    keepView({ icons: ICONS });
    applyIcons();
  });
  iconBtn.setAttribute("aria-pressed", String(ICONS));

  /* A folder nobody has opened has no thumbnails, so the desktop's own
   * thumbnailer is asked; how many decoders run is the service's. The four
   * bounds below are quiet, batch, one request at a time, and memory. */
  const PICTURE_QUIET = 250;
  const PICTURE_BATCH = 24;
  const PICTURE_MEMORY = 4096;
  /* With `--no-thumbnails` the route answers 403; asked once. */
  let picturesRefused = false;
  let askingPictures = false;
  const askedForPictures = new Set();

  /* Oldest out first: a `Set` iterates in insertion order. */
  function rememberAsking(path) {
    if (askedForPictures.size >= PICTURE_MEMORY) {
      askedForPictures.delete(askedForPictures.values().next().value);
    }
    askedForPictures.add(path);
  }

  function askForPictures() {
    if (!ICONS || picturesRefused || askingPictures || LIST.query === null) return;
    const [from, to] = visibleRange();
    const paths = [];
    for (let i = from; i < to && paths.length < PICTURE_BATCH; i++) {
      const f = LIST.rows.get(i);
      /* `make` is the bridge saying the machine declares a thumbnailer for
         this type, so the page never guesses from an extension. */
      if (!f || !f.make || f.thumb) continue;
      const p = fullPath(f);
      if (askedForPictures.has(p)) continue;
      rememberAsking(p);
      paths.push(p);
    }
    if (!paths.length) return;
    askingPictures = true;
    /* Returned so `afterQuiet` charges itself: a batch of videos is seconds. */
    return SERVICE.thumbs(paths)
      .then((res) => {
        askingPictures = false;
        if (!res || !res.ready || !res.ready.length) return;
        const made = new Set(res.ready);
        /* Over the visible range: `LIST.rows` reaches twenty thousand. */
        const [nowFrom, nowTo] = visibleRange();
        let any = false;
        for (let i = nowFrom; i < nowTo; i++) {
          const f = LIST.rows.get(i);
          if (f && !f.thumb && made.has(fullPath(f))) { f.thumb = true; any = true; }
        }
        if (any) repaint(true);
        /* The rest of the screen, through the same quiet timer. */
        picturesSoon();
      })
      .catch((e) => {
        askingPictures = false;
        if (e && /\b403\b/.test(String(e.message))) {
          picturesRefused = true;
          return;
        }
        /* Not retried: a dead bridge then costs one request a screen. */
        console.error("scour: pictures could not be asked for:", e);
      });
  }
  const picturesSoon = afterQuiet(askForPictures, PICTURE_QUIET);

  /* The item is kept across the change, not the pixel, and the extent is
   * stated before the position because a browser clamps `scrollTop` to the one
   * it has. The only place here that writes it, and only from a click. */
  const viewBox = document.getElementById("views");
  const viewBtns = [...viewBox.querySelectorAll("button")];

  const topItem = () =>
    Math.min(reach(), Math.floor(scrollEl.scrollTop / pitch()) * PER_ROW);

  function applyMode(next) {
    const at = topItem();
    MODE = MODES.includes(next) ? next : "detail";
    for (const b of viewBtns) {
      b.setAttribute("aria-pressed", String(b.dataset.view === MODE));
    }
    /* Both: the sheet asks "a grid?" and "which of the two?" separately. */
    document.body.classList.toggle("grid", grid());
    document.body.dataset.view = MODE;
    applyWidths();
    // Forces the layout above, so the width below is the new one.
    measurePerRow();
    sizedTo = Math.round(lines() * pitch());
    sizer.style.height = `${sizedTo}px`;
    scrollEl.scrollTop = Math.round(Math.floor(at / PER_ROW) * pitch());
    for (const tr of pool) tr.__stamp = "";
    painted = "";
    repaint(true);
  }

  viewBox.addEventListener("click", (e) => {
    const b = e.target.closest("button[data-view]");
    if (!b || b.dataset.view === MODE) return;
    applyMode(b.dataset.view);
    keep({ layout: MODE });
  });

  /* The help, behind a button and remembered under `view.side`. */
  let SIDE = false;
  const aboutBtn = document.getElementById("about");
  const helpBox = document.getElementById("help");

  /* From the table the parser and the completions share; `about` is a msgid. */
  function buildHelpFields() {
    const dl = document.getElementById("help-fields");
    const out = [];
    for (const f of FIELDS) {
      const dt = document.createElement("dt");
      dt.textContent = f.eg || f.name;
      const dd = document.createElement("dd");
      dd.textContent = T(f.about);
      out.push(dt, dd);
    }
    dl.replaceChildren(...out);
  }

  /* Measured rather than positioned by constant: the bar's height depends on
   * the font and on how the meter wrapped. The clamp keeps it on screen. */
  function placeHelp() {
    const b = aboutBtn.getBoundingClientRect();
    const w = helpBox.offsetWidth;
    const left = Math.max(10, Math.min(b.right - w, innerWidth - w - 10));
    helpBox.style.left = `${Math.round(left)}px`;
    helpBox.style.top = `${Math.round(b.bottom + 8)}px`;
  }

  function applySide() {
    aboutBtn.setAttribute("aria-pressed", String(SIDE));
    helpBox.hidden = !SIDE;
    /* No `repaint`: an overlay changes no row and no column. */
    if (SIDE) placeHelp();
  }
  aboutBtn.addEventListener("click", (e) => {
    // Or the listener below reads it as "outside" and closes at once.
    e.stopPropagation();
    SIDE = !SIDE;
    keepView({ side: SIDE });
    applySide();
  });
  document.getElementById("help-x").addEventListener("click", () => {
    SIDE = false;
    keepView({ side: SIDE });
    applySide();
  });
  document.addEventListener("click", (e) => {
    if (SIDE && !e.target.closest("#help")) {
      SIDE = false;
      keepView({ side: SIDE });
      applySide();
    }
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && SIDE) {
      SIDE = false;
      keepView({ side: SIDE });
      applySide();
    }
  });
  addEventListener("resize", () => { if (SIDE) placeHelp(); });
  applySide();
  // The shape the window opens in, before the settings land.
  applyMode(MODE);

  /* This window's settings, once they land; defaults until then. */
  function applyView() {
    ICONS = VIEW.icons !== false;
    SIDE = VIEW.side === true;
    applyIcons();
    applySide();
  }

  /* The window opens on the whole index, not the last keystroke: an empty
   * query is the cheapest there is, its count from `stats().entries`. */
  q.focus();
  q.select();

  buildHeads();
  /* First: taxonomy, places, settings and status only decorate. */
  render();
  /* One status to start from — its revision is what the wait measures
     against — and after that the waiting loop is the heartbeat. */
  SERVICE.status()
    .then((s) => { applyStatus(s); REV = s.revision; })
    .catch(() => {})
    .finally(watch);

  /* What the window remembered, over the defaults it has already drawn with.
     Then what another face asked this page to show — `?q=…&tab=report` from
     the window's "open in the browser" — which comes after, so it wins. */
  /* Newest change first on every opening, in every tab, as the other faces
     open: an order picked stays for the session. See `Settings::opened`. */
  const opened = (s) => {
    delete s.sort;
    delete s.descending;
    for (const tab of Array.isArray(s.tabs) ? s.tabs : []) { delete tab.sort; delete tab.descending; }
    return s;
  };
  SERVICE.settings()
    .then((s) => applySettings(opened(s)))
    .catch(() => {})
    .finally(() => {
      const asked = new URLSearchParams(location.search);
      const aq = asked.get("q");
      if (aq !== null && aq !== q.value) {
        q.value = aq;
        q.dispatchEvent(new Event("input", { bubbles: true }));
      }
      if (asked.get("tab") === "report") document.getElementById("tab-report").click();
    });

  SERVICE.get("places", {})
    .then((res) => {
      HOME = res.home || "";
      MOUNTS = res.mounts || [];
      SCOPES = (res.places || []).map((p) => [p.label, p.path, null]);
      /* Local display data, so nothing is refetched to redraw it. */
      syncFacets(parse(q.value));
      for (const tr of pool) tr.__stamp = "";
      repaint(true);
    })
    .catch(() => {});

  /* `/api/strings` carries the catalogue and the languages; `/api/kinds` the
   * kind labels, the one part of the window with no msgid here. Applied
   * together, or the rail changes a frame before the headings. */
  function applyKinds(res) {
    KINDS = res.kinds.map((k) => [k.label, k.token]);
    KIND_KEY = Object.fromEntries(res.kinds.map((k) => [k.token, k.label]));
    /* Only the word for a row's kind has changed. */
    for (const f of LIST.rows.values()) f.kind = labelOfToken(f.ktoken);
    for (const f of FILES) f.kind = labelOfToken(f.ktoken);
  }

  /* Everything with a word in it: a reload would lose the query, the scroll
   * position and the selection. */
  function applyLanguage() {
    document.documentElement.lang = LANG;
    drawTypos();
    buildFormatters();
    applyMarkup(document);
    buildLangMenu();
    /* The help's field list is generated, so `applyMarkup` cannot reach it. */
    buildHelpFields();
    buildHeads();               // the headings, and the picker's title
    syncFacets(parse(q.value)); // the three rails
    drawSelBar();               // the selection bar, if anything is selected
    drawTabs();                 // "New tab", on a tab with nothing typed
    /* The two lines nothing else rewrites until something happens. */
    drawCost();
    if (LAST_STATUS) applyStatus(LAST_STATUS);
    resRelabel();
    drawAsk();
    drawScopeChip(q.value);     // "Everywhere"
    if (!settingsBox.hidden) showSection(SECTION);
    if (!disksPanel.hidden) drawDisks();
    /* `drawPeek` returns early for the row it shows, so the labels stay. */
    if (PEEK.on && !peek.hidden) {
      const row = focusedRow();
      if (row && row.__row) { peekFactsOf(row.__row); peekFoundOf(row.__row); }
    }
    if (window.SCOUR && window.SCOUR.relabel) window.SCOUR.relabel();
    /* The rows carry `ago(...)` and a kind label, and the pool skips a row
       whose stamp has not moved. */
    for (const tr of pool) tr.__stamp = "";
    painted = "";
    repaint(true);
  }

  /* An empty `tag` means whatever the service works out. */
  function loadLanguage(tag) {
    const params = tag ? { lang: tag } : {};
    return Promise.all([
      SERVICE.get("strings", params),
      SERVICE.get("kinds", params),
    ]).then(([words, kinds]) => {
      LANG = words.lang || "en";
      LANGUAGES = words.languages || [];
      STRINGS = words.strings || {};
      applyKinds(kinds);
      applyLanguage();
    });
  }

  /* Drawn whether or not the save lands: `keep` is fire-and-forget. */
  function chooseLanguage(tag) {
    keep({ language: tag });
    langMenu.hidden = true;
    langBtn.setAttribute("aria-expanded", "false");
    loadLanguage(tag).catch((e) => console.error("scour: language:", e));
  }

  /* The whole result set, not the list: every row, in the columns on screen
   * and the index's own order — no `sort` is sent. A navigation, so the browser
   * owns the progress, the cancel and the file. Guarded: one inline script. */
  const csvBtn = document.getElementById("csv");
  if (csvBtn) {
    csvBtn.addEventListener("click", () => {
      const p = new URLSearchParams({
        q: q.value,
        cols: SHOWN.join(","),
        t: TOKEN,
      });
      location.href = `/api/csv?${p}`;
    });
  }


