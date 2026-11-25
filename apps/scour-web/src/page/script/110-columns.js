  /* Which columns are shown and how wide is the service's, keyed by column id.
   * `table-layout: fixed` is what makes a width mean anything. `key` is what
   * the engine sorts by; `write` sets text on a cell that exists. */
  const writeText = (td, f, cls, get) => {
    if (td.className !== cls) td.className = cls;
    const v = get(f);
    if (td.textContent !== v) td.textContent = v;
  };
  const cell = (cls, get) => (td, f) => writeText(td, f, cls, get);

  /* `msgid`, translated at draw time. `id` keys the settings, `key` sorts. */
  const COLUMNS = [
    { id: "name",  msgid: "Name",      key: "name",  w: 240, min: 110, near: 52, far: 30, max: 520,
      write: (td, f, p) => {
        if (td.className !== "name") td.className = "name";
        writeName(td, f, p.text, p);
      } },
    { id: "path",  msgid: "Location",  key: "path",  w: 320, min: 120, near: 48, far: 70, max: 0,
      write: (td, f) => {
        if (td.className !== "loc") td.className = "loc";
        let div = td.firstElementChild;
        if (!div || div.className !== "path") {
          td.textContent = "";
          div = document.createElement("div");
          div.className = "path";
          td.appendChild(div);
        }
        const v = HOME && f.path.startsWith(HOME + "/") ? "~" + f.path.slice(HOME.length) : f.path;
        if (div.textContent !== v) div.textContent = v;
      } },
    /* A folder's number is what the index holds under it, so it carries a `~`
     * — every folder, since nothing knows which hide nothing without a walk. */
    { id: "size",  msgid: "Size",      key: "size",  w: 90, min: 68, near: 0, far: 0, max: 0,  num: true,
      write: (td, f) => {
        cell("n", (x) => (x.is_dir
          ? (x.under ? "~" + bytes(x.under.disk) : "")
          : bytes(x.size)))(td, f);
        const t = f.is_dir && f.under
          ? T("indexed size · {n} files", { n: fmt(f.under.files) })
            + "\n"
            + T("what is excluded (target, node_modules…) is not counted — see the settings")
          : "";
        if (td.title !== t) td.title = t;
      } },
    { id: "mtime", msgid: "Modified",  key: "modified", w: 120, min: 100, near: 0, far: 0, max: 0,
      write: (td, f) => {
        cell("n left", (x) => ago(x.mtime))(td, f);
        td.style.color = "var(--ink-3)";
        const t = exact(f.mtime);
        if (td.title !== t) td.title = t;
      } },
    { id: "kind",  msgid: "Kind",      key: "kind",  w: 110, min: 68, near: 0, far: 0, max: 0,
      write: cell("kind", (f) => f.kind || "") },
    { id: "ext",   msgid: "Extension", key: "ext",   w: 80, min: 56, near: 0, far: 0, max: 0,
      write: cell("kind", (f) => f.ext || "") },
    { id: "ctime", msgid: "Created",   key: "created", w: 120, min: 100, near: 0, far: 0, max: 0,
      write: (td, f) => {
        cell("n left", (x) => (x.ctime ? ago(x.ctime) : ""))(td, f);
        const t = exact(f.ctime);
        if (td.title !== t) td.title = t;
      } },
    /* A dash where nothing is recorded, checked against the mounts: one
     * volume can be `relatime` and another `noatime`. */
    { id: "atime", msgid: "Accessed",  key: "accessed", w: 120, min: 100, near: 0, far: 0, max: 0,
      write: (td, f) => {
        const dead = frozenAtime(f);
        cell("n left", (x) => (dead ? "—" : x.atime ? ago(x.atime) : ""))(td, f);
        // One literal, not two joined: the msgid is the catalogue's key.
        const t = dead
          ? T("this volume does not record reads (noatime) — the number shown would be left over from when the file was created")
          : exact(f.atime);
        if (td.title !== t) td.title = t;
        td.style.color = dead ? "var(--ink-3)" : "";
      } },
    { id: "perm",  msgid: "Mode",      key: "mode",  w: 108, min: 92, near: 0, far: 0, max: 0,
      write: cell("mono", (f) => f.perm || "") },
    { id: "user",  msgid: "Owner",     key: "uid",   w: 100, min: 72, near: 0, far: 0, max: 0,
      write: cell("mono", (f) => f.user || "") },
    { id: "group", msgid: "Group",     key: "gid",   w: 100, min: 72, near: 0, far: 0, max: 0,
      write: cell("mono", (f) => f.group || "") },
    { id: "disk",  msgid: "On disk",   key: "disk",  w: 90, min: 68, near: 0, far: 0, max: 0, num: true,
      write: cell("n", (f) => (f.disk > 0 ? bytes(f.disk) : "")) },
  ];
  const COL_DEFAULT = ["name", "path", "size", "mtime", "kind"];

  /* The service holds these; defaults until its reply. See `applySettings`. */
  let SHOWN = COL_DEFAULT.slice();
  const shownColumns = () => SHOWN.map((id) => COLUMNS.find((c) => c.id === id)).filter(Boolean);

  /* Keyed by column id, never by the sort key: they differ for five columns. */
  let WIDTHS = {};

  /* Under `table-layout: fixed` an unspecified column is the first thing a
   * browser takes space from, and at 900px that left the name at zero. The
   * moves are `scour_ui::lay_out`'s, integer at every step. */
  /* `scour_ui::NARROW` and `WIDE`; the test below fails if they drift. */
  const NARROW = 700, WIDE = 1900;

  /* A column's share of the stretching room, in parts per thousand, integer
     the whole way as Rust is. */
  function shareAt(c, avail) {
    const t = avail <= NARROW ? 0
      : avail >= WIDE ? 1000
      : Math.floor(((avail - NARROW) * 1000) / (WIDE - NARROW));
    return Math.floor((c.near * 10 * (1000 - t) + c.far * 10 * t) / 1000);
  }

  /* A dragged column is exactly as wide as it was dragged; the stretching
     columns nobody dragged share what is left. `scour_ui::lay_out`. */
  function layOut(cols, room) {
    const chosen = cols.map((c) => !!WIDTHS[c.id]);
    const w = cols.map((c, i) =>
      Math.max(c.min, (chosen[i] ? WIDTHS[c.id] : 0) || c.w || 120));
    const free = [];
    cols.forEach((c, i) => { if (c.near > 0 && !chosen[i]) free.push(i); });
    let taken = 0;
    w.forEach((x, i) => { if (!free.includes(i)) taken += x; });
    const budget = Math.max(0, room - taken);
    const weight = free.reduce((a, i) => a + shareAt(cols[i], room), 0);
    let handed = 0;
    free.forEach((i, n) => {
      /* The last takes what the others left, however they rounded. */
      const want = n + 1 === free.length ? Math.max(0, budget - handed)
        : !weight ? 0
        : Math.floor((budget * shareAt(cols[i], room)) / weight);
      handed += want;
      w[i] = Math.max(want, cols[i].min);
      if (cols[i].max) w[i] = Math.max(Math.min(w[i], cols[i].max), cols[i].min);
    });

    const sum = w.reduce((a, b) => a + b, 0);
    if (sum < room) shareOut(cols, chosen, w, room - sum);
    else if (sum > room) {
      /* In turn: the undragged, then the dragged, then everything. */
      const dragged = [];
      cols.forEach((c, i) => { if (c.near > 0 && chosen[i]) dragged.push(i); });
      let over = sum - room;
      for (const from of [free, dragged, cols.map((_, i) => i)]) {
        over = takeBack(cols, w, over, from);
      }
      squash(w, room);
    }
    return w;
  }

  /* Move 4, when even the floors do not fit: every column gives way at once,
     which leaves each one visible and its edge reachable. */
  function squash(w, room) {
    const sum = w.reduce((a, b) => a + b, 0);
    if (sum <= room || !sum || room < w.length) return;
    /* The widest takes the rounding: a pixel matters least to it. */
    let widest = 0;
    for (let i = 1; i < w.length; i++) if (w[i] > w[widest]) widest = i;
    let spent = 0;
    for (let i = 0; i < w.length; i++) {
      if (i === widest) continue;
      w[i] = Math.max(1, Math.floor((w[i] * room) / sum));
      spent += w[i];
    }
    w[widest] = Math.max(1, room - spent);
  }

  /* Move 2: hand out the spare, a column at its ceiling dropping out. */
  function shareOut(cols, chosen, w, spare) {
    let left = spare;
    for (;;) {
      const open = [];
      for (let i = 0; i < cols.length; i++) {
        if (!chosen[i] && cols[i].near > 0 && (!cols[i].max || w[i] < cols[i].max)) open.push(i);
      }
      const weight = open.reduce((a, i) => a + Math.max(1, cols[i].far), 0);
      if (!left) return;
      /* Every stretching column dragged or at its ceiling: the last of them
         takes the rest, or empty row shows past the last column. */
      if (!weight) {
        for (let i = cols.length - 1; i >= 0; i--) {
          if (cols[i].near > 0) { w[i] += left; break; }
        }
        return;
      }
      let spent = 0;
      open.forEach((i, n) => {
        /* The last takes the rounding, or a hairline moves with the window. */
        const share = n + 1 === open.length
          ? left - spent
          : Math.floor((left * Math.max(1, cols[i].far)) / weight);
        const before = w[i];
        const want = before + share;
        w[i] = cols[i].max ? Math.min(want, cols[i].max) : want;
        spent += w[i] - before;
      });
      if (!spent) return;
      left -= spent;
    }
  }

  /* Take `over` back from the columns in `from`, in proportion to the room
     each has above its floor; what could not be taken is returned. */
  function takeBack(cols, w, over, from) {
    let left = over;
    for (;;) {
      const open = from.filter((i) => w[i] > cols[i].min);
      const room = open.reduce((a, i) => a + (w[i] - cols[i].min), 0);
      if (!left || !room) return left;
      const take = Math.min(left, room);
      let taken = 0;
      open.forEach((i, n) => {
        const want = n + 1 === open.length
          ? take - taken
          : Math.floor((take * (w[i] - cols[i].min)) / room);
        const share = Math.min(want, w[i] - cols[i].min);
        w[i] -= share;
        taken += share;
      });
      if (!taken) return left;
      left -= taken;
    }
  }

  function applyWidths() {
    const table = document.querySelector(".results table");
    /* A grid has no columns to be wide: a declared `min-width` is a sideways
       scrollbar over tiles that fit. `WIDTHS` is untouched. */
    if (grid()) {
      for (const th of document.querySelectorAll("thead th[data-col]")) {
        th.style.width = "";
      }
      if (table) table.style.minWidth = "";
      return;
    }
    const heads = [...document.querySelectorAll("thead th[data-col]")];
    if (!heads.length) return;
    /* The mark column and the picker come out of the room first. */
    const FURNITURE = 30 + 30;
    /* `clientWidth`: `offsetWidth` still counts the scrollbar's lane. */
    const room = Math.max(0, (scrollEl ? scrollEl.clientWidth : 0) - FURNITURE);
    const cols = heads.map((th) => COLUMNS.find((c) => c.id === th.dataset.col));
    if (cols.some((c) => !c)) return;
    const w = layOut(cols, room);
    let sum = FURNITURE;
    heads.forEach((th, i) => {
      th.style.width = w[i] + "px";
      sum += w[i];
    });
    /* Only when the floors do not fit does this exceed the room. */
    if (table) table.style.minWidth = sum + "px";
  }


  /* The room changes for more than a resize, so the observer watches the box
     the columns divide up — guarded, since `applyWidths` writes inside it. */
  let lastRoom = -1;
  if (scrollEl && typeof ResizeObserver === "function") {
    new ResizeObserver(() => {
      const room = scrollEl.clientWidth;
      if (room === lastRoom) return;
      lastRoom = room;
      applyWidths();
    }).observe(scrollEl);
  }

  /* What changed and nothing else, so an unknown field survives a save
   * (`scour_settings::Change`). Debounced: one gesture is one write. */
  let pending = {};
  let keepTimer = null;
  function keep(change) {
    Object.assign(pending, change || {});
    clearTimeout(keepTimer);
    keepTimer = setTimeout(() => {
      const change = pending;
      pending = {};
      if (Object.keys(change).length) SERVICE.saveSettings(change);
    }, 250);
  }

  /* What no other frontend can act on. Merged key by key under `web`. */
  const keepView = (patch) => keep({ view: { web: patch } });

  /* The window's outer size, which is what a browser's --window-size sets, so
     the launcher opens the next window the way this one was left. */
  let sizeTimer = 0;
  addEventListener("resize", () => {
    clearTimeout(sizeTimer);
    sizeTimer = setTimeout(() => keepView({ size: [outerWidth, outerHeight] }), 600);
  });

  /* The meter is one line. What a width cannot hold is shed least useful first:
     the index facts, the rows read, the advisory said in two words, then the
     advisory itself. The count and the time never go. */
  const METER_STEPS = ["note", "rows", "stale-short", "stale"];
  /* Takes steps until `fits(taken)` says so; pure, so it can be tested. */
  function meterPlan(steps, fits) {
    const taken = [];
    for (const step of steps) {
      if (fits(taken)) break;
      taken.push(step);
    }
    return taken;
  }
  function fitMeter() {
    const meterEl = document.querySelector(".meter");
    if (!meterEl) return;
    const set = (taken) => {
      meterEl.dataset.drop = taken.filter((t) => t !== "stale-short").join(" ");
      meterEl.dataset.stale = taken.includes("stale-short") ? "short" : "";
    };
    const taken = meterPlan(METER_STEPS, (so_far) => {
      set(so_far);
      return meterEl.scrollWidth <= meterEl.clientWidth;
    });
    set(taken);
  }
  {
    const meterEl = document.querySelector(".meter");
    if (meterEl) {
      /* Text and visibility come from many places; the fit follows them all.
         Its own marks are data attributes, which the filter leaves out. */
      new MutationObserver(() => fitMeter()).observe(meterEl, {
        subtree: true, childList: true, characterData: true,
        attributes: true, attributeFilter: ["hidden"],
      });
      addEventListener("resize", fitMeter);
      fitMeter();
    }
  }

  /* Applied to a window that has already drawn itself with defaults. */
  /* Lines between rows and columns, when the settings ask for them. */
  /* The rail and the ribbon as the settings hold them. */
  function applyPanes() {
    const body = document.querySelector(".body");
    const railOff = !(KEPT && KEPT.rail_shown);
    const ribbonOff = !(KEPT && KEPT.ribbon_shown);
    if (body) {
      body.classList.toggle("no-rail", railOff);
      body.classList.toggle("no-ribbon", ribbonOff);
    }
    // The rail comes back from its own edge; only the ribbon needs the line.
    document.getElementById("rail-back").hidden = true;
    // No line left to bring it back: Settings → List does, and says so.
    document.getElementById("ribbon-back").hidden = true;
    document.getElementById("restore").hidden = true;
  }
  function setPanes(change) {
    KEPT = { ...(KEPT || {}), ...change };
    applyPanes();
    keep(change);
  }
  document.getElementById("rail-x").addEventListener("click", () => setPanes({ rail_shown: false }));
  document.getElementById("ribbon-x").addEventListener("click", () => setPanes({ ribbon_shown: false }));
  document.getElementById("rail-back").addEventListener("click", () => setPanes({ rail_shown: true }));
  document.getElementById("rail-edge").addEventListener("click", () => setPanes({ rail_shown: true }));
  document.getElementById("ribbon-back").addEventListener("click", () => setPanes({ ribbon_shown: true }));

  function applyGrid() {
    document.body.classList.toggle("lines", !!(KEPT && KEPT.grid_lines));
    document.body.classList.toggle("vlines", !!(KEPT && KEPT.grid_columns));
  }

  function applySettings(s) {
    KEPT = s;
    applyGrid();
    applyPanes();
    drawTypos();
    applyTheme(s.theme || "");
    const oldSort = sortKey;
    const oldDescending = sortDesc;
    if (Array.isArray(s.columns) && s.columns.length) {
      const ok = s.columns.filter((id) => COLUMNS.some((c) => c.id === id));
      if (ok.length) SHOWN = ok;
    }
    if (s.widths && typeof s.widths === "object") WIDTHS = { ...s.widths };
    /* Against the column keys, the protocol's own words, so another face's
       settings file is accepted. */
    if (s.sort && COLUMNS.some((c) => c.key === s.sort)) sortKey = s.sort;
    if (typeof s.descending === "boolean") sortDesc = s.descending;
    HISTORY = Array.isArray(s.history) ? s.history : [];
    DUPES_OPEN = !!s.dupes_open;
    /* A shared field: pinning the panel open is not about this face. */
    if (s.preview) setPeek(true, false);
    peekToggle.setAttribute("aria-pressed", s.preview ? "true" : "false");
    VIEW = (s.view && s.view.web) || {};
    applyView();
    showPastAffordance();
    buildHeads();
    applyWidths();
    applyPeekWidth();
    /* From the top of the settings, not `view.web`, and after the headings,
       whose widths it decides. An empty string means nobody has chosen. */
    if (typeof s.layout === "string" && MODES.includes(s.layout)) applyMode(s.layout);
    /* The list started with the defaults, so only a new ordering re-searches —
       or a tab that comes back with a query in it. */
    const tabQuery = readTabs(s);
    if (tabQuery || sortKey !== oldSort || sortDesc !== oldDescending) render();
  }

  /* Held, because both closures want it and only one owns the transport. */
  let VIEW = {};
  /* The settings as the service last gave them, for the settings page. */
  let KEPT = null;
  /* Shared: a duplicate hunt on opening the report means the same everywhere. */
  let DUPES_OPEN = false;

  /* Past queries, newest first, added to only when a search was meant. */
  let HISTORY = [];

  function saveWidths() {
    keep({ widths: WIDTHS });
  }

  /* The preview's width, dragged at its left edge and kept beside the columns'
     under `peek` — the key every face reads, so a width set in the window
     holds here too. Never so wide that the list has no room. */
  /* `scour_ui::preview`'s bounds; a test holds the two numbers to them. */
  const PEEK_MIN = 200;
  const PEEK_MAX = 600;
  const peekRoom = () => Math.max(PEEK_MIN, Math.min(PEEK_MAX, innerWidth - 520));
  function applyPeekWidth() {
    const el = document.getElementById("view-search");
    if (!el) return;
    if (WIDTHS.peek) el.style.setProperty("--peek-w", Math.min(WIDTHS.peek, peekRoom()) + "px");
    else el.style.removeProperty("--peek-w");
  }
  {
    const grip = document.getElementById("peek-grip");
    grip.addEventListener("pointerdown", (e) => {
      e.preventDefault();
      grip.setPointerCapture(e.pointerId);
      grip.classList.add("dragging");
      const right = document.getElementById("view-search").getBoundingClientRect().right;
      const move = (ev) => {
        WIDTHS.peek = Math.round(Math.max(PEEK_MIN, Math.min(right - ev.clientX, peekRoom())));
        applyPeekWidth();
      };
      const up = () => {
        grip.removeEventListener("pointermove", move);
        grip.removeEventListener("pointerup", up);
        grip.classList.remove("dragging");
        saveWidths();
      };
      grip.addEventListener("pointermove", move);
      grip.addEventListener("pointerup", up);
    });
    grip.addEventListener("dblclick", () => {
      delete WIDTHS.peek;
      applyPeekWidth();
      saveWidths();
    });
  }

  /* The heading row. The sort arrow and the grip are added by their owners. */
  function buildHeads() {
    const row = document.getElementById("heads");
    const cols = shownColumns();
    /* Kept, not rebuilt: an `innerHTML` over the glyph switch loses its
       listener. */
    const mark = document.getElementById("markhead");
    mark.remove();
    row.innerHTML =
      cols
        .map(
          (c) =>
            `<th data-col="${c.id}"${c.key ? ` data-sort="${c.key}"` : ""}` +
            `${c.num ? ' class="num"' : ""}` +
            `${c.key ? "" : ` title="${escape_(T("this column cannot be sorted by"))}"`}>` +
            `${escape_(T(c.msgid))}</th>`
        )
        .join("") +
      `<th class="pick"><button type="button" id="pick" title="${escape_(T("columns"))}"
         aria-label="${escape_(T("choose the columns"))}">⋮</button></th>`;
    // Before the numbering below, which counts across the row.
    row.prepend(mark);
    /* Numbered downwards: each sticky heading is its own stacking context and
     * the grip hangs four pixels into the next column. */
    [...row.querySelectorAll("th")].forEach((th, i) => {
      th.style.zIndex = String(40 - i);
    });
    fitGrips();
    applyWidths();
    syncHeads();
    /* A reorder keeps the cell count, so the shape check will not rebuild. */
    for (const tr of pool) tr.__stamp = "";
    painted = "";
    repaint(true);
  }

  function columnMenu() {
    const menu = document.createElement("div");
    menu.className = "rowmenu cols";
    menu.innerHTML = COLUMNS.map((c) => {
      const on = SHOWN.includes(c.id);
      const locked = on && SHOWN.length === 1;
      return `<button type="button" data-col="${c.id}"${locked ? " disabled" : ""}>
        <span class="tick">${on ? "✓" : ""}</span>${escape_(T(c.msgid))}</button>`;
    }).join("")
      + `<button type="button" data-col="" class="reset">${escape_(T("Back to the default"))}</button>`;
    return menu;
  }

  function fitGrips() {
    const heads = [...document.querySelectorAll("thead th[data-sort]")];
    heads.forEach((th, i) => {
      const last = i === heads.length - 1;
      const has = th.querySelector(".grip");
      if (last) { if (has) has.remove(); return; }
      if (has) return;
      const g = document.createElement("span");
      g.className = "grip";
      g.title = T("drag · double-click to reset");
      th.appendChild(g);
    });
  }

  document.querySelector("thead").addEventListener("pointerdown", (e) => {
    const grip = e.target.closest(".grip");
    if (!grip) return;
    e.preventDefault();
    e.stopPropagation();          // not a click on the heading, so no sort
    const th = grip.closest("th");
    const startX = e.clientX;
    const startW = th.getBoundingClientRect().width;
    grip.classList.add("dragging");
    document.body.classList.add("resizing");
    grip.setPointerCapture(e.pointerId);

    const move = (ev) => {
      /* This column's own floor: a flat 56px is under what a date needs. */
      const floor = (COLUMNS.find((c) => c.id === th.dataset.col) || {}).min || 56;
      const w = Math.max(floor, Math.round(startW + ev.clientX - startX));
      WIDTHS[th.dataset.col] = w;
      // Through `applyWidths`, so the table's minimum grows with the drag.
      applyWidths();
    };
    const up = () => {
      grip.classList.remove("dragging");
      document.body.classList.remove("resizing");
      grip.removeEventListener("pointermove", move);
      grip.removeEventListener("pointerup", up);
      /* Dragged past where the column stops — the others on their floors —
         what is kept is what is on screen, not where the pointer went:
         kept, the overshoot came back as a width nobody saw. */
      const col = th.dataset.col;
      const shown = Math.round(th.getBoundingClientRect().width);
      if (WIDTHS[col] && shown < WIDTHS[col]) WIDTHS[col] = shown;
      saveWidths();
      painted = "";
      repaint(true);
    };
    grip.addEventListener("pointermove", move);
    grip.addEventListener("pointerup", up);
  });

  document.querySelector("thead").addEventListener("dblclick", (e) => {
    const grip = e.target.closest(".grip");
    if (!grip) return;
    e.preventDefault();
    e.stopPropagation();
    delete WIDTHS[grip.closest("th").dataset.col];
    applyWidths();
    saveWidths();
    painted = "";
    repaint(true);
  });

  function syncHeads() {
    document.querySelectorAll("thead th[data-sort]").forEach((th) => {
      const on = th.dataset.sort === sortKey;
      th.setAttribute("aria-sort", on ? (sortDesc ? "descending" : "ascending") : "none");
      // The grip is not part of the label and must survive being relabelled.
      const grip = th.querySelector(".grip");
      const label = (th.dataset.label ||= th.textContent.replace(/[▾▴]\s*$/, "").trim());
      th.innerHTML = label + (on ? ` <span class="arrow">${sortDesc ? "▾" : "▴"}</span>` : "");
      if (grip) th.appendChild(grip);
    });
  }

  // Clicking a filter adds the term to the *text*, not to hidden state.
  function toggleTerm(term) {
    const parts = q.value.trim().split(/\s+/).filter(Boolean);
    /* A question about the field, not its spelling: `tur:image` and
     * `kind:image` left as two terms match no file at all. */
    const mine = term.slice(0, Math.max(0, term.indexOf(":")));
    const want = field(mine);
    const at = parts.findIndex((t) => {
      const i = t.indexOf(":");
      if (i < 0) return false;
      const his = t.slice(0, i);
      const f = want && field(his);
      return f ? f === want : his === mine;
    });
    if (at >= 0 && parts[at] === term) parts.splice(at, 1);
    else if (at >= 0) parts[at] = term;
    else parts.push(term);
    q.value = parts.join(" ");
    render();
    q.focus();
  }

  document.querySelector(".rail").addEventListener("click", (e) => {
    const b = e.target.closest(".facet");
    if (b) toggleTerm(b.dataset.term);
  });
  bars.addEventListener("click", (e) => {
    const b = e.target.closest("button");
    if (b) toggleTerm("dm:" + b.dataset.days + "d");
  });
  document.querySelector("thead").addEventListener("click", (e) => {
    const th = e.target.closest("th[data-sort]");
    if (!th) return;
    // A heading just carried is not one clicked, or a reorder also sorts.
    if (justMoved) return;
    applySortChoice(th.dataset.sort);
  });

  /* One pointer, three things, told apart by where and how far: the last nine
   * pixels resize, a press under `DRAG_SLOP` sorts, anything else carries. */
  const DRAG_SLOP = 5;
  let moving = null;      /* the heading being carried */
  let movingFrom = 0;     /* where the press started, in page pixels */
  let dropAt = -1;        /* the gap it would fall into, as an index */
  let justMoved = false;  /* so the click that follows a carry does not sort */

  const dropLine = document.createElement("div");
  dropLine.className = "dropline";
  dropLine.hidden = true;
  document.querySelector(".results .scroll").appendChild(dropLine);

  const headCells = () =>
    [...document.querySelectorAll("#view-search thead th[data-col]")];

  function showDrop(x) {
    const cells = headCells();
    let at = cells.length;
    for (let i = 0; i < cells.length; i++) {
      const r = cells[i].getBoundingClientRect();
      if (x < r.left + r.width / 2) { at = i; break; }
    }
    dropAt = at;
    /* Against the scroller it is drawn inside, for a sideways-scrolled list. */
    const host = document.querySelector(".results .scroll").getBoundingClientRect();
    const edge = at < cells.length
      ? cells[at].getBoundingClientRect().left
      : cells[cells.length - 1].getBoundingClientRect().right;
    dropLine.style.left = Math.round(edge - host.left +
      document.querySelector(".results .scroll").scrollLeft) + "px";
    dropLine.hidden = false;
  }

  document.querySelector("thead").addEventListener("pointerdown", (e) => {
    if (e.target.closest(".grip")) return;        /* that press is a resize */
    const th = e.target.closest("th[data-col]");
    if (!th) return;
    moving = th;
    movingFrom = e.clientX;
    dropAt = -1;
    justMoved = false;
    th.setPointerCapture(e.pointerId);
  });

  document.querySelector("thead").addEventListener("pointermove", (e) => {
    if (!moving) return;
    if (!justMoved && Math.abs(e.clientX - movingFrom) < DRAG_SLOP) return;
    if (!justMoved) {
      justMoved = true;
      moving.classList.add("moving");
      document.body.classList.add("moving-col");
    }
    showDrop(e.clientX);
  });

  const endMove = () => {
    if (!moving) return;
    const th = moving;
    moving = null;
    dropLine.hidden = true;
    th.classList.remove("moving");
    document.body.classList.remove("moving-col");
    if (justMoved && dropAt >= 0) {
      const from = SHOWN.indexOf(th.dataset.col);
      // Taking it out shifts everything after it one place left.
      let to = dropAt > from ? dropAt - 1 : dropAt;
      if (from >= 0 && to !== from) {
        const [id] = SHOWN.splice(from, 1);
        SHOWN.splice(to, 0, id);
        keep({ columns: SHOWN });
        buildHeads();
      }
    }
    /* Cleared after the click this press produces, not before it. */
    setTimeout(() => { justMoved = false; }, 0);
  };
  document.querySelector("thead").addEventListener("pointerup", endMove);
  document.querySelector("thead").addEventListener("pointercancel", endMove);
  /* The box never edits what is being typed: a caret that moves reads as a
   * fault, and the engine reads both spellings alike. */
  q.addEventListener("beforeinput", (e) => {
    if (e.inputType !== "insertText" || !e.data || /\s$/.test(e.data)) return;
    if (q.selectionStart !== q.selectionEnd) return;
    const c = q.selectionStart ?? 0, v = q.value;
    const after = placeCapType(e.data);
    if (after) {
      e.preventDefault();
      q.value = after[0];
      q.setSelectionRange(after[1], after[1]);
      lastQueryCaret = after[1];
      render();
      return;
    }
    for (const [ta, tb] of terms(v)) {
      if (c === ta && capHeadOf(v, ta, tb) > ta) {
        e.preventDefault();
        q.value = v.slice(0, c) + e.data + " " + v.slice(c);
        const at = c + e.data.length;
        q.setSelectionRange(at, at);
        lastQueryCaret = at;
        render();
        return;
      }
    }
  });
  q.addEventListener("input", render);
  // The offer depends on where the caret is, not on what was last typed.
  let lastClick = null;
  q.addEventListener("mousedown", (e) => { queryPressing = true; lastClick = { x: e.clientX, y: e.clientY }; });
  document.addEventListener("mouseup", () => { queryPressing = false; });
  q.addEventListener("click", () => {
    queryPressing = true;
    clampQuerySelection();
    queryPressing = false;
    /* A press anywhere else leaves an empty filter, which goes. */
    if (dropEmpty(q.selectionStart)) return;
    paint(q.value);
    showHints();
    /* A press past the end of the text leaves the term there. */
    const lastRect = shadow.lastElementChild && [...shadow.lastElementChild.getClientRects()].pop();
    if (q.selectionStart === q.value.length && lastRect && lastClick &&
        (lastClick.x > lastRect.right + 4 || lastClick.y > lastRect.bottom + 2) && lensExit()) return;
    /* A filter opens its lens when it is clicked; a word only when asked. */
    const t = lensTerm(q.value, q.selectionStart);
    lensSync(!!(t && t.named && lensKindOf(t)));
  });
  q.addEventListener("dblclick", () => {
    const v = q.value, a = q.selectionStart ?? 0;
    for (const [ta, tb] of terms(v)) {
      if (a < ta || a > tb) continue;
      const head = capHeadOf(v, ta, tb);
      if (head > ta && a >= head) {
        const s = head < tb && v[head] === '"' ? head + 1 : head;
        const e = tb > s && v[tb - 1] === '"' ? tb - 1 : tb;
        q.setSelectionRange(s, e);
        lastQueryCaret = e;
        paint(v);
      }
      return;
    }
  });
  q.addEventListener("focus", () => { clampQuerySelection(); paint(q.value); });
  q.addEventListener("blur", () => paint(q.value));
  document.addEventListener("selectionchange", () => {
    if (document.activeElement === q) {
      clampQuerySelection();
      paint(q.value);
    }
  });
  q.addEventListener("keyup", (e) => {
    if (e.key.startsWith("Arrow") && hintBox.hidden && !LENS) showHints();
    if (/^(Arrow(Left|Right)|Home|End)$/.test(e.key)) lensSync(false);
  });
  /* The textarea is the one that is pressed, and its own idea of where a
     letter is has nothing to do with what is drawn: a press is placed from the
     drawing, and a drag goes on from there. */
  let qDrag = null;
  const setRange = (anchor, off) => {
    q.setSelectionRange(Math.min(anchor, off), Math.max(anchor, off), off < anchor ? "backward" : "forward");
  };
  q.addEventListener("mousedown", (e) => {
    if (e.button !== 0) return;
    e.preventDefault();
    q.focus({ preventScroll: true });
    const v = q.value, off = offsetAtPoint(e.clientX, e.clientY);
    if (e.detail >= 3) { q.setSelectionRange(0, v.length); return; }
    if (e.detail === 2) {
      let lo = off, hi = off;
      while (lo > 0 && !/\s/.test(v[lo - 1])) lo--;
      while (hi < v.length && !/\s/.test(v[hi])) hi++;
      q.setSelectionRange(lo, hi);
      return;
    }
    const anchor = e.shiftKey ? (q.selectionDirection === "backward" ? q.selectionEnd : q.selectionStart) : off;
    qDrag = { anchor };
    setRange(anchor, off);
    // A press into a folder part edits it; one on a disk rests.
    if (!e.shiftKey && pressEdits) editingAt(off);
    else if (!e.shiftKey && pressEdits === false) qEdit = null;
  });
  document.addEventListener("mousemove", (e) => {
    if (!qDrag) return;
    if (!(e.buttons & 1)) { qDrag = null; return; }
    setRange(qDrag.anchor, offsetAtPoint(e.clientX, e.clientY));
  });
  document.addEventListener("mouseup", () => { qDrag = null; });
  q.addEventListener("wheel", (e) => {
    if (shadow.scrollHeight <= shadow.clientHeight) return;
    e.preventDefault();
    shadow.scrollTop += e.deltaY;
    drawDecor();
  }, { passive: false });
  addEventListener("resize", () => paint(q.value));
  document.fonts.ready.then(() => paint(q.value));

  /* A browser cannot open a local file, so the bridge asks the desktop. Both
   * fences are the bridge's — the path must be one the index holds, and
   * anything runnable is shown in its folder instead. `ask` is the menu's
   * "Open": the one way that offers to run a program. */
  async function launch(path, what, yes, ask) {
    // Opening a result belongs in the history even without an Enter.
    remember(q.value);
    try {
      const q = new URLSearchParams({ path, what: what || "file", t: TOKEN });
      if (ask) q.set("ask", "1");
      if (yes) q.set("run", "yes");
      const r = await fetch(`/api/open?${q}`, { method: "POST" });
      const j = await r.json();
      if (j.error) { flash(j.error); return; }
      /* A program: asked, as a file manager asks, before it is started. */
      if (j.ask_run) {
        if (confirm(T("Run {name}?", { name: j.ask_run }) + "\n\n" +
                    T("This file is a program. Opening it starts it."))) {
          launch(path, what, true, true);
        }
        return;
      }
      if (j.ran) flash(T("Started {name}", { name: j.ran }));
      else if (j.folder_instead) flash(T("It runs when opened — its folder was opened instead"));
    } catch (e) { flash(T("could not open: {why}", { why: e.message || e })); }
  }

  let flashTimer = null;
  function flash(text) {
    const el = document.getElementById("rows");
    if (!el) return;
    el.dataset.was = el.dataset.was || el.textContent;
    el.textContent = text;
    clearTimeout(flashTimer);
    flashTimer = setTimeout(() => {
      el.textContent = el.dataset.was || "";
      delete el.dataset.was;
    }, 4000);
  }


