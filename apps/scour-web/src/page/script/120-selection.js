  /* Choosing columns. The list is stored beside the widths, by the service. */
  let colMenu = null;
  document.addEventListener("click", (e) => {
    const opener = e.target.closest("#pick");
    if (colMenu && !e.target.closest(".cols")) {
      colMenu.remove();
      colMenu = null;
      if (!opener) return;
    }
    if (!opener) return;
    colMenu = columnMenu();
    document.body.appendChild(colMenu);
    const r = opener.getBoundingClientRect();
    const m = colMenu.getBoundingClientRect();
    colMenu.style.left = Math.max(8, Math.min(r.left, innerWidth - m.width - 8)) + "px";
    colMenu.style.top = Math.min(r.bottom + 4, innerHeight - m.height - 8) + "px";
    colMenu.addEventListener("click", (ev) => {
      const b = ev.target.closest("[data-col]");
      if (!b || b.disabled) return;
      const id = b.dataset.col;
      if (id === "") {
        SHOWN = COL_DEFAULT.slice();
      } else if (SHOWN.includes(id)) {
        SHOWN = SHOWN.filter((x) => x !== id);
      } else {
        /* Into the table's own order, without rearranging what is shown. */
        const rank = (x) => COLUMNS.findIndex((c) => c.id === x);
        const at = SHOWN.findIndex((x) => rank(x) > rank(id));
        if (at < 0) SHOWN = [...SHOWN, id];
        else SHOWN = [...SHOWN.slice(0, at), id, ...SHOWN.slice(at)];
      }
      keep({ columns: SHOWN });
      buildHeads();
      colMenu.remove();
      colMenu = null;
    });
  });

  const pathOf = (tr) => tr && tr.dataset && tr.dataset.path;

  /* Keyed by path, not position: the list is sorted by time, so positions
   * move. `Ctrl+A` takes the windows that have arrived, and no more. */
  const selbar = document.getElementById("selbar");
  /* path -> the row: the bar adds up sizes, the menu asks what a row is. */
  const SEL = new Map();
  const SEL_AT = new Map();
  /* Where a shift-range measures from: tracked by path so live arrivals do not drift it. */
  let selAnchor = null, selAnchorPath = null;

  function indexOfPath(p) {
    if (!p) return -1;
    for (const [i, f] of LIST.rows) {
      if (f && fullPath(f) === p) return i;
    }
    return -1;
  }

  function anchorIndex() {
    if (selAnchorPath) {
      const at = indexOfPath(selAnchorPath);
      if (at >= 0) return at;
    }
    return selAnchor;
  }

  function clearSel() {
    focusPath = null;
    wantFocusPath = null;
    if (!SEL.size) return;
    SEL.clear();
    SEL_AT.clear();
    selAnchor = null;
    selAnchorPath = null;
    paintSelection();
    drawSelBar();
  }

  function pickRow(tr, keep) {
    const p = pathOf(tr);
    if (!p) return;
    if (!keep) { SEL.clear(); SEL_AT.clear(); }
    if (keep && SEL.has(p)) {
      SEL.delete(p);
      SEL_AT.delete(p);
    } else {
      SEL.set(p, tr.__row);
      const i = Number(tr.dataset.i);
      if (Number.isFinite(i)) SEL_AT.set(p, i);
    }
  }

  /* Every row between two positions; unfetched rows cannot be selected, so
   * the bar counts what was taken and not the range. */
  function selectRange(a, b) {
    const [lo, hi] = a <= b ? [a, b] : [b, a];
    for (let i = lo; i <= hi; i++) {
      const f = LIST.rows.get(i);
      if (f) {
        const p = f.path + "/" + f.name;
        SEL.set(p, f);
        SEL_AT.set(p, i);
      }
    }
  }

  /* After every paint, never inside `writeRow`: it returns early when only
   * the selection changed, and assigns `className` outright. */
  function paintSelection() {
    for (const tr of body.querySelectorAll("tr[data-path]")) {
      tr.classList.toggle("sel", SEL.has(tr.dataset.path));
    }
  }

  function drawSelBar() {
    peekFollowSelection();
    const n = SEL.size;
    selbar.hidden = n === 0;
    if (!n) return;
    let total = 0;
    let dirs = 0;
    for (const f of SEL.values()) {
      if (!f) continue;
      if (f.is_dir) dirs++;
      else total += f.size > 0 ? f.size : 0;
    }
    /* Folders are counted, not weighed. The count sits inside the msgid with
       its markup, because a language need not put the number first. */
    /* One row: its path, which says more than "1 selected". */
    const only = n === 1 ? [...SEL.keys()][0] : null;
    const parts = only
      ? [`<span class="one">${escape_(only)}</span>`]
      : [T('<span class="n">{n}</span> selected', { n: fmt(n) })];
    if (n - dirs > 0) parts.push(`<span class="sum">${bytes(total)}</span>`);
    if (dirs) parts.push(T('<span class="sum">{n} folders</span>', { n: fmt(dirs) }));
    const nFolders = foldersOf().length;
    const tooMany = nFolders > OPEN_MOST;
    const tip = only || [...SEL.keys()][0] || "";
    /* The way out first and apart, frameless; the folders' button off past
       twenty, asked about past three. Numbers from `scour_ui`. */
    selbar.innerHTML =
      `<button type="button" data-sel="clear" class="out">× ${escape_(T("Clear the selection"))}</button>` +
      `<span class="rule"></span>` +
      `<button type="button" data-sel="focus" class="jump" title="${escape_(tip)}">` +
      parts.join(' <span class="sum">·</span> ') +
      `</button>` +
      `<button type="button" data-sel="copy">${escape_(T("Copy the paths"))}</button>` +
      `<button type="button" data-sel="folder"${tooMany ? " disabled" : asking ? ' class="asking"' : ""}>` +
      escape_(tooMany
        ? T("{n} folders: too many to open", { n: fmt(nFolders) })
        : asking
          ? T("Open {n} windows?", { n: fmt(nFolders) })
          : T("Open their folders ({n})", { n: fmt(nFolders) })) +
      `</button>`;
  }

  /* The folders a selection sits in: eleven files there are one window. */
  const foldersOf = () => [...new Set([...SEL.values()].filter(Boolean).map((f) => f.path))];

  /* Asked about past a handful, and always on the bar's own button: a menu
   * closes when clicked and has nowhere to put a question. */
  const OPEN_MANY = 3;    /* scour_ui::OPEN_FOLDERS_ASK */
  const OPEN_MOST = 20;   /* scour_ui::OPEN_FOLDERS_MOST */
  let asking = false;
  let askTimer = null;

  function askFolders() {
    asking = true;
    drawSelBar();
    clearTimeout(askTimer);
    // The question expires, or the button surprises the next press.
    askTimer = setTimeout(() => { asking = false; drawSelBar(); }, 5000);
  }

  function openFolders() {
    const dirs = foldersOf();
    if (dirs.length > OPEN_MOST) return;
    if (dirs.length > OPEN_MANY && !asking) { askFolders(); return; }
    asking = false;
    clearTimeout(askTimer);
    /* The files themselves, each selected in its folder: opening the folders
       alone showed nothing, and did nothing to one already open. */
    SERVICE.reveal([...SEL.keys()]).catch((err) => flash(String(err.message || err)));
    drawSelBar();
  }

  function copySelected() {
    const text = [...SEL.keys()].join("\n");
    navigator.clipboard.writeText(text).then(
      () => flash(T("{n} paths copied", { n: fmt(SEL.size) })),
      () => flash(T("could not copy")));
  }

  function focusSelected() {
    const keys = [...SEL.keys()];
    if (!keys.length) return;
    let target = keys[0];
    if (keys.length > 1 && focusPath && SEL.has(focusPath)) {
      const cur = indexOfPath(focusPath);
      target = cur >= 0 && rowFullyVisibleAt(cur)
        ? keys[(keys.indexOf(focusPath) + 1) % keys.length]
        : focusPath;
    }
    focusPath = target;
    wantFocusPath = target;
    let at = indexOfPath(target);
    if (at < 0 && SEL_AT.has(target)) {
      at = Math.min(Math.max(0, SEL_AT.get(target)), Math.max(0, reach() - 1));
    }
    if (at >= 0 && !rowFullyVisibleAt(at)) {
      measurePerRow();
      const p = pitch(), h = lineH();
      const line = Math.floor(at / PER_ROW);
      const head = grid() ? (headPx || 0) : (headPx || 33);
      const vis = Math.max(h, scrollEl.clientHeight - 30);
      scrollEl.scrollTop = Math.max(0, Math.round(line * p - (vis - head - h) / 2));
    }
    const nowTr = pool.find((tr) => tr.dataset.path === target);
    if (nowTr && (at < 0 || rowFullyVisibleAt(at))) {
      wantFocusPath = null;
      nowTr.focus({ preventScroll: true });
    }
    repaint(true);
  }

  selbar.addEventListener("click", (e) => {
    const btn = e.target.closest("button[data-sel]");
    if (!btn) return;
    if (btn.dataset.sel === "clear") clearSel();
    else if (btn.dataset.sel === "focus") focusSelected();
    else if (btn.dataset.sel === "copy") copySelected();
    else openFolders();
  });

  /* Clicking picks. Plain replaces, `Ctrl` adds one, `Shift` takes the run. */
  /* A cell that cut its text says the whole of it on hover. Only cells that
     set no title of their own: the date and the folder size keep theirs. */
  body.addEventListener("mouseover", (e) => {
    const td = e.target.closest && e.target.closest("td");
    if (!td || (td.title && !td.dataset.tip)) return;
    const el = td.querySelector(".path") || td;
    if (el.scrollWidth > el.clientWidth) { td.title = el.textContent; td.dataset.tip = "1"; }
    else if (td.dataset.tip) { td.removeAttribute("title"); delete td.dataset.tip; }
  });
  body.addEventListener("click", (e) => {
    const tr = e.target.closest("tr[data-path]");
    if (!tr) { return; }
    const i = Number(tr.dataset.i);
    const p = pathOf(tr);
    const anc = anchorIndex();
    if (e.shiftKey && anc !== null && Number.isFinite(i)) {
      SEL.clear();
      selectRange(anc, i);
    } else if (e.ctrlKey || e.metaKey) {
      pickRow(tr, true);
      selAnchor = i;
      selAnchorPath = p;
    } else {
      pickRow(tr, false);
      selAnchor = i;
      selAnchorPath = p;
    }
    if (p) focusPath = p;
    paintSelection();
    drawSelBar();
  });

  body.addEventListener("keydown", (e) => {
    if ((e.ctrlKey || e.metaKey) && (e.key === "a" || e.key === "A")) {
      e.preventDefault();
      SEL.clear();
      SEL_AT.clear();
      for (const [idx, f] of LIST.rows) {
        const p = f.path + "/" + f.name;
        SEL.set(p, f);
        SEL_AT.set(p, idx);
      }
      paintSelection();
      drawSelBar();
      return;
    }
    if (e.key === "Escape" && SEL.size) { e.preventDefault(); clearSel(); }
  });

  /* The column decides: the name is the file, the location its folder. */
  body.addEventListener("dblclick", (e) => {
    const p = pathOf(e.target.closest("tr"));
    if (!p) return;
    const cell = e.target.closest("td");
    const onPath = !!cell && !!cell.querySelector(".path");
    launch(p, onPath ? "folder" : "file");
  });
  body.addEventListener("keydown", (e) => {
    const row = e.target.closest("tr");
    if (!row) return;
    /* A screenful at a time: the rows are virtual, so these move the
       scroller and let the list draw what lands. */
    if (e.key === "PageDown" || e.key === "PageUp" || e.key === "Home" || e.key === "End") {
      e.preventDefault();
      const screenful = scrollEl.clientHeight - ROW_H;
      if (e.key === "Home") scrollEl.scrollTop = 0;
      else if (e.key === "End") scrollEl.scrollTop = scrollEl.scrollHeight;
      else scrollEl.scrollTop += e.key === "PageDown" ? screenful : -screenful;
      return;
    }
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const rows = [...body.querySelectorAll("tr[data-path]")];
      const at = rows.indexOf(row);
      const next = rows[at + (e.key === "ArrowDown" ? 1 : -1)];
      if (next) {
        /* The anchor is where the walk began, so reversing gives rows back. */
        if (e.shiftKey) {
          const i = Number(next.dataset.i);
          if (anchorIndex() === null) {
            selAnchor = Number(row.dataset.i);
            selAnchorPath = pathOf(row);
          }
          const anc = anchorIndex();
          if (Number.isFinite(i) && anc !== null) {
            SEL.clear();
            selectRange(anc, i);
            paintSelection();
            drawSelBar();
          }
        }
        focusPath = pathOf(next) || focusPath;
        next.focus();
        // `nearest` leaves the list still when the row is already on screen.
        next.scrollIntoView({ block: "nearest" });
      } else if (e.key === "ArrowUp") {
        // Off the top is back to the query.
        q.focus();
      }
      return;
    }
    /* What the menu prints beside its two removals. */
    if (e.key === "Delete") {
      const p = pathOf(row);
      if (!p) return;
      e.preventDefault();
      const picked = SEL.size > 1 && SEL.has(p) ? [...SEL.keys()] : [p];
      runAct(e.shiftKey ? "delete" : "trash", p, row.__row || null, picked, null);
      return;
    }
    if (e.key !== "Enter") return;
    const p = pathOf(row);
    if (p) { e.preventDefault(); launch(p, e.shiftKey ? "folder" : "file"); }
  });

  /* Out of the window, by way of the window's drag tray: Chromium will not
     let a page hand out a file — a `file://` in `text/uri-list` arrives as
     `about:blank#`, and a file manager made a text file of the path. The
     browser's own drag is cancelled; the tray opens with the files in it.
     The whole selection goes when the row is in it. */
  body.addEventListener("dragstart", (e) => {
    const tr = e.target.closest && e.target.closest("tr");
    const p = pathOf(tr);
    if (!p) return;
    e.preventDefault();
    const paths = SEL.size > 1 && SEL.has(p) ? [...SEL.keys()] : [p];
    SERVICE.drag(paths)
      .then((r) => { if (r.error) flash(T("Could not open the drag window: {e}", { e: r.error })); })
      .catch((err) => flash(String(err.message || err)));
  });

  /* The right-click menu. Built per press from `MENU`. */
  const menu = document.createElement("div");
  menu.className = "rowmenu";
  menu.hidden = true;
  document.body.appendChild(menu);

  body.addEventListener("contextmenu", (e) => {
    const tr = e.target.closest("tr");
    const p = pathOf(tr);
    if (!p) return;
    /* Right-clicking outside the selection selects what was clicked. */
    if (!SEL.has(p)) {
      pickRow(tr, false);
      selAnchor = Number(tr.dataset.i);
      selAnchorPath = p;
      paintSelection();
      drawSelBar();
    }
    focusPath = p;
    openRowMenu(e, p, tr.__row || null, null);
  });

  /* The one menu, wherever a path is drawn. `after` means the caller owns the
   * path — the report's duplicate rows do — so the search selection is not
   * what the press is about and the caller redraws its own list. */
  function openRowMenu(e, p, row, after) {
    e.preventDefault();
    const alone = !!after;
    const many = !alone && SEL.size > 1;
    const dir = !!(row && row.is_dir);
    /* Built from `MENU`; the count goes in through the catalogue's `{n}`. */
    const shape = (w) => (w === "many" ? many
                        : w === "one" ? !many
                        : w === "file" ? (!many && !dir)
                        : (!many && dir));
    const icon = (name) => name && MENU_ICONS[name]
      ? `<span class="mi" aria-hidden="true">${MENU_ICONS[name]}</span>` : "";
    const items = MENU.filter((it) => shape(it.when));
    /* What it is about: the row, or what the selection comes to. */
    let html = "";
    if (many) {
      const rows = [...SEL.values()].filter(Boolean);
      const dirs = rows.filter((f) => f.is_dir).length;
      const files = rows.length - dirs;
      const weight = rows.filter((f) => !f.is_dir).reduce((a, f) => a + (f.size || 0), 0);
      const said = [];
      if (files) said.push(T("{n} files", { n: fmt(files) }));
      if (dirs) said.push(T("{n} folders", { n: fmt(dirs) }));
      if (files) said.push(bytes(weight));
      html += `<div class="mhead"><span class="mk">${icon("stack")}</span><span class="mt">` +
        `<b>${escape_(T("{n} selected", { n: fmt(SEL.size) }))}</b>` +
        `<small>${escape_(said.join(" · "))}</small></span></div>`;
    } else {
      const name = p.slice(p.lastIndexOf("/") + 1);
      const up = p.slice(0, p.lastIndexOf("/")) || "/";
      const folder = HOME && (up === HOME || up.startsWith(HOME + "/")) ? "~" + up.slice(HOME.length) : up;
      const k = dir ? "folder" : (row && row.ktoken) || "file";
      const where = dir || !row || row.size == null ? folder : folder + " · " + bytes(row.size);
      html += `<div class="mhead"><span class="mk"><span class="ico k-${GLYPHS.has(k) ? k : "file"}"></span></span>` +
        `<span class="mt"><b>${escape_(name)}</b><small>${escape_(where)}</small></span></div>`;
    }
    const label = (msgid) => escape_(T(msgid, { n: fmt(alone ? 1 : SEL.size || 1) }));
    const strip = items.filter((it) => it.short);
    if (strip.length) {
      html += '<div class="mstrip">' + strip.map((it) =>
        `<button type="button" data-act="${it.id}" class="w-${it.weight}" title="${label(it.msgid)}">` +
        `${icon(it.icon)}${escape_(T(it.short))}</button>`).join("") + "</div>";
    }
    html += '<div class="mline"></div>';
    let last = null;
    for (const it of items.filter((it) => !it.short)) {
      if (last !== null && it.group !== last) html += '<div class="mline"></div>';
      last = it.group;
      const key = it.key ? `<span class="mkey">${escape_(it.key)}</span>` : "";
      html += `<button type="button" data-act="${it.id}" class="w-${it.weight}">${icon(it.icon)}${label(it.msgid)}${key}</button>`;
    }
    menu.innerHTML = html;
    menu.classList.add("row");
    menu.dataset.path = p;
    /* The row itself: three items ask what kind or how big it is. */
    menu.__row = row;
    menu.__alone = alone;
    menu.__after = after || null;
    menu.hidden = false;
    const r = menu.getBoundingClientRect();
    menu.style.left = Math.min(e.clientX, innerWidth - r.width - 8) + "px";
    menu.style.top = Math.min(e.clientY, innerHeight - r.height - 8) + "px";
    menu.querySelector("button").focus();
  }

  menu.addEventListener("click", (e) => {
    const b = e.target.closest("button");
    const act = b && b.dataset && b.dataset.act;
    if (!act) return;
    const p = menu.dataset.path;
    const many = !menu.__alone && SEL.size > 1;
    menu.hidden = true;
    /* Whoever drew the row redraws it; the search list refreshes itself. */
    runAct(act, p, menu.__row, many ? [...SEL.keys()] : [p], menu.__after);
  });

  /* What an item does, from the menu or from a key: `picked` is what it is
     about, and `after` redraws a list the caller owns. */
  async function runAct(act, p, row, picked, after) {
    const many = picked.length > 1;

    /* Said once: the copy items differ only in what part of the path. */
    const put = (text, said) =>
      navigator.clipboard.writeText(text).then(() => flash(said),
                                               () => flash(T("could not copy")));
    const leaf = (x) => x.slice(x.lastIndexOf("/") + 1);
    const up = (x) => x.slice(0, x.lastIndexOf("/")) || "/";

    switch (act) {
      case "clear":   return clearSel();
      case "folders": return openFolders();
      case "open":    return launch(p, "file", false, true);
      case "folder":  return launch(p, "folder");

      case "open-all": {
        /* The trailing ellipsis in the label promises this question. */
        if (!confirm(T("Open all {n}…", { n: fmt(picked.length) }))) return;
        for (const x of picked) launch(x, "file");
        return;
      }

      case "copy-path":
        return put(picked.join("\n"), many
          ? T("{n} paths copied", { n: fmt(picked.length) })
          : T("path copied"));
      case "copy-name":
        return put(picked.map(leaf).join("\n"), many
          ? T("{n} names copied", { n: fmt(picked.length) })
          : T("name copied"));

      case "search-here":
        // Home-relative and quoted when it has a space: one term, read back as typed.
        return toggleTerm("under:" + quoteIf(homeForm(row && row.is_dir ? p : up(p))));
      case "search-kind": {
        const dot = leaf(p).lastIndexOf(".");
        if (dot <= 0) return flash(T("no extension to search for"));
        return toggleTerm("ext:" + leaf(p).slice(dot + 1).toLowerCase());
      }
      case "duplicates": {
        /* Every copy of this file is in the set of files its exact size. */
        /* In the list, where they can be seen: the report asked nothing about
           this file. */
        if (row && row.size !== undefined && row.size !== null) toggleTerm("size:=" + row.size);
        return;
      }
      case "usage":
        toggleTerm("under:" + p);
        document.getElementById("tab-report").click();
        return;
      case "skip": {
        rulesBtn.click();
        /* The name, not the path, and left unsent: a menu press must not add
           a rule by itself. */
        const field = rulesPanel.querySelector('input[type="text"]');
        if (field) { field.value = leaf(p); field.focus(); field.select(); }
        return;
      }

      case "drag": {
        try {
          const r = await SERVICE.drag(picked);
          if (r.error) flash(T("Could not open the drag window: {e}", { e: r.error }));
        } catch (err) {
          flash(String(err.message || err));
        }
        return;
      }
      case "csv": {
        const b = document.getElementById("csv");
        if (b) b.click();
        return;
      }
      case "details": {
        const t = document.getElementById("peek-toggle");
        if (t && t.getAttribute("aria-pressed") !== "true") t.click();
        return;
      }

      case "trash": {
        const said = many
          ? T("Move {n} to trash", { n: fmt(picked.length) })
          : T("Move to trash");
        if (!confirm(said + "\n\n" + picked.slice(0, 8).join("\n") +
                     (picked.length > 8 ? "\n…" : ""))) return;
        try {
          const r = await SERVICE.trash(picked);
          /* The rows go now rather than when a watcher gets round to them. */
          for (const x of picked) SEL.delete(x);
          /* Ask the current query again through the path a keystroke takes. */
          if (after) after(picked, r);
          else q.dispatchEvent(new Event("input", { bubbles: true }));
          flash(r.refused && r.refused.length
            ? T("{n} could not be moved", { n: fmt(r.refused.length) })
            : T("{n} moved to trash", { n: fmt(r.gone) }));
        } catch (err) {
          flash(String(err.message || err));
        }
        return;
      }

      case "delete": {
        /* Said in full: the label promised this question, and nothing brings
           these back — not the trash, not the index. */
        const said = many
          ? T("Delete {n} permanently…", { n: fmt(picked.length) })
          : T("Delete permanently…");
        /* A folder's path reads like a file's; this one takes its tree with it. */
        const isDir = (x) => !!(SEL.get(x) || (x === p ? row : null) || {}).is_dir;
        const trees = picked.some(isDir) ? "\n" + T("Folders go with everything in them.") : "";
        const named = picked.slice(0, 8).map((x) => (isDir(x) ? x + "/" : x));
        if (!confirm(said + "\n" + T("This cannot be undone.") + trees + "\n\n" +
                     named.join("\n") + (picked.length > 8 ? "\n…" : ""))) return;
        try {
          const r = await SERVICE.erase(picked);
          for (const x of picked) SEL.delete(x);
          if (after) after(picked, r);
          else q.dispatchEvent(new Event("input", { bubbles: true }));
          flash(r.refused && r.refused.length
            ? T("{n} could not be deleted", { n: fmt(r.refused.length) })
            : T("{n} deleted", { n: fmt(r.gone) }));
        } catch (err) {
          flash(String(err.message || err));
        }
        return;
      }

      case "send": {
        /* The menu becomes the places, as "Open with…" becomes the programs. */
        try {
          const r = await SERVICE.sendTargets();
          if (!r.targets.length) return flash(T("nowhere to send to on this machine"));
          menu.innerHTML = r.targets.map((t) =>
            `<button type="button" data-act="send:${escape_(t.id)}">` +
            `${escape_(t.named ? t.label : T(t.label))}</button>`).join("");
          menu.hidden = false;
        } catch (err) {
          flash(String(err.message || err));
        }
        return;
      }

      case "open-with": {
        /* The menu becomes the list rather than growing a submenu. */
        try {
          const r = await SERVICE.openers(p);
          if (!r.openers.length) return flash(T("nothing on this machine claims it"));
          menu.innerHTML = r.openers.map((o) =>
            `<button type="button" data-act="open-with:${escape_(o.id)}">` +
            `${o.preferred ? "★ " : ""}${escape_(o.name)}</button>`).join("");
          menu.hidden = false;
        } catch (err) {
          flash(String(err.message || err));
        }
        return;
      }

      case "rename": {
        /* `prompt`: modal in the way a rename has to be, and a browser will
           not let a page select the stem alone. */
        const was = leaf(p);
        const name = prompt(T("Rename…"), was);
        if (name === null || name === was) return;
        try {
          const r = await SERVICE.rename(p, name);
          if (r.refused) return flash(T(r.refused));
          if (after) after([p], r);
          else q.dispatchEvent(new Event("input", { bubbles: true }));
        } catch (err) {
          flash(String(err.message || err));
        }
        return;
      }

      default:
        if (act.startsWith("send:")) {
          try {
            const r = await SERVICE.send(act.slice("send:".length), picked);
            if (r.error) return flash(T(r.error));
            flash(r.sent === "linked" ? T("a link is on the desktop")
              : r.sent === "packing" ? T("packing into {path}", { path: r.at })
              : r.sent === "copying" ? T("copying to {path}", { path: r.at })
              : T("handed over"));
          } catch (err) {
            flash(String(err.message || err));
          }
          return;
        }
        if (act.startsWith("open-with:")) {
          try {
            await SERVICE.openWith(p, act.slice("open-with:".length));
          } catch (err) {
            flash(String(err.message || err));
          }
          return;
        }
        /* An id the page has no answer for; the table is shared. */
        flash(T("not in this face yet"));
    }
  }
  const hideMenu = () => { menu.hidden = true; };
  document.addEventListener("click", hideMenu);
  document.addEventListener("scroll", hideMenu, true);
  /* Escape shuts the menu and only the menu: the preview under it stays. */
  document.addEventListener("keydown", (e) => {
    if (e.key !== "Escape" || menu.hidden) return;
    hideMenu();
    e.stopImmediatePropagation();
  });

