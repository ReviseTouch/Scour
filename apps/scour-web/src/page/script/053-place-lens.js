  /* ---------------- the place lens ----------------
   * Disks first: the strip, a tile a disk, then the folders, then what the
   * query searches in. Drawn from the `volumes` and `sources` answers and the
   * text; every click writes the text through the place model above. */

  /* The last `volumes` and `sources` answers, for the lens and the scope chip. */
  let PLACE_DISKS = [], PLACE_SOURCES = [];
  /* The folder the list keeps showing after a click, while the text is what it wrote. */
  let placeView = null;
  /* Files per tile's weighed count query, counted once per opening of the lens. */
  let placeFiles = new Map(), placeAsked = new Set(), placeOpening = 0;
  /* Which row the arrows are on: kept across a click's rewrite, reset by typing. */
  let placeSelKey = "", placeSelAfter = null, placeSelAuto = true, placeDrawnSel = -1;
  const COUNT_ALL = 4294967295;

  const DISK_ART = {
    system: SVG('<rect x="2" y="2.5" width="12" height="8.5" rx="1.2"/><path d="M5.5 13.5h5M8 11v2.5"/>'),
    fixed: SVG('<rect x="1.8" y="4.5" width="12.4" height="7" rx="1.6"/><path d="M4.3 8h3.4"/><circle cx="11.4" cy="8" r=".7"/>'),
    removable: SVG('<rect x="4.8" y="6" width="6.4" height="8" rx="1.2"/><path d="M6.3 6V2.2h3.4V6M7.2 3.8v.4M8.8 3.8v.4"/>'),
    network: SVG('<circle cx="8" cy="8" r="5.7"/><path d="M2.3 8h11.4M8 2.3c2.2 2.4 2.2 9 0 11.4M8 2.3c-2.2 2.4-2.2 9 0 11.4"/>'),
    optical: SVG('<circle cx="8" cy="8" r="5.7"/><circle cx="8" cy="8" r="1.6"/>'),
  };

  /* A place lens opens: the disks are asked again and counted again. */
  function placeOpen() {
    lensSeen.delete("volumes");
    lensSeen.delete("sources");
    placeFiles = new Map();
    placeAsked = new Set();
    placeOpening++;
    placeDrawnSel = -1;
    placeSelKey = "";
  }

  /* An answer about the disks, for whoever draws them. */
  function placeHeard(disks, sources) {
    if (disks) PLACE_DISKS = disks;
    if (sources) PLACE_SOURCES = sources;
    drawScopeChip(q.value);
    // The Location capsule names the disks it holds whole.
    paint(q.value);
  }

  function placeTilesNow() {
    const disks = (LENS && LENS.disks) || PLACE_DISKS;
    const sources = (LENS && LENS.sources) || PLACE_SOURCES;
    const tiles = diskTiles(disks.map(diskIn), sources, () => null);
    for (const tl of tiles) if (tl.count !== null) tl.files = placeFiles.get(placeWeighed(tl)) ?? null;
    return tiles;
  }

  /* What a tile is counted as: its roots, weighed by the query less its
     places — never by a query that would read files. */
  function placeWeighed(tl) {
    const rest = withoutPlaces(q.value);
    return rest && !READS.test(rest) ? tl.count + " " + rest : tl.count;
  }

  /* Each tile: held whole by the query, and whether the query searches on it. */
  function placeMarks(tiles, scope) {
    return tiles.map((tl) => {
      const sel = tileHeld(tl, scope), lit = tileLit(tl, scope);
      return { sel, lit, some: !sel && lit && scope.within.length > 0 };
    });
  }

  /* The folder the list shows, and what is typed after it to narrow it. */
  function placeViewAt(t) {
    if (placeView && placeView.text === q.value) return [placeView.dir, ""];
    return placeAt(t.value, HOME);
  }

  /* The lens's term as the place model reads it. */
  function placeLensTerm() {
    return placeTerms(q.value).find((x) => x.start === LENS.a) || null;
  }

  /* A folder's own folders: weighed by `usage`, and the light ones `tree` adds. */
  function placeDirsOf(part) {
    const want = fold(part);
    const byPath = new Map();
    const u = LENS.usage;
    for (const c of (u && u.children) || []) {
      byPath.set(c.path, { path: c.path, name: c.path.slice(c.path.lastIndexOf("/") + 1), bytes: c.bytes || 0, files: c.files || 0 });
    }
    for (const d of LENS.tree || []) {
      if (!byPath.has(d.path)) byPath.set(d.path, { path: d.path, name: d.name, bytes: 0, files: d.children || 0 });
    }
    // `usage` of `/` answers `/` itself as its one child.
    byPath.delete(LENS.placeDir);
    return [...byPath.values()]
      .filter((d) => !want || fold(d.name).includes(want))
      .sort((x, y) => (y.bytes - x.bytes) || (y.files - x.files) || x.name.localeCompare(y.name))
      .slice(0, 60);
  }

  /* What the arrows move over, in the order it is drawn: tiles, then folders. */
  function lensPlaceItems() {
    const t = LENS.term;
    const fld = t.field || "under";
    const [active, part] = placeViewAt(t);
    LENS.placeDir = active;
    LENS.title = T("Where it searches");
    LENS.foot = T("→ goes into a folder, Backspace climbs back out; Enter chooses it; Tab leaves the term.");
    const out = placeTilesNow().map((tl, i) => ({ label: tl.name, tile: i, wait: tl.count === null }));
    const tiles = out.length;
    out.push({ label: T("this folder"), term: neg(t) + fld + ":" + quoteIf(homeForm(active)), here: true });
    for (const d of placeDirsOf(part)) {
      out.push({ label: d.name + "/", term: neg(t) + fld + ":" + quoteIf(homeForm(d.path)), dir: d.path, stays: true, bytes: d.bytes, files: d.files });
    }
    // A new term starts on the first tile, a path being typed on its folders.
    const key = t.a + "\n" + t.value + "\n" + tiles;
    if (placeSelAfter !== null) {
      [LENS.sel, placeSelKey, placeSelAfter, placeSelAuto] = [placeSelAfter, key, null, false];
    }
    if (key !== placeSelKey) placeSelAuto = true;
    if (placeSelAuto || !out[LENS.sel] || out[LENS.sel].wait) {
      placeSelKey = key;
      const first = (want) => out.findIndex((it) => !it.wait && want(it));
      const dir = part ? first((it) => it.dir) : -1;
      const folder = first((it) => it.tile === undefined), tile = first((it) => it.tile !== undefined);
      const pick = t.value ? [dir, folder, tile] : [tile, folder];
      LENS.sel = Math.max(pick.find((k) => k >= 0) ?? 0, 0);
    }
    return out;
  }

  /* Each disk's files, asked once the panel is drawn and both answers are in. */
  function placeCount() {
    if (!LENS || LENS.kind !== "place" || !LENS.disks || !LENS.sources) return;
    const mine = placeOpening;
    for (const tl of placeTilesNow()) {
      if (tl.count === null) continue;
      const key = placeWeighed(tl);
      if (placeAsked.has(key)) continue;
      placeAsked.add(key);
      SERVICE.get("count", { q: key, cap: COUNT_ALL })
        .then((r) => {
          if (mine !== placeOpening) return;
          placeFiles.set(key, r.total);
          if (LENS && LENS.kind === "place") lensDraw();
        })
        .catch(() => {});
    }
  }

  /* Write what a click made, and keep the lens on the place group's first
     term, so the panel does not move under the pointer. */
  function placeWrite(text, viewDir, sel) {
    const [out, at] = placeAnchor(text, HOME, LENS.a);
    placeSelAfter = sel ?? LENS.sel;
    q.value = out;
    q.setSelectionRange(at, at);
    placeView = viewDir ? { dir: viewDir, text: out } : null;
    render();
    lensSync(true);
  }

  /* A tile clicked: this disk alone, or (Ctrl) this one too. Enter closes. */
  function placeTile(i, pick, close) {
    const tl = placeTilesNow()[i];
    if (!tl || tl.count === null) return;
    const [text, caret] = tileToggle(q.value, HOME, tl.roots, pick);
    if (close) {
      // Enter: written, and the caret stands outside the term.
      const gap = /\s/.test(text[caret] || "") ? "" : " ";
      q.value = text.slice(0, caret) + gap + text.slice(caret);
      q.setSelectionRange(caret + gap.length, caret + gap.length);
      render();
      lensClose();
      return;
    }
    // A disk chosen shows its own folders: its root, or home on a disk that holds it.
    const held = tl.roots.every((r) => scopeHolds(placeScope(text, HOME), r));
    const home = tl.whole ? tl.root : tl.roots.includes(HOME) ? HOME : tl.roots[0];
    placeWrite(text, held && home ? home : LENS.placeDir, i);
  }

  /* "Add": into the group, never out of it; the lens's term gives way while typed. */
  function placePressAdd(path) {
    const typed = placeView && placeView.text === q.value ? null : placeLensTerm();
    placeWrite(placeAdd(q.value, HOME, path, typed)[0], LENS.placeDir);
  }

  /* "Exclude": its own `!under:` term at the end, or taken away again. */
  function placePressExclude(path) {
    placeWrite(placeExclude(q.value, HOME, path)[0], LENS.placeDir);
  }

  /* An entry of "Searching in:" taken away: the n-th of `spots`. */
  function placeDrop(n) {
    const spot = spots(q.value, HOME, placeTilesNow())[n];
    if (!spot) return;
    placeWrite(spotRemove(spot, q.value, HOME)[0], LENS.placeDir);
  }

  function placeAnswer(id, what) {
    SERVICE.answerDisk(id, what)
      .then(() => {
        if (what === "index") flash(T("The disk is being indexed."));
        lensSeen.delete("volumes");
        lensFill();
        loadDisks();
      })
      .catch((err) => flash(String(err.message || err)));
  }

  /* The lens's clicks that belong to the place model; true when handled. */
  function placeMouse(e) {
    const hit = (attr) => e.target.closest("[" + attr + "]");
    let el;
    if ((el = hit("data-tdo"))) placeAnswer(el.dataset.tid, el.dataset.tdo);
    else if ((el = hit("data-prm"))) placeDrop(+el.dataset.prm);
    else if ((el = hit("data-padd"))) placePressAdd(el.dataset.padd);
    else if ((el = hit("data-pexcl"))) placePressExclude(el.dataset.pexcl);
    else if ((el = hit("data-tile"))) placeTile(+el.dataset.tile, e.ctrlKey || e.metaKey ? Pick.Also : Pick.Only, false);
    else return false;
    return true;
  }

  /* Drawn again as answers land: the lists keep where they were scrolled to,
     and follow the keyboard only when it moved. */
  function placeDraw() {
    const scrolled = [...lensBox.querySelectorAll(".lp-tiles, .lp-left, .lp-right")].map((el) => el.scrollTop);
    lensBox.innerHTML = lensPlaceHtml();
    lensBox.querySelectorAll(".lp-tiles, .lp-left, .lp-right").forEach((el, i) => { el.scrollTop = scrolled[i] || 0; });
    if (placeDrawnSel === LENS.sel) return;
    placeDrawnSel = LENS.sel;
    const on = lensBox.querySelector(".lp-tile.kb, .lp-dir.on");
    if (on) on.scrollIntoView({ block: "nearest" });
  }

