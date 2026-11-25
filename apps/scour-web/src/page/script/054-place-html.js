  /* ---------------- the place lens, drawn ----------------
   * Top to bottom: the strip and the tiles; the places and the folder list;
   * "Searching in:", read from the text each time; the keys. */

  function lensPlaceHtml() {
    const t = LENS.term;
    const tiles = placeTilesNow();
    const scope = placeScope(q.value, HOME);
    const marks = placeMarks(tiles, scope);
    const on = LENS.items[LENS.sel] || {};
    const active = LENS.placeDir || placeViewAt(t)[0];
    return placeTopHtml(tiles, marks, on) +
      '<div class="lp-grid">' + placeLeftHtml(active) + placeRightHtml(active, scope, on) + "</div>" +
      placeBasketHtml(tiles, scope) +
      '<div class="lfoot lp-keys">' +
      (tiles.some((tl) => tl.count !== null)
        ? "<span>" + escape_(T("click a disk: search only there")) + "</span><span>" + escape_(T("Ctrl+click: several")) + "</span>" : "") +
      "<span>" + escape_(LENS.foot) + "</span></div>";
  }

  function placeTopHtml(tiles, marks, on) {
    if (!tiles.length) return "";
    const strip = diskStrip(tiles);
    const segs = strip.map(({ at, share }) => {
      const tl = tiles[at];
      const lit = marks[at].lit;
      const said = tl.name + (tl.files !== null ? " · " + T("{n} files", { n: fmt(tl.files) }) : "");
      return '<i class="seg' + (lit ? "" : " dim") + '" data-tile="' + at + '" style="flex-grow:' + share.toFixed(5) +
        ";background:var(--disk-" + tl.kind + ')" title="' + escape_(said) + '"></i>';
    }).join("");
    const counted = strip.every(({ at }) => tiles[at].files !== null);
    const total = strip.reduce((n, { at }) => n + (tiles[at].files || 0), 0);
    const legend = strip.length
      ? '<div class="lp-strip">' + segs + '</div><div class="lp-strip-l"><span>' +
        escape_(T("Where the search looks: one slice a disk, as wide as its files in the index")) + "</span><span>" +
        (counted ? escape_(T("{n} files", { n: fmt(total) })) : "") + "</span></div>"
      : "";
    const states = (LENS.disks || PLACE_DISKS).map((d) => d.state);
    return '<div class="lp-top">' + legend + '<div class="lp-tiles">' +
      tiles.map((tl, i) => placeTileHtml(tl, i, marks[i], on.tile === i, states[i] === "asking")).join("") +
      "</div></div>";
  }

  function placeTileHtml(tl, i, mark, kb, fresh) {
    const choosable = tl.count !== null;
    const cls = "lp-tile k-" + tl.kind + (mark.sel ? " sel" : mark.some ? " some" : "") +
      (kb ? " kb" : "") + (choosable ? "" : " off");
    const kind = T((DISKS.kinds || {})[tl.kind] || tl.kind);
    const meta = [tl.fs, tl.root].filter(Boolean).join(" · ");
    const tags = !tl.whole && tl.roots.length
      ? '<div class="roots">' + tl.roots.map((r) => "<span>" + escape_(homeForm(r)) + "</span>").join("") + "</div>" : "";
    const files = tl.files !== null ? '<span class="n">' + escape_(T("{n} files", { n: fmt(tl.files) })) + "</span>" : "";
    const btn = (what, label, pri) => '<button type="button" class="lp-btn' + (pri ? " pri" : "") +
      '" data-tdo="' + what + '" data-tid="' + escape_(tl.id) + '">' + escape_(T(label)) + "</button>";
    const asking = tl.shown === "asking";
    const question = (DISKS.questions || {})[tl.kind] || "{name}";
    const acts = asking
      ? '<div class="ask">' + escape_(T(question, { name: tl.name })) + '</div><div class="acts">' +
        (tl.index_it ? btn("index", "Index it", true) : "") + btn("later", "Not this time") + btn("leave", "Never") + "</div>"
      : tl.index_it ? '<div class="acts">' + btn("index", "Index it", false) + "</div>" : "";
    return '<div class="' + cls + '" data-tile="' + i + '" role="option" aria-selected="' + mark.sel + '">' +
      '<div class="top"><span class="ic" title="' + escape_(kind) + '">' + (DISK_ART[tl.kind] || "") + "</span>" +
      '<span class="nm" title="' + escape_(tl.name) + '">' + escape_(tl.name) + "</span>" +
      (fresh ? '<span class="newb">' + escape_(T("newly plugged in")) + "</span>" : "") +
      (choosable ? '<span class="chk" aria-hidden="true">✓</span>' : "") + "</div>" +
      '<div class="mt" title="' + escape_(kind + " · " + meta) + '">' + escape_(meta) + "</div>" + tags +
      '<div class="ln"><span class="st ' + tl.shown + '">' + escape_(T(SHOWN_LABEL[tl.shown])) + "</span>" + files + "</div>" +
      acts + "</div>";
  }

  function placeLeftHtml(active) {
    const rows = (HOME ? [["~", HOME], ...SCOPES] : SCOPES).map(([label, path]) =>
      '<button type="button" class="lp-item' + (active === path ? " active" : "") + '" data-pdir="' + escape_(path) + '">' +
      '<span class="nm">' + escape_(label) + '</span><span class="sub">' + escape_(homeForm(path)) + "</span></button>").join("");
    return '<div class="lp-left"><div class="lp-sec">' + escape_(T("Places")) + "</div>" + rows +
      '<button type="button" class="lp-pick" data-ppick="' + escape_(active) + '">📁 ' + escape_(T("Choose folder…")) + "</button></div>";
  }

  function placeCrumbHtml(active) {
    const fld = LENS.term.field || "under";
    const shown = homeForm(active);
    const segs = [];
    let cur = shown.startsWith("~") ? HOME : "";
    segs.push(shown.startsWith("~") ? ["~", HOME] : ["/", "/"]);
    for (const p of (shown.startsWith("~") ? shown.slice(1) : active).split("/").filter(Boolean)) {
      cur += "/" + p;
      segs.push([p, cur]);
    }
    const up = active === "/" ? null : (active.lastIndexOf("/") <= 0 ? "/" : active.slice(0, active.lastIndexOf("/")));
    return '<div class="lp-crumb">' +
      (up ? '<button type="button" data-pdir="' + escape_(up) + '" title="' + escape_(T("Climb up")) + '">‹</button>' : "") +
      segs.map(([label, p], i) => (i ? '<span class="s">/</span>' : "") +
        '<button type="button" class="' + (i + 1 === segs.length ? "cur" : "") + '" data-pdir="' + escape_(p) + '">' + escape_(label) + "</button>").join("") +
      '<button type="button" class="direct' + (fld === "parent" ? " on" : "") + '" data-pflip="1">' +
      escape_(T("only what is directly inside")) + "</button></div>";
  }

  /* A folder's two buttons, in words; pressed when the query holds it so. */
  function placeActsHtml(path, scope) {
    const p = untrailed(path);
    const added = scopeHolds(scope, p), out = scope.outside.some((h) => h.path === p);
    return '<span class="da">' +
      '<button type="button" class="inc' + (added ? " on" : "") + '" data-padd="' + escape_(p) + '" title="' + escape_(T("Include folder")) + '">' + escape_(T("Add")) + "</button>" +
      '<button type="button" class="exc' + (out ? " on" : "") + '" data-pexcl="' + escape_(p) + '" title="' + escape_(T("Exclude folder")) + '">' + escape_(T("Exclude")) + "</button></span>";
  }

  function placeRightHtml(active, scope, on) {
    const u = LENS.usage;
    const dirs = LENS.items.filter((it) => it.dir);
    const maxBytes = Math.max(1, ...dirs.map((it) => it.bytes));
    const here = '<div class="lp-dir' + (on.here ? " on" : "") + '" data-pchoose="' + escape_(active) + '">' +
      '<span class="dn">✓ ' + escape_(T("this folder")) + " (" + escape_(homeForm(active)) + ")</span>" +
      '<span class="dm">' + (u && u.root && u.root.bytes ? escape_(bytes(u.root.bytes)) : "") + "</span>" +
      placeActsHtml(active, scope) + "</div>";
    const list = !u && !LENS.tree
      ? '<div class="lgroup">' + escape_(T("reading the folder…")) + "</div>"
      : !dirs.length
      ? '<div class="lgroup">' + escape_(T("no folder here by that name")) + "</div>"
      : dirs.map((it) => {
          const pct = it.bytes ? Math.max(3, Math.round(92 * it.bytes / maxBytes)) : 0;
          const meta = it.bytes ? bytes(it.bytes) : (it.files ? fmt(it.files) : "");
          return '<div class="lp-dir' + (on.dir === it.dir ? " on" : "") + '" data-pdrill="' + escape_(it.dir) + '">' +
            (pct ? '<span class="wbar" style="width:' + pct + '%"></span>' : "") +
            '<span class="dn">' + escape_(it.label) + "</span>" +
            '<span class="dm">' + escape_(meta) + "</span>" + placeActsHtml(it.dir, scope) + "</div>";
        }).join("");
    return '<div class="lp-right">' + placeCrumbHtml(active) + here +
      '<div class="lp-sec">' + escape_(T("Subfolders")) + "</div>" + list + "</div>";
  }

  /* "Searching in:": `spots` in their order, each with its ×; or everywhere. */
  function placeBasketHtml(tiles, scope) {
    const out = spots(q.value, HOME, tiles).map((sp, n) =>
      '<span class="lp-chip' + (sp.not ? " no" : "") + '" title="' + escape_(sp.paths.map(homeForm).join("  ")) + '">' +
      (sp.kind ? '<i class="dot" style="background:var(--disk-' + sp.kind + ')"></i>' : "") +
      '<span class="t">' + escape_(sp.name) + '</span><button type="button" data-prm="' + n +
      '" aria-label="' + escape_(T("Remove")) + '">×</button></span>');
    const disks = tiles.filter((tl) => tl.count !== null).length;
    const every = scope.within.length ? "" : '<span class="every">' + escape_(T("everywhere")) + "</span>" +
      (disks ? '<span class="lbl">' + escape_(T("{n} disks", { n: fmt(disks) })) + "</span>" : "");
    return '<div class="lp-basket"><span class="lbl">' + escape_(T("Searching in:")) + "</span>" + every + out.join("") + "</div>";
  }

