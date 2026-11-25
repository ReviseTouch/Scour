  /* ---------------- where the query looks ----------------
   * `scour_ui::lens::spots` and `lens::capsule` in the page's words: the
   * entries of "Searching in", the scope chip's line and dots, and the parts
   * a run of `under:` terms, or a block of `!under:` ones, is drawn as. Pure; kept alike by the page test. */

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

  /* `Spot::remove`: the query without its terms, each with one `|` beside it,
     in whichever run they are; the caret where the first was. */
  function spotRemove(spot, text) {
    let out = [text, spot.terms[0].start];
    for (const t of [...spot.terms].reverse()) out = placeRemove(out[0], t);
    return out;
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
     held whole as one — and then where it does not, a tile left out whole too. */
  function spots(text, home, tiles) {
    const s = placeScope(text, home);
    return [...placeGather(s.within, home, tiles, false, null), ...placeGather(s.outside, home, tiles, true, null)];
  }

  /* `spots::gather`: `members` as entries in the order of the text, a tile
     whose roots they all hold as one. With `raw` (a capsule's parts) a tile
     collapses only over consecutive members, none of them raw. */
  function placeGather(members, home, tiles, not, raw) {
    const taken = members.map(() => false);
    const out = [];
    tiles.forEach((t, k) => {
      const on = members.map((_, i) => i).filter((i) => !taken[i] && t.roots.includes(members[i].path));
      if (!placeCollapses(t, members, on, raw)) return;
      on.forEach((i) => { taken[i] = true; });
      out.push({ tile: k, name: t.name, short: t.name, kind: t.kind, not, paths: [...t.roots], terms: on.map((i) => members[i].term) });
    });
    members.forEach((h, i) => { if (!taken[i]) out.push(spotFolder(h, home, not)); });
    return out.sort((x, y) => x.terms[0].start - y.terms[0].start);
  }

  /* `spots::collapses`: the members at `on` stand for the whole tile. */
  function placeCollapses(t, members, on, raw) {
    const all = t.roots.every((r) => on.some((i) => members[i].path === r));
    if (!tileChoosable(t) || !t.roots.length || !all) return false;
    return !raw || (on.every((i) => !raw(members[i].term)) && on[on.length - 1] - on[0] + 1 === on.length);
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

  /* `lens::place_caps_at`: every run made only of positive `under:` terms as
     one capsule of parts, every part shown. `caret` is a `Caret`, or null. */
  function placeCapsAt(text, home, tiles, caret) {
    return placeRuns(text).filter((r) => r.every(isPlace)).map((run) => placeCapOfTerms(run, home, tiles, caret, false));
  }

  /* `lens::exclusion_caps`: each block of standalone `!under:` terms side by
     side as one negated capsule, drawn as a group is. */
  function exclusionCaps(text, home, tiles, caret) {
    return exclusionBlocks(text).map((block) => placeCapOfTerms(block, home, tiles, caret, true));
  }

  function placeCapOfTerms(members, home, tiles, caret, not) {
    const held = members.map((t) => ({ path: placeNorm(home, t.value), term: t }));
    const parts = placeGather(held, home, tiles, not, (t) => isEdited(t, caret)).map((spot) => {
      const empty = spot.tile === null && spot.terms[0].value === "";
      return {
        spot,
        crumbs: spot.tile === null && !empty ? crumbs(spot.paths[0], home) : [],
        raw: spot.tile === null && isEdited(spot.terms[0], caret),
        caret: !!caret && spot.terms.some((t) => t.start < caret.at && caret.at <= t.end),
        shown: true,
      };
    });
    return { start: members[0].start, end: members[members.length - 1].end, parts, hidden: 0 };
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

