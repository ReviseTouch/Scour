  /* ---------------- the lens ----------------
   * What the term under the caret can become, in a short list under it, each
   * choice with the number of results it would leave. It never takes the
   * keyboard: the arrows choose, Tab writes and stays, Enter writes and
   * closes, Escape closes. A choice is a replacement of one term, so the
   * query stays the one thing the window holds. `scour_ui::lens` is the same
   * list for the window; the two are kept alike by hand. */
  const lensBox = document.getElementById("lens");
  let LENS = null;
  let lensGen = 0, lensStop = null;
  /* Answers already had this time the lens was open: a folder walked once is
     not walked again on the way back up. */
  const lensSeen = new Map();

  const LENS_FIELDS = {
    under: "under", in: "under", altinda: "under",
    parent: "parent", child: "parent", children: "parent",
    dm: "dm", modified: "dm", size: "size",
    kind: "kind", type: "kind", tur: "kind", "t\u00fcr": "kind",
    ext: "ext", content: "content", text: "content", icerik: "content", "i\u00e7erik": "content",
    fuzzy: "fuzzy", typo: "fuzzy", benzer: "fuzzy", letters: "letters", harfler: "letters",
  };
  const LENS_LAUNCH = [
    ["under:", "Location", "which disk, which folder"],
    ["dm:", "Modified", "when it last changed"],
    ["kind:", "Type", "document, code, image…"],
    ["ext:", "Extension", "pdf, rs, docx…"],
    ["size:", "Size", "how big"],
    ["fuzzy:", "Similar spelling", "one letter off still matches"],
    ["content:", "Inside", "looks inside the file, not at its name"],
  ];
  const LENS_TIMES = [
    ["today", "today"], ["yesterday", "yesterday"], ["7d", "last 7 days"],
    ["30d", "last 30 days"], ["1y", "last year"],
  ];
  const LENS_SIZES = [
    ["<100kb", "under 100 KB"], ["100kb..10mb", "100 KB to 10 MB"],
    ["10mb..1gb", "10 MB to 1 GB"], [">1gb", "over 1 GB"],
  ];
  /* Counting a query with `content:` in it would start reading files — with
     something in it: an empty `content:""` is left out of the search. */
  const READS = /(^|[\s|(])!?(content|text|icerik|i\u00e7erik):(?!\s|$|[|)]|"\s*("|$))/i;
  const COUNT_CAP = 100000;

  /* The term the caret is in or just after, read as `scour_ui::lens::term`. */
  function lensTerm(text, caret) {
    const hit = terms(text).find(([a, b]) => caret > a && caret <= b);
    if (!hit) return null;
    const [a, b] = hit;
    const raw = text.slice(a, b);
    const not = raw.startsWith("!");
    const body = not ? raw.slice(1) : raw;
    const sp = body.startsWith('"') ? null : fieldSplit(body);
    const f = sp ? LENS_FIELDS[fold(sp[0])] || null : null;
    const value = (sp ? sp[1] : body).replace(/^"|"$/g, "");
    return { a, b, raw, not, field: f, named: !!sp, value };
  }

  function lensKindOf(t) {
    if (/^:[^:]*$/.test(t.raw)) return "launch";
    switch (t.field) {
      case "under": case "parent": return "place";
      case "dm": return "time";
      case "size": return "size";
      case "kind": return "kind";
      case "ext": return "ext";
      case "content": return "content";
      case "fuzzy": case "letters": return "word";
    }
    if (t.named) return null;
    return [...t.value].length >= 2 ? "word" : null;
  }

  const quoteIf = (v) => (/\s/.test(v) ? '"' + v + '"' : v);
  const neg = (t) => (t.not ? "!" : "");
  /* A path as a person writes it: under the home folder, with `~`. */
  const homeForm = (p) => (HOME && (p === HOME || p.startsWith(HOME + "/")) ? "~" + p.slice(HOME.length) : p);
  const fullForm = (p) => (HOME && (p === "~" || p.startsWith("~/")) ? HOME + p.slice(1) : p);
  function placeParts(v) {
    const i = v.lastIndexOf("/");
    if (i < 0) return ["", v];
    if (i === 0) return ["/", v.slice(1)];
    return [v.slice(0, i), v.slice(i + 1)];
  }

  function lensClose() {
    LENS = null;
    lensGen++;
    if (lensStop) lensStop.abort();
    lensBox.hidden = true;
    lensBox.innerHTML = "";
    q.setAttribute("aria-expanded", String(!hintBox.hidden));
  }

  /* Open, follow or close the lens for where the caret now is. `asked` is a
     click on the term or Ctrl+Space; otherwise a lens opens by itself only
     at the end of a filter being typed, never for a plain word. */
  function lensSync(asked) {
    if (document.activeElement !== q || q.selectionStart !== q.selectionEnd) { lensClose(); return; }
    const caret = q.selectionStart;
    const t = lensTerm(q.value, caret);
    const kind = t && lensKindOf(t);
    const keep = LENS && t && LENS.a === t.a;
    if (!kind || !(asked || keep || (kind !== "word" && caret === t.b))) { lensClose(); return; }
    const sel = keep && LENS.kind === kind ? LENS.sel : 0;
    LENS = { kind, a: t.a, b: t.b, term: t, sel, items: LENS?.items || [], title: "", foot: "" };
    hideHints();
    lensFill();
  }

  /* `:` on its own, or the + in the box: the launcher at the caret. */
  function lensLaunch() {
    q.focus();
    // A `:` already waiting is opened, not joined by another.
    const open = terms(q.value).find(([a, b]) => /^:[^:]*$/.test(q.value.slice(a, b)));
    if (open) {
      q.setSelectionRange(open[1], open[1]);
      lensSync(true);
      return;
    }
    const c = q.selectionStart ?? q.value.length;
    const v = q.value;
    const lead = c > 0 && !/\s/.test(v[c - 1]) ? " " : "";
    const tail = c < v.length && !/\s/.test(v[c]) ? " " : "";
    q.value = v.slice(0, c) + lead + ":" + tail + v.slice(c);
    q.setSelectionRange(c + lead.length + 1, c + lead.length + 1);
    render();
    lensSync(true);
  }

  function lensItems() {
    const t = LENS.term;
    const L = (label, hint, term, more) => Object.assign({ label: T(label), hint, term, n: null }, more || {});
    switch (LENS.kind) {
      case "launch": {
        const want = fold(t.raw.slice(1));
        LENS.title = T("Add a filter");
        LENS.foot = T("Type a few letters to narrow it. Enter starts the filter.");
        return LENS_LAUNCH
          .filter(([f, l]) => !want || f.startsWith(want) || fold(T(l)).startsWith(want))
          .map(([f, l, about]) => L(l, T(about), f, { launch: true }));
      }
      case "time":
        LENS.title = T("When it last changed");
        LENS.foot = T("Or type one: 24h, 2w, 2026-03-01, >2026-06-01");
        return LENS_TIMES.map(([v, l]) => L(l, v, neg(t) + "dm:" + v, { count: true }));
      case "size":
        LENS.title = T("Size");
        LENS.foot = T("Or type one: >10mb, <1kb, 1mb..50mb");
        return LENS_SIZES.map(([v, l]) => L(l, v, neg(t) + "size:" + v, { count: true }));
      case "word": {
        const v = t.value;
        LENS.title = T("How “{w}” is read", { w: v });
        LENS.foot = T("Only this word changes. Every word at once: Settings → Searching.");
        const out = [L("as typed", "", neg(t) + quoteIf(v), { count: true })];
        if (!/\s/.test(v)) {
          out.push(L("a letter off still matches", "fuzzy:", neg(t) + "fuzzy:" + v, { count: true }));
          out.push(L("these letters in this order", "letters:", neg(t) + "letters:" + v, { count: true }));
        }
        out.push(L("inside the files", "content:", neg(t) + 'content:"' + v + '"', { reads: true }));
        out.push(L(t.not ? "stop excluding it" : "exclude it", "!", (t.not ? "" : "!") + quoteIf(v), { count: true }));
        return out;
      }
      case "content": {
        const v = t.value;
        LENS.title = T("Inside the files");
        LENS.foot = T("Only the files the rest of the query names are read: narrow the place and the type first.");
        return [
          L("inside the files", "content:", neg(t) + 'content:"' + v + '"', { reads: true }),
          L("in names instead", "", neg(t) + quoteIf(v), { count: true }),
        ];
      }
      case "kind": case "ext": {
        LENS.title = LENS.kind === "kind" ? T("Type") : T("Extension");
        LENS.foot = T("→ adds or takes one away and stays open; Enter writes it; Tab leaves the term.");
        return lensListItems(LENS.facets || []);
      }
      case "place": return lensPlaceItems();
    }
    return [];
  }

  /* A list field's values, each pressing in or out of what is written. */
  function lensListItems(facets) {
    const t = LENS.term;
    const field = LENS.kind;
    const have = t.value.toLowerCase().split(";").filter(Boolean);
    let values = facets.map((f) => [f.key, f.count]);
    if (field === "kind" && !values.length) values = KIND_VALUES.map((k) => [k, null]);
    const known = new Set(values.map(([k]) => k));
    const typed = have.length && !known.has(have[have.length - 1]) ? have[have.length - 1] : "";
    return values
      .filter(([k]) => !typed || k.startsWith(typed))
      .slice(0, 12)
      .map(([k, n]) => {
        const on = have.includes(k);
        const next = have.filter((h) => known.has(h) && h !== k);
        if (!on) next.push(k);
        const label = field === "kind" ? (KIND_KEY[k] || k) : "." + k;
        return { label: (on ? "✓ " : "") + label, hint: field === "kind" ? k : "", term: neg(t) + field + ":" + next.join(";"), n, stays: true };
      });
  }

  /* Folders: the disks and the usual places at the top, then a folder's own
     folders, heaviest in what this query finds first. */
  function lensPlaceItems() {
    const t = LENS.term;
    const field = t.field || "under";
    const [base, part] = placeParts(t.value);
    const want = fold(part);
    LENS.foot = T("→ goes into a folder, Backspace climbs back out; Enter chooses it; Tab leaves the term.");
    if (!base) {
      LENS.title = T("Where");
      const out = [];
      for (const d of LENS.disks || []) {
        const root = d.volume.roots[0] || "";
        const name = d.volume.label || root;
        if (want && !fold(name).startsWith(want) && !fold(root).includes(want)) continue;
        const on = ["auto", "chosen", "configured"].includes(d.state);
        out.push(on
          ? { label: name, hint: homeForm(root), term: neg(t) + field + ":" + quoteIf(homeForm(root)), dir: root, stays: true, group: out.length ? "" : T("Disks") }
          : { label: name, hint: T("not indexed — choose to index it"), disk: d.volume.id, group: out.length ? "" : T("Disks") });
      }
      let first = true;
      for (const [label, path] of SCOPES) {
        if (want && !fold(label).startsWith(want) && !fold(homeForm(path)).includes(want)) continue;
        out.push({ label, hint: homeForm(path), term: neg(t) + field + ":" + quoteIf(homeForm(path)), dir: path, stays: true, group: first ? T("Places") : "" });
        first = false;
      }
      return out;
    }
    const out = [];
    const u = LENS.usage;
    LENS.title = homeForm(fullForm(base)) + (u && u.child_count > u.children.length
      ? " · " + T("the {n} heaviest of {all}", { n: fmt(u.children.length), all: fmt(u.child_count) }) : "");
    if (!part) {
      out.push({ label: T("this folder"), hint: u && u.root.bytes ? bytes(u.root.bytes) : "", term: neg(t) + field + ":" + quoteIf(base), n: u ? u.root.files : null });
    }
    /* Where this query finds the most first; then the heaviest; then by name. */
    const kids = (u ? u.children.slice() : []).sort((x, y) =>
      (y.files - x.files) || (y.bytes - x.bytes) || x.path.localeCompare(y.path));
    for (const c of kids) {
      const name = c.path.slice(c.path.lastIndexOf("/") + 1);
      if (want && !fold(name).includes(want)) continue;
      out.push({ label: name + "/", hint: c.bytes ? bytes(c.bytes) : "", term: neg(t) + field + ":" + quoteIf(homeForm(c.path)), dir: c.path, n: c.files, stays: true });
    }
    if (u && !part) {
      const other = field === "under" ? "parent" : "under";
      out.push({ label: T(field === "under" ? "only what is directly inside" : "everything below"), hint: other + ":", term: neg(t) + other + ":" + quoteIf(base), group: " " });
    }
    if (!u) out.push({ label: T("reading the folder…"), hint: "", wait: true });
    else if (!out.length) out.push({ label: T("no folder here by that name"), hint: "", wait: true });
    return out;
  }

  /* Fill the list now, then ask the service for what it lacks: the counts,
     the folder's own folders, the disks. One answer at a time per lens. */
  function lensFill() {
    if (!LENS) return;
    const mine = ++lensGen;
    if (lensStop) lensStop.abort();
    lensStop = new AbortController();
    const signal = lensStop.signal;
    const t = LENS.term;
    const rest = (q.value.slice(0, t.a) + q.value.slice(t.b)).replace(/\s+/g, " ").trim();
    const live = () => LENS && mine === lensGen;
    LENS.items = lensItems();
    lensDraw();
    const cache = (key, ask) => {
      if (lensSeen.has(key)) return Promise.resolve(lensSeen.get(key));
      return ask().then((r) => { lensSeen.set(key, r); return r; });
    };
    const again = () => { if (live()) { LENS.items = lensItems(); lensDraw(); lensCounts(rest, mine, signal); } };
    if (LENS.kind === "kind" || LENS.kind === "ext") {
      if (!READS.test(rest)) {
        cache("facets " + LENS.kind + " " + rest, () => SERVICE.get("facets", { q: rest, by: LENS.kind }, signal))
          .then((r) => { if (live()) { LENS.facets = (r.groups[0] || {}).facets || []; again(); } })
          .catch(() => {});
      }
      return;
    }
    if (LENS.kind === "place") {
      const [base] = placeParts(t.value);
      const activeBase = base ? fullForm(base) : (HOME || "/");
      LENS.placeDir = activeBase;
      const scoped = READS.test(rest) ? "" : rest;
      LENS.disks = lensSeen.get("volumes") || null;
      LENS.usage = lensSeen.get("usage " + activeBase + " " + scoped) || null;
      LENS.tree = lensSeen.get("tree " + activeBase) || null;
      again();
      if (!LENS.disks) {
        cache("volumes", () => SERVICE.volumes().then((r) => r.disks || []))
          .then((d) => { if (live()) { LENS.disks = d; again(); } })
          .catch(() => {});
      }
      if (!LENS.usage) {
        cache("usage " + activeBase + " " + scoped, () => SERVICE.get("usage", { path: activeBase, top: 60, q: scoped }, signal))
          .then((r) => { if (live()) { LENS.usage = r; again(); } })
          .catch(() => { if (live()) { LENS.usage = { root: { bytes: 0, files: null }, children: [] }; again(); } });
      }
      if (!LENS.tree) {
        cache("tree " + activeBase, () => SERVICE.tree(activeBase, signal).then((r) => r.dirs || []))
          .then((d) => { if (live()) { LENS.tree = d; again(); } })
          .catch(() => { if (live()) { LENS.tree = []; again(); } });
      }
      return;
    }
    lensCounts(rest, mine, signal);
  }

  /* Each choice's number: the query with that term in place of this one. */
  function lensCounts(rest, mine, signal) {
    if (READS.test(rest)) return;
    for (const it of LENS.items) {
      if (!it.count) continue;
      const whole = (rest ? rest + " " : "") + it.term;
      const key = "count " + whole;
      const done = (n) => {
        if (!LENS || mine !== lensGen) return;
        it.n = n;
        lensDraw();
      };
      if (lensSeen.has(key)) { done(lensSeen.get(key)); continue; }
      SERVICE.get("count", { q: whole, cap: COUNT_CAP }, signal)
        .then((r) => { const n = r.capped ? -r.total : r.total; lensSeen.set(key, n); done(n); })
        .catch(() => {});
    }
  }

  function lensWriteRaw(text, keepOpen, caretIn) {
    if (!LENS) return;
    const { a, b } = LENS;
    const v = q.value;
    const tail = !keepOpen && !/\s/.test(v[b] || "") ? " " : "";
    q.value = v.slice(0, a) + text + tail + v.slice(b);
    const caret = caretIn ?? (a + text.length + tail.length);
    q.setSelectionRange(caret, caret);
    render();
    if (keepOpen) lensSync(true); else lensClose();
  }

  function lensPlaceHtml() {
    const t = LENS.term;
    const fld = t.field || "under";
    const [base, part] = placeParts(t.value);
    const active = LENS.placeDir || (base ? fullForm(base) : (HOME || "/"));
    const want = fold(part);
    const disks = LENS.disks || [];
    const diskHtml = disks.map((d) => {
      const root = d.volume.roots[0] || "";
      const name = d.volume.label || root || d.volume.id;
      const on = ["auto", "chosen", "configured"].includes(d.state);
      const act = on && (active === root || active.startsWith(root === "/" ? "/" : root + "/"));
      return on
        ? '<button type="button" class="lp-item' + (act ? " active" : "") + '" data-pdir="' + escape_(root) + '">' +
          '<span class="nm">⛁ ' + escape_(name) + '</span><span class="sub">' + escape_(homeForm(root)) + "</span></button>"
        : '<button type="button" class="lp-item" data-pdisk="' + escape_(d.volume.id) + '">' +
          '<span class="nm">⛁ ' + escape_(name) + '</span><span class="ask-tag">' + escape_(T("Index it")) + "</span></button>";
    }).join("");
    const placeRows = (HOME ? [["~", HOME], ...SCOPES] : SCOPES).map(([label, path]) => {
      const act = active === path;
      return '<button type="button" class="lp-item' + (act ? " active" : "") + '" data-pdir="' + escape_(path) + '">' +
        '<span class="nm">' + escape_(label) + '</span><span class="sub">' + escape_(homeForm(path)) + "</span></button>";
    }).join("");

    // Breadcrumb segments for the right column
    const shown = homeForm(active);
    const segs = [];
    if (shown.startsWith("~")) {
      segs.push(["~", HOME]);
      let cur = HOME;
      for (const p of shown.slice(1).split("/").filter(Boolean)) { cur += "/" + p; segs.push([p, cur]); }
    } else {
      segs.push(["/", "/"]);
      let cur = "";
      for (const p of active.split("/").filter(Boolean)) { cur += "/" + p; segs.push([p, cur]); }
    }
    const parentPath = active === "/" ? null : (active.lastIndexOf("/") <= 0 ? "/" : active.slice(0, active.lastIndexOf("/")));
    const crumbHtml =
      (parentPath ? '<button type="button" data-pdir="' + escape_(parentPath) + '" title="' + escape_(T("Climb up")) + '">‹</button>' : "") +
      segs.map(([lbl, p], idx) =>
        (idx ? '<span class="s">/</span>' : "") +
        '<button type="button" class="' + (idx + 1 === segs.length ? "cur" : "") + '" data-pdir="' + escape_(p) + '">' + escape_(lbl) + "</button>"
      ).join("") +
      '<button type="button" class="direct' + (fld === "parent" ? " on" : "") + '" data-pflip="1">' +
      escape_(T("only what is directly inside")) + "</button>";

    // Merge Usage weights + Tree subdirectories so empty/unweighed folders still appear
    const byPath = new Map();
    const u = LENS.usage;
    for (const c of (u && u.children) || []) {
      byPath.set(c.path, { path: c.path, name: c.path.slice(c.path.lastIndexOf("/") + 1), bytes: c.bytes || 0, files: c.files || 0 });
    }
    for (const d of LENS.tree || []) {
      if (!byPath.has(d.path)) byPath.set(d.path, { path: d.path, name: d.name, bytes: 0, files: d.children || 0 });
    }
    const dirs = [...byPath.values()]
      .filter((d) => !want || fold(d.name).includes(want))
      .sort((x, y) => (y.bytes - x.bytes) || (y.files - x.files) || x.name.localeCompare(y.name))
      .slice(0, 60);
    const maxBytes = Math.max(1, ...dirs.map((d) => d.bytes || 0));
    const applyHere = '<div class="lp-dir" data-pchoose="' + escape_(active) + '">' +
      '<span class="dn">✓ ' + escape_(T("this folder")) + " (" + escape_(shown) + ')</span>' +
      '<span class="dm">' + (u && u.root && u.root.bytes ? escape_(bytes(u.root.bytes)) : "") + "</span>" +
      '<span class="da">' +
      '<button type="button" class="inc" data-pinc="' + escape_(active) + '" title="' + escape_(T("Include folder")) + '">+</button>' +
      '<button type="button" class="exc" data-pexc="' + escape_(active) + '" title="' + escape_(T("Exclude folder")) + '">!</button>' +
      "</span></div>";
    const listHtml = !u && !LENS.tree
      ? '<div class="lgroup">' + escape_(T("reading the folder…")) + "</div>"
      : !dirs.length
      ? '<div class="lgroup">' + escape_(T("no folder here by that name")) + "</div>"
      : dirs.map((d) => {
          const pct = d.bytes ? Math.max(3, Math.round(92 * d.bytes / maxBytes)) : 0;
          const meta = d.bytes ? bytes(d.bytes) : (d.files ? fmt(d.files) : "");
          return '<div class="lp-dir" data-pdrill="' + escape_(d.path) + '">' +
            (pct ? '<span class="wbar" style="width:' + pct + '%"></span>' : "") +
            '<span class="dn">' + escape_(d.name) + '/</span>' +
            '<span class="dm">' + escape_(meta) + '</span>' +
            '<span class="da">' +
            '<button type="button" class="inc" data-pinc="' + escape_(d.path) + '" title="' + escape_(T("Include folder")) + '">+</button>' +
            '<button type="button" class="exc" data-pexc="' + escape_(d.path) + '" title="' + escape_(T("Exclude folder")) + '">!</button>' +
            "</span></div>";
        }).join("");

    return '<div class="lp-grid">' +
      '<div class="lp-left">' +
      (diskHtml ? '<div class="lp-sec">' + escape_(T("Disks")) + "</div>" + diskHtml : "") +
      '<div class="lp-sec">' + escape_(T("Places")) + "</div>" + placeRows +
      '<button type="button" class="lp-pick" data-ppick="' + escape_(active) + '">📁 ' + escape_(T("Choose folder…")) + "</button>" +
      "</div>" +
      '<div class="lp-right">' +
      '<div class="lp-crumb">' + crumbHtml + "</div>" +
      applyHere +
      '<div class="lp-sec">' + escape_(T("Subfolders")) + "</div>" +
      listHtml +
      "</div></div>" +
      '<div class="lfoot">' + escape_(T("→ goes into a folder, Backspace climbs back out; Enter chooses it; Tab leaves the term.")) + "</div>";
  }

  function lensSparkHtml(its, peak) {
    if (LENS.kind !== "time" && LENS.kind !== "size") return "";
    const lo = LENS.dragA !== undefined ? Math.min(LENS.dragA, LENS.dragB) : LENS.sel;
    const hi = LENS.dragA !== undefined ? Math.max(LENS.dragA, LENS.dragB) : LENS.sel;
    return '<div class="lspark-wrap">' +
      '<div class="lspark" id="lspark">' +
      its.map((it, i) => {
        const h = it.n ? Math.max(8, Math.round(100 * Math.abs(it.n) / peak)) : 6;
        return '<div class="sbar' + (i >= lo && i <= hi ? " in" : "") + '" data-si="' + i + '">' +
          '<i style="height:' + h + '%"></i></div>';
      }).join("") +
      '</div><div class="lspark-lbls">' +
      its.map((it) => "<span>" + escape_(it.hint || it.label) + "</span>").join("") +
      '</div><div class="lspark-note">' + escape_(T("Click or drag across bars to pick a range")) + "</div></div>";
  }

  function lensWordGlobalHtml() {
    if (LENS.kind !== "word") return "";
    const typos = (KEPT && KEPT.typos) || "";
    const exactAcc = !!(KEPT && KEPT.accents_exact);
    return '<div class="lw-global">' +
      '<span>' + escape_(T("Typing mistakes")) + ': <span class="lw-pills">' +
      '<button type="button" class="' + (typos === "" ? "on" : "") + '" data-wtypos="">' + escape_(T("Offer")) + "</button>" +
      '<button type="button" class="' + (typos === "always" ? "on" : "") + '" data-wtypos="always">' + escape_(T("Always")) + "</button>" +
      '<button type="button" class="' + (typos === "off" ? "on" : "") + '" data-wtypos="off">' + escape_(T("Off")) + "</button>" +
      "</span></span>" +
      '<span>' + escape_(T("Ignore accents")) + ': <span class="lw-pills">' +
      '<button type="button" class="' + (!exactAcc ? "on" : "") + '" data-wacc="0">' + escape_(T("Always")) + "</button>" +
      '<button type="button" class="' + (exactAcc ? "on" : "") + '" data-wacc="1">' + escape_(T("Exact")) + "</button>" +
      "</span></span></div>";
  }

  function lensDraw() {
    if (!LENS || (LENS.kind !== "place" && !LENS.items.length)) { lensBox.hidden = true; return; }
    lensBox.classList.toggle("lens-place", LENS.kind === "place");
    lensBox.classList.toggle("lens-band", LENS.kind === "time" || LENS.kind === "size");
    if (LENS.kind === "place") {
      lensBox.innerHTML = lensPlaceHtml();
    } else {
      const its = LENS.items;
      LENS.sel = Math.max(0, Math.min(LENS.sel, its.length - 1));
      const peak = Math.max(1, ...its.map((i) => Math.abs(i.n || 0)));
      const num = (n) => (n === null || n === undefined ? "" : n < 0 ? fmt(-n) + "+" : fmt(n));
      lensBox.innerHTML =
        '<div class="lhead"><b>' + escape_(LENS.title) + "</b>" +
        (its.some((i) => i.count || i.n !== null && i.n !== undefined) ? "<span>" + escape_(T("results it would leave")) + "</span>" : "") + "</div>" +
        lensSparkHtml(its, peak) +
        its.map((it, i) =>
          (it.group ? '<div class="lgroup">' + escape_(it.group.trim()) + "</div>" : "") +
          '<div class="lopt' + (i === LENS.sel ? " on" : "") + (it.n === 0 ? " zero" : "") + (it.wait ? " wait" : "") +
          '" role="option" data-i="' + i + '" aria-selected="' + (i === LENS.sel) + '">' +
          '<span class="l">' + escape_(it.label) + (it.hint ? "<small>" + escape_(it.hint) + "</small>" : "") + "</span>" +
          (it.reads ? '<span class="r">' + escape_(T("reads files")) + "</span>"
            : it.n === null || it.n === undefined ? (it.count ? '<span class="b"></span><span class="c">…</span>' : "")
            : '<span class="b"><i style="width:' + Math.round(100 * Math.abs(it.n) / peak) + '%"></i></span><span class="c">' + num(it.n) + "</span>") +
          "</div>").join("") +
        lensWordGlobalHtml() +
        '<div class="lfoot">' + escape_(LENS.foot) + "</div>";
    }
    lensBox.hidden = false;
    q.setAttribute("aria-expanded", "true");
    /* Under the term, at its first letter, on the line it ends on. */
    const el = termEl(LENS.a);
    const box = qfield.getBoundingClientRect();
    const rects = el ? [...el.getClientRects()] : [];
    const r = rects[rects.length - 1];
    const left = r ? Math.min(rects[0].left, r.left) - box.left : 0;
    lensBox.style.left = Math.max(-box.left + 12, Math.min(left, innerWidth - box.left - lensBox.offsetWidth - 14)) + "px";
    lensBox.style.top = (r ? r.bottom - box.top + 8 : box.height + 8) + "px";
    const on = lensBox.querySelector(".lopt.on");
    if (on) on.scrollIntoView({ block: "nearest" });
  }

  /* Write a choice. `stay` (Tab) keeps the lens open: a list takes another
     value, a folder is gone into. */
  function lensChoose(i, stay) {
    if (!LENS) return;
    const it = LENS.items[i];
    if (!it || it.wait) return;
    if (it.disk) {
      SERVICE.answerDisk(it.disk, "index")
        .then(() => { lensSeen.delete("volumes"); flash(T("The disk is being indexed.")); lensFill(); })
        .catch((e) => flash(String(e.message || e)));
      return;
    }
    const { a, b } = LENS;
    const v = q.value;
    let text = it.term;
    let caretIn = null;
    // What the files hold is not a filter in the query but the line under
    // the field: the `:` that asked goes, and that line opens.
    if (it.launch && text === "content:") {
      lensClose();
      q.value = v.slice(0, a) + v.slice(b);
      q.setSelectionRange(a, a);
      render();
      setGrepOpen(true);
      return;
    }
    const deeper = stay && it.dir;
    if (deeper) text = text.replace(/"$/, "") + (text.endsWith('"') ? '/"' : "/");
    const keep = it.launch || (stay && (it.stays || deeper));
    const tail = !keep && !/\s/.test(v[b] || "") ? " " : "";
    q.value = v.slice(0, a) + text + tail + v.slice(b);
    const caret = caretIn ?? (a + text.length - (deeper && text.endsWith('"') ? 1 : 0) + tail.length);
    q.setSelectionRange(caret, caret);
    render();
    if (keep) {
      if (LENS && (it.launch || deeper)) LENS.sel = 0;
      lensSync(true);
    } else {
      lensClose();
    }
  }

  /* The launcher shut without a choice: a lone `:` it left goes with it. */
  function dropLauncher() {
    const v = q.value;
    const t = terms(v).find(([a, b]) => v.slice(a, b) === ":");
    lensClose();
    if (!t) return;
    let [a, b] = t;
    if (v[b] === " ") b++; else if (v[a - 1] === " ") a--;
    q.value = v.slice(0, a) + v.slice(b);
    q.setSelectionRange(a, a);
    render();
  }

  let lastQueryCaret = 0, queryPressing = false, clampingSel = false;

  function clampQuerySelection() {
    if (clampingSel) return false;
    const v = q.value;
    const a = q.selectionStart ?? 0, c = q.selectionEnd ?? 0;
    const back = q.selectionDirection === "backward";
    if (a === c) {
      for (const [ta, tb] of terms(v)) {
        const head = capHeadOf(v, ta, tb);
        if (head < 0) continue;
        const entry = head < tb && v[head] === '"' ? head + 1 : head;
        if ((queryPressing ? c >= ta : c > ta) && c < entry) {
          const to = !queryPressing && lastQueryCaret >= entry && c < lastQueryCaret ? ta : entry;
          clampingSel = true;
          q.setSelectionRange(to, to);
          clampingSel = false;
          lastQueryCaret = to;
          return true;
        }
      }
      lastQueryCaret = c;
      return false;
    }
    let lo = a, hi = c;
    for (const [ta, tb] of terms(v)) {
      const head = capHeadOf(v, ta, tb);
      if (head < 0) continue;
      if (lo < head && hi > ta) {
        lo = Math.min(lo, ta);
        hi = Math.max(hi, tb);
      }
    }
    if (lo !== a || hi !== c) {
      clampingSel = true;
      q.setSelectionRange(lo, hi, back ? "backward" : "forward");
      clampingSel = false;
      lastQueryCaret = back ? lo : hi;
      return true;
    }
    lastQueryCaret = back ? a : c;
    return false;
  }

  function frameBackspace() {
    const c = q.selectionStart, v = q.value;
    for (const [ta, tb] of terms(v)) {
      const head = capHeadOf(v, ta, tb);
      if (head < 0) continue;
      const quoted = v[head] === '"';
      if (quoted && c === tb && v[c - 1] === '"' && c - 1 > head) {
        q.setSelectionRange(c - 1, c - 1);
        lastQueryCaret = c - 1;
        paint(v);
        lensSync(true);
        return true;
      }
      if (quoted && c === head + 1 && v[c] === '"') {
        q.value = v.slice(0, head) + v.slice(head + 2);
        q.setSelectionRange(head, head);
        lastQueryCaret = head;
        render();
        return true;
      }
      if (c === head && c === tb) {
        let a = ta, b = tb;
        if (v[b] === " ") b++; else if (v[a - 1] === " ") a--;
        q.value = v.slice(0, a) + v.slice(b);
        q.setSelectionRange(a, a);
        lastQueryCaret = a;
        lensClose();
        render();
        return true;
      }
      const entry = head < tb && quoted ? head + 1 : head;
      if (c > ta && c <= entry) {
        q.setSelectionRange(entry, entry);
        lastQueryCaret = entry;
        return true;
      }
      if (c === ta && c >= 2 && v[c - 1] === " " && !/\s/.test(v[c - 2])) {
        q.setSelectionRange(c - 1, c - 1);
        lastQueryCaret = c - 1;
        paint(v);
        return true;
      }
    }
    return false;
  }

  function frameDelete() {
    const c = q.selectionStart, v = q.value;
    for (const [ta, tb] of terms(v)) {
      const head = capHeadOf(v, ta, tb);
      if (head < 0) continue;
      if (c === ta) {
        let a = ta, b = tb;
        if (v[b] === " ") b++; else if (v[a - 1] === " ") a--;
        q.value = v.slice(0, a) + v.slice(b);
        q.setSelectionRange(a, a);
        lastQueryCaret = a;
        lensClose();
        render();
        return true;
      }
      if (c > ta && c < head) {
        const entry = head < tb && v[head] === '"' ? head + 1 : head;
        q.setSelectionRange(entry, entry);
        lastQueryCaret = entry;
        return true;
      }
      if (c + 1 === ta && v[c] === " " && c > 0 && !/\s/.test(v[c - 1])) {
        q.setSelectionRange(ta, ta);
        lastQueryCaret = ta;
        paint(v);
        return true;
      }
    }
    return false;
  }

  /* Leave the term the caret is at the end of: the lens closes and a space
     follows the term, so the caret stands outside it and what is typed next
     is a new term. True when there was a term to leave. */
  function lensExit() {
    const had = !!LENS;
    lensClose();
    const c = q.selectionStart;
    if (c !== q.selectionEnd) return had;
    const t = lensTerm(q.value, c);
    if (!t || c !== t.b || !/cap/.test(termClass(t.raw))) return had;
    // Left with nothing in it, a filter goes rather than stays empty.
    if (/\bidle\b/.test(termClass(t.raw))) return dropEmpty(null);
    const v = q.value;
    if (!/\s/.test(v[c] || "")) q.value = v.slice(0, c) + " " + v.slice(c);
    q.setSelectionRange(c + 1, c + 1);
    render();
    lensClose();
    return true;
  }

  function sparkRangeTerm(kind, a, b, negPrefix) {
    const lo = Math.min(a, b), hi = Math.max(a, b);
    if (lo === hi) return LENS.items[lo].term;
    if (kind === "time") {
      // Cumulative age bands: picking up to `hi` covers up to that horizon; if `lo > 0`, bound both sides.
      const edges = ["1d", "2d", "7d", "30d", "365d"];
      return negPrefix + "dm:" + edges[hi];
    }
    const loEdge = ["", "100kb", "10mb", "1gb"][lo];
    const hiEdge = ["100kb", "10mb", "1gb", ""][hi];
    const val = loEdge && hiEdge ? loEdge + ".." + hiEdge : loEdge ? ">" + loEdge : "<" + hiEdge;
    return negPrefix + "size:" + val;
  }

  let sparkDrag = false;
  lensBox.addEventListener("pointerdown", (e) => {
    const sb = e.target.closest(".sbar");
    if (!sb || !LENS) return;
    e.preventDefault();
    sparkDrag = true;
    LENS.dragA = LENS.dragB = +sb.dataset.si;
    lensBox.setPointerCapture(e.pointerId);
    lensDraw();
  });
  lensBox.addEventListener("pointermove", (e) => {
    if (!sparkDrag || !LENS) return;
    const el = document.elementFromPoint(e.clientX, e.clientY);
    const sb = el && el.closest && el.closest(".sbar");
    if (sb && +sb.dataset.si !== LENS.dragB) {
      LENS.dragB = +sb.dataset.si;
      lensDraw();
    }
  });
  const finishSpark = () => {
    if (!sparkDrag || !LENS) return;
    sparkDrag = false;
    const term = sparkRangeTerm(LENS.kind, LENS.dragA, LENS.dragB, neg(LENS.term));
    delete LENS.dragA;
    delete LENS.dragB;
    lensWriteRaw(term, false);
  };
  lensBox.addEventListener("pointerup", finishSpark);
  lensBox.addEventListener("pointercancel", () => { sparkDrag = false; if (LENS) { delete LENS.dragA; delete LENS.dragB; } });

  lensBox.addEventListener("mousedown", (e) => {
    if (e.target.closest(".sbar")) { e.preventDefault(); return; }
    e.preventDefault();
    if (!LENS) return;
    const wt = e.target.closest("[data-wtypos]");
    if (wt) {
      const next = wt.dataset.wtypos;
      KEPT = { ...(KEPT || {}), typos: next };
      keep({ typos: next });
      drawTypos();
      lensDraw();
      setTimeout(render, 120);
      return;
    }
    const wa = e.target.closest("[data-wacc]");
    if (wa) {
      const exactAcc = wa.dataset.wacc === "1";
      KEPT = { ...(KEPT || {}), accents_exact: exactAcc };
      keep({ accents_exact: exactAcc });
      lensDraw();
      setTimeout(render, 120);
      return;
    }
    const pdisk = e.target.closest("[data-pdisk]");
    if (pdisk) {
      SERVICE.answerDisk(pdisk.dataset.pdisk, "index")
        .then(() => { lensSeen.delete("volumes"); flash(T("The disk is being indexed.")); lensFill(); })
        .catch((err) => flash(String(err.message || err)));
      return;
    }
    const ppick = e.target.closest("[data-ppick]");
    if (ppick) {
      const savedA = LENS.a, savedB = LENS.b, savedFld = LENS.term.field || "under", savedNeg = neg(LENS.term);
      SERVICE.pickFolder(ppick.dataset.ppick)
        .then((r) => {
          if (!r || !r.path) return;
          const v = q.value;
          const txt = savedNeg + savedFld + ":" + quoteIf(homeForm(r.path)) + " ";
          q.value = v.slice(0, savedA) + txt + v.slice(savedB);
          q.focus();
          q.setSelectionRange(savedA + txt.length, savedA + txt.length);
          render();
          lensClose();
        })
        .catch((err) => flash(String(err.message || err)));
      return;
    }
    const pinc = e.target.closest("[data-pinc]");
    if (pinc) {
      const fld = LENS.term.field || "under";
      lensWriteRaw(fld + ":" + quoteIf(homeForm(pinc.dataset.pinc)), false);
      return;
    }
    const pexc = e.target.closest("[data-pexc]");
    if (pexc) {
      const fld = LENS.term.field || "under";
      lensWriteRaw("!" + fld + ":" + quoteIf(homeForm(pexc.dataset.pexc)), false);
      return;
    }
    const pchoose = e.target.closest("[data-pchoose]");
    if (pchoose) {
      const fld = LENS.term.field || "under";
      lensWriteRaw(neg(LENS.term) + fld + ":" + quoteIf(homeForm(pchoose.dataset.pchoose)), false);
      return;
    }
    const pflip = e.target.closest("[data-pflip]");
    if (pflip) {
      const other = (LENS.term.field || "under") === "under" ? "parent" : "under";
      const cur = LENS.placeDir || HOME || "/";
      const raw = neg(LENS.term) + other + ":" + quoteIf(homeForm(cur === "/" ? "/" : cur + "/"));
      const caret = LENS.a + raw.length - (raw.endsWith('"') ? 1 : 0);
      lensWriteRaw(raw, true, caret);
      return;
    }
    const pdir = e.target.closest("[data-pdir]") || e.target.closest("[data-pdrill]");
    if (pdir) {
      const target = pdir.dataset.pdir || pdir.dataset.pdrill;
      const fld = LENS.term.field || "under";
      const shown = homeForm(target);
      const slash = shown.endsWith("/") ? shown : shown + "/";
      const raw = neg(LENS.term) + fld + ":" + quoteIf(slash);
      const caret = LENS.a + raw.length - (raw.endsWith('"') ? 1 : 0);
      lensWriteRaw(raw, true, caret);
      return;
    }
    const o = e.target.closest(".lopt");
    if (!o) return;
    const it = LENS.items[+o.dataset.i];
    lensChoose(+o.dataset.i, !!(it && (it.stays || it.dir)));
  });

  /* Before every other key handler of the box: while the lens is open the
     arrows, Tab, Enter and Escape are its own. */
  q.addEventListener("keydown", (e) => {
    if (e.ctrlKey && (e.code === "Space" || e.key === " ")) {
      e.preventDefault();
      e.stopImmediatePropagation();
      if (LENS) lensClose();
      else if (lensTerm(q.value, q.selectionStart) && lensKindOf(lensTerm(q.value, q.selectionStart))) lensSync(true);
      else lensLaunch();
      return;
    }
    // Backspace and Delete do not take a capsule's frame apart.
    if (e.key === "Backspace" && !e.ctrlKey && !e.altKey && !e.metaKey &&
        q.selectionStart === q.selectionEnd && frameBackspace()) {
      e.preventDefault();
      e.stopImmediatePropagation();
      return;
    }
    if (e.key === "Delete" && !e.ctrlKey && !e.altKey && !e.metaKey &&
        q.selectionStart === q.selectionEnd && frameDelete()) {
      e.preventDefault();
      e.stopImmediatePropagation();
      return;
    }
    // Tab leaves the term, lens or not: the caret goes outside it.
    if (e.key === "Tab" && !e.shiftKey && !e.ctrlKey && !e.altKey) {
      if (lensExit()) { e.preventDefault(); e.stopImmediatePropagation(); }
      return;
    }
    if (!LENS || lensBox.hidden) return;
    const n = LENS.items.length;
    const step = (d) => {
      let s = LENS.sel;
      for (let k = 0; k < n; k++) { s = (s + d + n) % n; if (!LENS.items[s].wait) break; }
      LENS.sel = s;
      lensDraw();
    };
    if (e.key === "ArrowDown" && !e.altKey) { e.preventDefault(); e.stopImmediatePropagation(); step(1); return; }
    if (e.key === "ArrowUp" && !e.altKey) { e.preventDefault(); e.stopImmediatePropagation(); step(-1); return; }
    // → goes into a folder, or adds one more value to a list, and stays open.
    if (e.key === "ArrowRight" && q.selectionStart === LENS.b) {
      const it = LENS.items[LENS.sel];
      if (it && (it.stays || it.dir)) { e.preventDefault(); e.stopImmediatePropagation(); lensChoose(LENS.sel, true); }
      return;
    }
    if (e.key === "Enter") { e.preventDefault(); e.stopImmediatePropagation(); lensChoose(LENS.sel, false); return; }
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopImmediatePropagation();
      if (LENS.kind === "launch") dropLauncher(); else if (!dropEmpty(null)) lensClose();
    }
  }, true);

  document.getElementById("qadd").addEventListener("mousedown", (e) => {
    e.preventDefault();
    if (LENS && LENS.kind === "launch") { dropLauncher(); return; }
    q.setSelectionRange(q.value.length, q.value.length);
    lensLaunch();
  });
  q.addEventListener("blur", () => setTimeout(() => { if (document.activeElement !== q) lensClose(); }, 0));

