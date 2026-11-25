  /* ---------------- the place lens ----------------
   * Disks first: the strip, a tile a disk, then the folders, then what the
   * query searches in. Drawn from the `volumes` and `sources` answers and the
   * text; every click writes the text through the place model above. */

  /* The last `volumes` and `sources` answers, for the lens and the scope chip. */
  let PLACE_DISKS = [], PLACE_SOURCES = [];
  /* The folder the list keeps showing after a click, while the text is what it wrote. */
  let placeView = null;
  /* Files per disk, counted once per opening of the lens. */
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
  }

  function placeTilesNow() {
    const disks = (LENS && LENS.disks) || PLACE_DISKS;
    const sources = (LENS && LENS.sources) || PLACE_SOURCES;
    return diskTiles(disks.map(diskIn), sources, (id) => placeFiles.get(id));
  }

  /* Each tile: held whole by the query, or some folder of it is. */
  function placeMarks(tiles, scope) {
    const ins = ((LENS && LENS.disks) || PLACE_DISKS).map(diskIn);
    return tiles.map((tl) => ({
      sel: tl.count !== null && tl.roots.every((r) => scopeHolds(scope, r)),
      some: scope.within.some((h) => diskOf(h.path, ins) === tl.id),
    }));
  }

  /* The folder the list shows, and what is typed after it to narrow it. */
  function placeAt(t) {
    if (placeView && placeView.text === q.value) return [placeView.dir, ""];
    const [base, part] = placeParts(t.value);
    return [base ? fullForm(base) : (HOME || "/"), part];
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
    const [active, part] = placeAt(t);
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

  /* The query without its places, what the folder list is weighed by: a
     click that adds, excludes or picks a disk does not reorder the list. */
  function placeRest(text) {
    const drop = placeTerms(text).filter((t) => t.field === "under" || t.field === "parent");
    let out = text;
    for (const t of drop.reverse()) out = placeRemove(out, t)[0];
    return out.replace(/\s+/g, " ").trim();
  }

  /* Each disk's files, asked once the panel is drawn and both answers are in. */
  function placeCount() {
    if (!LENS || LENS.kind !== "place" || !LENS.disks || !LENS.sources) return;
    const mine = placeOpening;
    for (const tl of placeTilesNow()) {
      const key = tl.id + "\n" + tl.count;
      if (tl.count === null || placeAsked.has(key)) continue;
      placeAsked.add(key);
      SERVICE.get("count", { q: tl.count, cap: COUNT_ALL })
        .then((r) => {
          if (mine !== placeOpening) return;
          placeFiles.set(tl.id, r.total);
          if (LENS && LENS.kind === "place") lensDraw();
        })
        .catch(() => {});
    }
  }

  /* Write what a click made, and keep the lens on the place group's first
     term, so the panel does not move under the pointer. */
  function placeWrite(text, caret, viewDir, sel) {
    const [out, at] = placeAnchor(text, caret, LENS.term);
    placeSelAfter = sel ?? LENS.sel;
    q.value = out;
    q.setSelectionRange(at, at);
    placeView = viewDir ? { dir: viewDir, text: out } : null;
    render();
    lensSync(true);
  }

  /* Where the caret goes: the end of the group's first term; an empty
     `under:` where the group was when none is left; the lens's own term
     when it was not of the group. */
  function placeAnchor(text, caret, old) {
    if (isPlace(old)) {
      const run = placeRuns(text).find((r) => r.some(isPlace));
      if (run) return [text, run.find(isPlace).end];
      const at = Math.min(old.a, text.length);
      const lead = at > 0 && !/\s/.test(text[at - 1]) ? " " : "";
      const tail = at < text.length && !/\s/.test(text[at]) ? " " : "";
      return [text.slice(0, at) + lead + "under:" + tail + text.slice(at), at + lead.length + 6];
    }
    let best = null;
    for (const x of placeTerms(text)) {
      if (text.slice(x.start, x.end) !== old.raw) continue;
      if (!best || Math.abs(x.start - old.a) < Math.abs(best.start - old.a)) best = x;
    }
    return [text, best ? best.end : caret];
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
    placeWrite(text, caret, held && home ? home : LENS.placeDir, i);
  }

  /* "Exclude": its own `!under:` term at the end, or taken away again. */
  function placeExclude(path) {
    const v = q.value;
    const have = placeScope(v, HOME).outside.find((h) => h.path === untrailed(path));
    if (have) {
      const [text, caret] = placeRemove(v, have.term);
      placeWrite(text, caret, LENS.placeDir);
      return;
    }
    const text = v + (!v || /\s$/.test(v) ? "" : " ") + "!under:" + quotedPath(homeForm(untrailed(path)));
    placeWrite(text, text.length, LENS.placeDir);
  }

  /* An entry of "Searching in:" taken away: a tile, a folder or an exclusion. */
  function placeDrop(what) {
    const [kind, n] = [what[0], +what.slice(2)];
    const v = q.value;
    const scope = placeScope(v, HOME);
    let out = null;
    if (kind === "t") {
      const tl = placeTilesNow()[n];
      if (tl) out = tileToggle(v, HOME, tl.roots, Pick.Also);
    } else if (kind === "w" && scope.within[n]) {
      out = placeToggle(v, HOME, scope.within[n].path, Pick.Also);
    } else if (kind === "o" && scope.outside[n]) {
      out = placeRemove(v, scope.outside[n].term);
    }
    if (out) placeWrite(out[0], out[1], LENS.placeDir);
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
    else if ((el = hit("data-prm"))) placeDrop(el.dataset.prm);
    else if ((el = hit("data-padd"))) {
      const [text, caret] = placeToggle(q.value, HOME, el.dataset.padd, Pick.Also);
      placeWrite(text, caret, LENS.placeDir);
    } else if ((el = hit("data-pexcl"))) placeExclude(el.dataset.pexcl);
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

