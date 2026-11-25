  /* ---------------- where the query looks ----------------
   * `scour_ui::lens::spots` and `lens::capsule` in the page's words: the
   * entries of "Searching in", the scope chip's line and dots, and the parts
   * a run of `under:` terms is drawn as. Pure; kept alike by the page test. */

  /* `path::leaf`: the last component; a path of slashes is its own name. */
  function pathLeaf(p) {
    const t = p.replace(/\/+$/, "");
    if (!t) return p;
    const leaf = t.slice(t.lastIndexOf("/") + 1);
    return leaf || t;
  }

  const tileChoosable = (t) => t.count !== null;

  /* A place of the query as a `Spot`: one folder, or (`tile`) a whole disk. */
  function spotFolder(h, home, not) {
    const shown = homeIn(home, h.path);
    return { tile: null, name: shown, short: pathLeaf(shown), kind: null, not, paths: [h.path], terms: [h.term] };
  }

  /* `Spot::remove`: a tile goes as `tile_toggle(…, Also)`, the rest as its term. */
  function spotRemove(spot, text, home) {
    return spot.tile !== null ? tileToggle(text, home, spot.paths, Pick.Also) : placeRemove(text, spot.terms[0]);
  }

  /* `lens::tile_held`: the place group holds every root of the tile. */
  function tileHeld(t, scope) {
    return tileChoosable(t) && t.roots.length > 0 && t.roots.every((r) => scopeHolds(scope, r));
  }

  /* `lens::tile_lit`: the query searches somewhere on the tile's disk. */
  function tileLit(t, scope) {
    const near = (r) => scope.within.some((h) => liesUnder(h.path, r) || liesUnder(r, h.path));
    return tileChoosable(t) && (!scope.within.length || t.roots.some(near));
  }

  /* `lens::spots`: where `text` looks, in the order of its terms — a tile
     held whole as one — and then where it does not. */
  function spots(text, home, tiles) {
    const s = placeScope(text, home);
    return [...placeGather(s.within, home, tiles, null), ...s.outside.map((h) => spotFolder(h, home, true))];
  }

  /* `spots::gather`: each tile whose roots `members` all hold is one entry,
     at its first member, `lone` left out of it; every other member its own. */
  function placeGather(members, home, tiles, lone) {
    const paths = members.map((h) => h.path);
    const taken = members.map(() => false);
    const out = [];
    tiles.forEach((t, k) => {
      if (!tileChoosable(t) || !t.roots.length || !t.roots.every((r) => paths.includes(r))) return;
      const terms = [];
      members.forEach((h, i) => {
        if (taken[i] || !t.roots.includes(h.path) || (lone && sameTerm(lone, h.term))) return;
        taken[i] = true;
        terms.push(h.term);
      });
      if (terms.length) out.push({ tile: k, name: t.name, short: t.name, kind: t.kind, not: false, paths: [...t.roots], terms });
    });
    members.forEach((h, i) => { if (!taken[i]) out.push(spotFolder(h, home, false)); });
    return out.sort((x, y) => x.terms[0].start - y.terms[0].start);
  }

  /* `lens::chip_label`: "everywhere", or the short names joined by " + ",
     the first `max` of them and "+k" for the rest. */
  function chipLabel(list, max, everywhere) {
    const names = list.filter((s) => !s.not).map((s) => s.short);
    const m = Math.max(max, 1);
    if (!names.length) return everywhere;
    if (names.length <= m) return names.join(" + ");
    return names.slice(0, m).join(" + ") + " +" + (names.length - m);
  }

  /* `lens::chip_dots`: every disk something is indexed on, and every one asking. */
  function chipDots(tiles, scope) {
    return tiles.filter((t) => tileChoosable(t) || t.shown === "asking")
      .map((t) => ({ kind: t.kind, asking: t.shown === "asking", lit: tileLit(t, scope) }));
  }

  /* `lens::crumbs`: the steps of `path` from `~` (inside `home`) or from `/`. */
  function crumbs(path, home) {
    const shown = homeIn(home, path);
    const fromHome = !!home && shown.startsWith("~");
    let at = fromHome ? home : "";
    const out = [{ label: fromHome ? "~" : "/", path: at || "/" }];
    for (const seg of (fromHome ? shown.slice(1) : path).split("/").filter(Boolean)) {
      at += "/" + seg;
      out.push({ label: seg, path: at });
    }
    return out;
  }

  /* `lens::place_caps`: every run made only of positive `under:` terms, as
     one capsule of parts, every part shown. `caret` is null without one. */
  function placeCaps(text, home, tiles, caret) {
    const inside = (t) => caret !== null && t.start < caret && caret <= t.end;
    return placeRuns(text).filter((r) => r.every(isPlace)).map((run) => {
      const members = run.map((t) => ({ path: placeNorm(home, t.value), term: t }));
      const lone = run.find(inside) || null;
      const parts = placeGather(members, home, tiles, lone).map((spot) => {
        const caretIn = spot.terms.some(inside);
        const empty = spot.tile === null && spot.terms[0].value === "";
        const steps = spot.tile === null && !empty ? crumbs(spot.paths[0], home) : [];
        return { spot, crumbs: steps, raw: caretIn || empty, caret: caretIn, shown: true };
      });
      return { start: run[0].start, end: run[run.length - 1].end, parts, hidden: 0 };
    });
  }

  /* `PlaceCap::part_at`: the part an offset of the run is drawn at — the one
     whose term it is in or just after, else the one before, else the first. */
  function capPartAt(cap, offset) {
    if (offset < cap.start || offset > cap.end) return null;
    const all = cap.parts.flatMap((p, k) => p.spot.terms.map((t) => [k, t]));
    const hit = all.find(([, t]) => t.start < offset && offset <= t.end) ||
      all.filter(([, t]) => t.end <= offset).reduce((b, x) => (!b || x[1].end > b[1].end ? x : b), null) ||
      all.reduce((b, x) => (!b || x[1].start < b[1].start ? x : b), null);
    return hit ? hit[0] : null;
  }

  /* `PlaceCap::fold`: no more than `max` parts shown, at least one, the
     caret's always among them; `hidden` counts the rest. */
  function capFold(cap, max) {
    const m = Math.max(max, 1), n = cap.parts.length;
    const caret = cap.parts.findIndex((p) => p.caret);
    cap.parts.forEach((p, k) => { p.shown = n <= m || k < m; });
    if (caret >= m && n > m) {
      cap.parts[m - 1].shown = false;
      cap.parts[caret].shown = true;
    }
    cap.hidden = cap.parts.filter((p) => !p.shown).length;
  }

