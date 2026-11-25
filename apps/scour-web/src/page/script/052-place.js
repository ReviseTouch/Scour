  /* ---------------- the place model ----------------
   * `scour_ui::disks` (the tiles and the strip) and `scour_ui::lens` (terms
   * and the place group) in the page's words, kept alike with the Rust by
   * hand: the page test runs the Rust tests' own cases on these. Pure: no
   * page state, and `home` is passed in. Offsets are the text's own. */

  const DISK_KINDS = ["system", "fixed", "removable", "network", "optical"];
  const DISK_STATES_ON = ["auto", "chosen", "configured"];
  /* The narrowest a strip segment is drawn, as a share of the strip. */
  const STRIP_FLOOR = 0.02;
  /* `Shown::label()`: the msgid each state of a tile is said with. */
  const SHOWN_LABEL = {
    live: "watched live", polled: "checked now and then", walking: "being walked",
    asking: "waiting for an answer", partly: "some folders indexed", off: "not indexed",
  };
  /* How a click changes the place group: this one alone, or this one too. */
  const Pick = Object.freeze({ Only: "only", Also: "also" });

  /* `scour_ui::path`: no trailing `/` (the root stays `/`); `~` for home and back. */
  function untrailed(p) {
    const t = p.replace(/\/+$/, "");
    return !t && p.startsWith("/") ? "/" : t;
  }
  function homeIn(home, p) {
    if (!home || !p.startsWith(home)) return p;
    const rest = p.slice(home.length);
    return !rest || rest.startsWith("/") ? "~" + rest : p;
  }
  function fullIn(home, p) {
    if (!home || !p.startsWith("~")) return p;
    const rest = p.slice(1);
    return !rest || rest.startsWith("/") ? home + rest : p;
  }
  const placeNorm = (home, p) => untrailed(fullIn(home, p));
  /* A path as a term's value: quoted when a space or a `|` would cut it. */
  const quotedPath = (p) => (/[\s|]/.test(p) ? '"' + p + '"' : p);

  /* `lens::replace`: one term for another; for nothing, a space beside it goes too. */
  function placeReplace(text, t, by) {
    if (by) return [text.slice(0, t.start) + by + text.slice(t.end), t.start + by.length];
    let a = t.start, b = t.end;
    if (text[b] === " ") b++;
    else if (text[a - 1] === " ") a--;
    return [text.slice(0, a) + text.slice(b), a];
  }

  /* Terms in runs: two are of one run when a `|` stands between them. */
  function placeRuns(text) {
    const out = [];
    for (const t of placeTerms(text)) {
      const run = out[out.length - 1];
      if (run && text.slice(run[run.length - 1].end, t.start).includes("|")) run.push(t);
      else out.push([t]);
    }
    return out;
  }
  const sameTerm = (x, y) => x.start === y.start && x.end === y.end;
  function placeRunOf(text, t) {
    return placeRuns(text).find((r) => r.some((m) => sameTerm(m, t))) || [t];
  }

  /* `lens::remove_term`: the query less `t` and one `|` beside it. */
  function placeRemove(text, t) {
    const run = placeRunOf(text, t);
    const k = run.findIndex((m) => sameTerm(m, t));
    const cut = (a, b) => [text.slice(0, a) + text.slice(b), a];
    if (k >= 0 && k + 1 < run.length) return cut(t.start, run[k + 1].start);
    if (k > 0) return cut(run[k - 1].end, t.end);
    return placeReplace(text, t, "");
  }

  const isPlace = (t) => t.field === "under" && !t.not;

  /* `lens::place_toggle`: one folder into or out of the place group, the
     first run with an `under:` term; with none, it goes at the end. */
  function placeToggle(text, home, root, pick) {
    const want = placeNorm(home, root);
    const add = "under:" + quotedPath(homeIn(home, want));
    const run = placeRuns(text).find((r) => r.some(isPlace));
    if (!run) {
      const out = text + (!text || /\s$/.test(text) ? "" : " ") + add;
      return [out, out.length];
    }
    const places = run.filter(isPlace);
    const is = (t) => t.value !== "" && placeNorm(home, t.value) === want;
    if (pick === Pick.Also) {
      const mine = places.find(is);
      if (mine) return placeRemove(text, mine);
      const empty = places.find((t) => t.value === "");
      if (empty) return placeReplace(text, empty, add);
      const end = places[places.length - 1].end;
      return [text.slice(0, end) + "|" + add + text.slice(end), end + 1 + add.length];
    }
    const alone = places.every(is);
    // Later ones first, so the offsets of the earlier stay true.
    let out = [text, 0];
    for (const t of places.slice(1).reverse()) out = placeRemove(out[0], t);
    return alone ? placeRemove(out[0], places[0]) : placeReplace(out[0], places[0], add);
  }

  /* `lens::scope`: the group's `under:` terms, and every `!under:` on its own. */
  function placeScope(text, home) {
    const held = (t) => ({ path: placeNorm(home, t.value), term: t });
    const all = placeRuns(text);
    const run = all.find((r) => r.some(isPlace));
    const within = run ? run.filter((t) => isPlace(t) && t.value !== "").map(held) : [];
    const outside = all.filter((r) => r.length === 1).map((r) => r[0])
      .filter((t) => t.not && t.field === "under" && t.value !== "").map(held);
    return { within, outside };
  }
  const scopeHolds = (scope, root) => scope.within.some((h) => h.path === untrailed(root));

  /* `lens::tile_toggle`: every root of a tile as one click; held when all are. */
  function tileToggle(text, home, roots, pick) {
    const want = roots.map((r) => placeNorm(home, r));
    const held = placeScope(text, home).within.map((h) => h.path);
    const allIn = want.length > 0 && want.every((w) => held.includes(w));
    const alone = allIn && held.every((h) => want.includes(h));
    const missing = want.filter((w) => !held.includes(w));
    const [outOf, into] = pick === Pick.Also
      ? (allIn ? [want, []] : [[], missing])
      : (alone ? [held, []] : [held.filter((h) => !want.includes(h)), missing]);
    let out = [text, text.length];
    for (const root of [...outOf, ...into]) out = placeToggle(out[0], home, root, Pick.Also);
    return out;
  }

  /* ---- the tiles: `scour_ui::disks::tiles` ---- */

  /* A disk of the `volumes` answer as `DiskIn` borrows it. */
  function diskIn(d) {
    const v = d.volume || {}, s = d.seen || {};
    return {
      id: v.id || "", label: v.label || "", kind: v.kind || "fixed", fs: v.fs || "",
      roots: v.roots || [], size: v.size || 0, state: d.state || "",
      live: !!s.live, walking: !!s.walking,
    };
  }

  /* `path` is `root` or below it, by whole components. */
  function liesUnder(path, root) {
    const p = untrailed(path), r = untrailed(root);
    return (r === "/" && p.startsWith("/")) || p === r || (p.startsWith(r) && p[r.length] === "/");
  }

  /* The disk a path lies on: the one with the longest root above it. */
  function diskOf(path, disks) {
    let best = null, len = -1;
    for (const d of disks) {
      for (const root of d.roots) {
        const r = untrailed(root);
        if (liesUnder(path, r) && r.length >= len) { best = d.id; len = r.length; }
      }
    }
    return best;
  }

  /* Every source root on the disk `id`, sorted, each once. */
  function rootsOn(id, disks, sources) {
    const out = [];
    for (const s of sources) {
      for (const r of s.roots) if (diskOf(r, disks) === id) out.push(untrailed(r));
    }
    return [...new Set(out)].sort();
  }

  /* `file: under:a|under:b`, less every other disk mounted below those roots. */
  function countQuery(id, roots, disks) {
    let q = "file: " + roots.map((r) => "under:" + quotedPath(r)).join("|");
    for (const d of disks) {
      if (d.id === id) continue;
      for (const root of d.roots) {
        const r = untrailed(root);
        if (roots.some((x) => x !== r && liesUnder(r, x))) q += " !under:" + quotedPath(r);
      }
    }
    return q;
  }

  function diskTile(d, all, sources, files) {
    const roots = rootsOn(d.id, all, sources);
    const whole = roots.some((r) => d.roots.some((x) => r === untrailed(x)));
    const indexed = DISK_STATES_ON.includes(d.state);
    const shown = d.state === "asking" || d.state === "later" ? "asking"
      : (indexed || roots.length) && d.walking ? "walking"
      : indexed && d.live ? "live"
      : indexed ? "polled"
      : roots.length ? "partly" : "off";
    // An indexed disk counted before `sources` came is its own source.
    const counted = indexed && !roots.length ? d.roots.map(untrailed) : roots;
    const count = shown !== "asking" && counted.length ? countQuery(d.id, counted, all) : null;
    return {
      id: d.id, name: d.label || d.roots[0] || "",
      kind: DISK_KINDS.includes(d.kind) ? d.kind : "fixed",
      fs: d.fs, root: d.roots.length ? untrailed(d.roots[0]) : "", size: d.size,
      // Never empty for an indexed disk: a click must have a term to write.
      roots: counted, whole: whole || (indexed && !roots.length),
      shown, files: count === null ? null : files ?? null, count,
      index_it: shown === "asking" || shown === "off",
    };
  }

  /* The tiles of `disks` (as `diskIn` gives them), in their order; `files`
     the counts so far, by disk id. A click on one writes `under:`: its
     `count` is not null. */
  function diskTiles(disks, sources, files) {
    return disks.map((d) => diskTile(d, disks, sources, files(d.id)));
  }

  /* `scour_ui::disks::strip`: one segment per choosable tile, by its files,
     none under the floor; even while nothing is counted. Shares sum to 1. */
  function diskStrip(tiles) {
    const on = tiles.map((t, i) => [i, t]).filter(([, t]) => t.count !== null)
      .map(([i, t]) => [i, t.files ?? 0]);
    const n = on.length;
    const total = on.reduce((s, [, f]) => s + f, 0);
    if (total <= 0 || n * STRIP_FLOOR >= 1) return on.map(([at]) => ({ at, share: 1 / n }));
    // Lift the smallest to the floor and share what is left among the rest.
    const floored = on.map(() => false);
    for (;;) {
      let rest = 0;
      on.forEach(([, c], k) => { if (!floored[k]) rest += c; });
      const left = 1 - STRIP_FLOOR * floored.filter(Boolean).length;
      let moved = false;
      on.forEach(([, c], k) => {
        if (!floored[k] && (rest <= 0 || c / rest * left < STRIP_FLOOR)) { floored[k] = true; moved = true; }
      });
      if (!moved) return on.map(([at, c], k) => ({ at, share: floored[k] ? STRIP_FLOOR : c / rest * left }));
    }
  }

