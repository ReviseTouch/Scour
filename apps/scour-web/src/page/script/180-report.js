// ---------------------------------------------------------------- REPORT
// Every number here comes from the index, through `Request::Usage`.
(function () {
  /* By name, so an unhanded reference throws here and not in a `catch`. */
  const SERVICE = window.SCOUR.service;
  const escape_ = window.SCOUR.escape;
  const home = window.SCOUR.home;
  const kinds = window.SCOUR.kinds;
  const barEdges = window.SCOUR.barEdges;
  /* Wrappers rather than values, so a language change is picked up here. */
  const T = window.SCOUR.t;
  const fmt = window.SCOUR.fmt;
  /* The reader's own spelling of a moment, from the formatter built once. */
  const stamp = window.SCOUR.stamp;

  /* The six age bands, resolved at draw time: this runs before the catalogue
     arrives. Named `msgid:`, which is what the catalogue test reads. */
  const AGE = [
    { msgid: "today",      c: "--t0" }, { msgid: "this week", c: "--t1" },
    { msgid: "this month", c: "--t2" }, { msgid: "six months", c: "--t3" },
    { msgid: "this year",  c: "--t4" }, { msgid: "older",     c: "--t5" },
  ];

  const bytes = (n) => {
    const u = ["B","KB","MB","GB","TB"]; let v = n, i = 0;
    while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
    return (i ? v.toFixed(1) : v) + " " + u[i];
  };

  /* `Intl` places the sign, which is the language's business. Built per call,
     unlike the other half's: a kept formatter belongs to a language, and 168
     constructions a redraw is 2.0 ms against a 96 ms walk. */
  const percent = (v, digits) => {
    const dp = digits === undefined ? (v < 1 ? 1 : 0) : digits;
    return new Intl.NumberFormat(document.documentElement.lang, {
      style: "percent", minimumFractionDigits: dp, maximumFractionDigits: dp,
    }).format(v / 100);
  };
  const leafOf = (p) => p.slice(p.lastIndexOf("/") + 1) || p;
  /* `~` for the reader's home, read at call time: `HOME` arrives later. */
  const shorten = (p) => {
    const h = home();
    return h && p.startsWith(h + "/") ? "~" + p.slice(h.length) : p;
  };

  /* Where the report is looking; empty is everything, as `Usage` reads it. */
  let scope = "";
  let drawing = 0;

  /* How many folders get a row, how many kinds get a slice, how many files get
     a bar. The rest of each is one folded line, never dropped. */
  const TOP = 12;
  const KEPT_KINDS = 6;
  const BIG_FILES = 6;

  const crumb = document.getElementById("crumb");
  const kidsEl = document.getElementById("r-kids");

  /* Read at draw time rather than tracked: this tab borrows the search half's
   * element and writes to it only through the clear button, which dispatches
   * `input`. A filter makes the rollup about a third faster, not free. */
  const qBox = document.getElementById("q");
  const filter = () => qBox.value.trim();

  /* ---- the numbers, mirrored from `scour-chart` ----
   * A drift test runs these under node against the crate's own answers.
   * `BigInt` because the crate sums in `u128`. */

  /* Percentages to `decimals` places that sum to exactly 100 — the largest
     remainders take the units rounding left over, so a legend never says 99.9. */
  function shares(values, decimals) {
    const total = values.reduce((a, v) => a + BigInt(v), 0n);
    if (total === 0n) return values.map(() => 0);
    const scale = 10n ** BigInt(decimals);
    const whole = 100n * scale;
    const units = values.map((v) => (BigInt(v) * whole) / total);
    const over = values.map((v, i) => [(BigInt(v) * whole) % total, i]);
    /* The largest remainder first; on a tie the earlier value, which is the
       larger one when the caller has sorted. */
    over.sort((a, b) => (a[0] === b[0] ? a[1] - b[1] : a[0] < b[0] ? 1 : -1));
    let short = whole - units.reduce((a, u) => a + u, 0n);
    for (let i = 0; short > 0n && i < over.length; i++, short--) units[over[i][1]] += 1n;
    return units.map((u) => Number(u) / Number(scale));
  }

  /* A bar cut in the order given; exact fractions, not rounded, so widths meet. */
  function segments(values) {
    const total = values.reduce((a, v) => a + v, 0);
    let at = 0;
    return values.map((v) => {
      const width = total === 0 ? 0 : v / total;
      const seg = { start: at, width };
      at += width;
      return seg;
    });
  }

  /* Keep the `keep` largest by `value`, fold the others into one rest. Stable:
     two equal values keep the order they came in. */
  function fold(items, value, keep) {
    const sorted = items.slice().sort((a, b) => {
      const d = value(b) - value(a);
      return d === 0 ? 0 : d > 0 ? 1 : -1;
    });
    if (sorted.length <= keep) return { kept: sorted, rest: null };
    const others = sorted.slice(keep);
    return {
      kept: sorted.slice(0, keep),
      rest: { count: others.length, value: others.reduce((a, x) => a + value(x), 0) },
    };
  }

  /* A slice drawn as a dash of a stroked circle; the offset is negative and
     cumulative, and the circle is rotated to start at twelve o'clock. */
  function dashes(values, circumference) {
    return segments(values).map((s) => ({
      length: s.width * circumference,
      offset: -(s.start * circumference),
    }));
  }

  /* ---- what those numbers are painted as ---- */

  /* A segment narrower than this cannot hold its own label. Measured against
     the bar's width after layout, so a narrow window drops words, not bars. */
  const LABEL_PX = 64;

  /* Darkest step first; everything past the sixth shares the lightest, and the
     folded remainder is the one colour that is not on the scale. */
  function barColour(i, restAt) {
    return i === restAt ? "--kx" : "--k" + Math.min(i, 5);
  }

  /* One bar cut by `values`. `label` and `tip` return escaped text or ""; a
     label is drawn only where `room` — the bar's own pixels — leaves space. */
  function stripHtml(values, colour, label, tip, room) {
    return segments(values).map((s, i) => {
      const pct = s.width * 100;
      if (pct < 0.05) return "";
      const text = label ? label(i) : "";
      const inside = text && s.width * room >= LABEL_PX ? `<span>${text}</span>` : "";
      const title = tip ? ` title="${tip(i)}"` : "";
      return `<i style="width:${pct.toFixed(3)}%;background:var(${colour(i)})"${title}>${inside}</i>`;
    }).join("");
  }

  /* The age of one folder's bytes, as the column beside its name. No labels:
     the column is too narrow for a word, and the legend above carries them. */
  function ageStrip(age) {
    const pct = shares(age, 0);
    return stripHtml(age, (i) => AGE[i].c, null,
      (i) => escape_(T(AGE[i].msgid) + " · " + percent(pct[i], 0)), 0);
  }

  /* The rows under the folder bar, header included. `rest` is what got no row
     of its own; a filter passes null for it — the answer is then about the
     matching files, not about what the folder holds. */
  function folderRows(kids, rest, pcts) {
    const top = Math.max(1, ...kids.map((v) => v.bytes));
    const head = [["Folder", ""], ["By age", ""], ["Size", " r"], ["Share", " r"], ["Files", " r"]];
    let html = head.map(([w, r]) => `<div class="hd${r}">${escape_(T(w))}</div>`).join("");
    html += kids.map((v, i) =>
      `<div class="nm"><i style="width:${((v.bytes / top) * 100).toFixed(1)}%"></i>` +
        `<button type="button" data-path="${escape_(v.path)}">${escape_(leafOf(v.path))}` +
        `<span class="go">›</span></button></div>` +
      `<div class="strip">${ageStrip(v.age)}</div>` +
      `<div class="r big">${escape_(bytes(v.bytes))}</div>` +
      `<div class="r mute">${escape_(percent(pcts[i], 1))}</div>` +
      `<div class="r mute">${fmt(v.files)}</div>`).join("");
    if (rest && rest.value > 0) {
      const said = rest.count
        ? T("the other {n} folders and the files here", { n: fmt(rest.count) })
        : T("the files here");
      html += `<div class="nm mute">${escape_(said)}</div><div></div>` +
        `<div class="r mute">${escape_(bytes(rest.value))}</div>` +
        `<div class="r mute">${escape_(percent(pcts[kids.length], 1))}</div><div></div>`;
    }
    return html;
  }

  /* The kind ring: r=54 under a 22px stroke inside a 150px box, rotated so the
     first slice starts at twelve o'clock. One dash of the circle a slice. */
  const RING_R = 54;
  function ringHtml(rows, restAt, middle, word) {
    const c = 2 * Math.PI * RING_R;
    const slices = dashes(rows.map((r) => r.count), c).map((d, i) =>
      `<circle cx="75" cy="75" r="${RING_R}" stroke="var(${barColour(i, restAt)})"` +
      ` stroke-dasharray="${d.length.toFixed(2)} ${c.toFixed(2)}"` +
      ` stroke-dashoffset="${d.offset.toFixed(2)}"` +
      (rows[i].key ? ` data-kind="${escape_(rows[i].key)}"` : "") +
      `><title>${escape_(rows[i].label)}</title></circle>`).join("");
    return `<svg width="150" height="150" viewBox="0 0 150 150" role="img"` +
      ` aria-label="${escape_(T("By kind"))}">` +
      `<g transform="rotate(-90 75 75)" fill="none" stroke-width="22">${slices}</g>` +
      `<text class="mid" x="75" y="72" text-anchor="middle">${escape_(middle)}</text>` +
      `<text class="mid2" x="75" y="88" text-anchor="middle">${escape_(word)}</text></svg>`;
  }

  /* Its legend, in the ring's own order. A row with no key is the folded rest,
     which nothing can be searched for. */
  function legendHtml(rows, restAt, pcts) {
    return rows.map((r, i) =>
      `<button type="button"${r.key ? ` data-kind="${escape_(r.key)}"` : " disabled"}>` +
      `<i style="background:var(${barColour(i, restAt)})"></i>` +
      `<span class="w">${escape_(r.label)}</span>` +
      `<span class="n">${fmt(r.count)}</span>` +
      `<span class="p">${escape_(percent(pcts[i], 1))}</span></button>`).join("");
  }

  function breadcrumb() {
    const parts = scope.split("/").filter(Boolean);
    const all = escape_(T("everything"));
    const home = `<button data-path="">${all}</button>`;
    const where = !parts.length ? `<span>${all}</span>` : home + parts.map((seg, i) => {
      const path = "/" + parts.slice(0, i + 1).join("/");
      const last = i === parts.length - 1;
      /* A folder may be named `<img onerror=…>`: everything here is escaped. */
      const label = last ? `<span>${escape_(seg)}</span>`
        : `<button data-path="${escape_(path)}">${escape_(seg)}</button>`;
      return `<span class="s">/</span>` + label;
    }).join("");
    /* Beside the scope, and escaped: a query is text somebody typed. */
    const f = filter();
    return where + (f
      ? `<span class="filt">${escape_(T("filter"))} <b>${escape_(f)}</b>
           <button id="unfilter" title="${escape_(T("remove the filter"))}"
                   aria-label="${escape_(T("remove the filter"))}">×</button>
         </span>`
      : "");
  }

  async function drawReport() {
    const mine = ++drawing;
    crumb.innerHTML = breadcrumb();
    const q = filter();
    let res;
    try {
      res = await SERVICE.usage(scope, TOP, q);
    } catch (e) {
      if (mine !== drawing) return;
      console.error("scour: the report did not load:", e);
      document.getElementById("r-bytes").textContent = "—";
      document.getElementById("r-meta").textContent = T("the service cannot be reached");
      document.getElementById("r-ms").textContent = "";
      kidsEl.innerHTML = "";
      return;
    }
    if (mine !== drawing) return;          /* a newer folder won */

    const n = res.root;
    document.getElementById("r-bytes").textContent = bytes(n.bytes);
    /* `disk` beside `bytes`: the logical size is not disk usage. */
    document.getElementById("r-meta").innerHTML =
      T("<b>{files}</b> files · <b>{disk}</b> on disk · {n} folders",
        { files: fmt(n.files), disk: escape_(bytes(n.disk)), n: fmt(res.child_count) });
    const ms = res.took_us / 1000;
    document.getElementById("r-ms").textContent =
      (ms >= 10 ? fmt(Math.round(ms)) : ms.toFixed(1)) + " ms";

    /* With a filter on, every number here is the matching files, not the folder. */
    const only = document.getElementById("r-only");
    only.hidden = !q;
    if (q) only.innerHTML = T("only the <b>matching</b> files · not the whole folder");

    /* The bars are as wide as the report, so one measurement serves both.
       Read after layout: a label is drawn only where it fits. */
    const ageBar = document.getElementById("r-age");
    const room = ageBar.clientWidth;

    const agePct = shares(n.age, 0);
    const ageWord = (i) => T(AGE[i].msgid);
    ageBar.innerHTML = stripHtml(n.age, (i) => AGE[i].c,
      (i) => escape_(ageWord(i) + " " + percent(agePct[i], 0)),
      (i) => escape_(ageWord(i) + " · " + percent(agePct[i], 0)), room);
    document.getElementById("agekey").innerHTML = AGE.map((band, i) =>
      `<span><i style="background:var(${band.c})"></i>${escape_(T(band.msgid))}` +
      ` <b>${escape_(percent(agePct[i], 0))}</b></span>`).join("");

    /* Every child is in the answer, since a gap in the rollup's tree strands
       bytes in a grandparent; the empty ones are dropped here. */
    const kids = q ? res.children.filter((c) => c.files > 0) : res.children;
    document.getElementById("r-cut").textContent = q
      ? (res.child_count
          ? T("{shown} of {total} folders have a match",
              { shown: fmt(kids.length), total: fmt(res.child_count) })
          : "")
      : res.child_count > kids.length
        ? T("the heaviest {shown} of {total} folders",
            { shown: fmt(kids.length), total: fmt(res.child_count) })
        : "";

    /* What the shown folders leave over: the folders with no row of their own
       and the files lying directly here. Never negative. */
    const rest = {
      count: Math.max(0, res.child_count - kids.length),
      value: Math.max(0, n.bytes - kids.reduce((a, c) => a + c.bytes, 0)),
    };
    const weights = kids.map((c) => c.bytes).concat(rest.value);
    const restAt = kids.length;
    /* Two roundings of one set: whole percent inside the bar, a tenth in the
       rows. Both from `shares`, so both add up to a hundred. */
    const wide = shares(weights, 0);
    const exact = shares(weights, 1);
    document.getElementById("r-folders").innerHTML = stripHtml(weights,
      (i) => barColour(i, restAt),
      (i) => (i === restAt ? "" : escape_(leafOf(kids[i].path) + " " + percent(wide[i], 0))),
      (i) => escape_(i === restAt
        ? T("the other {n} folders and the files here", { n: fmt(rest.count) })
        : leafOf(kids[i].path) + " · " + bytes(kids[i].bytes) + " · " + percent(wide[i], 0)),
      room);

    /* Already heaviest-first from the engine, which is what `top` cuts on. */
    kidsEl.innerHTML = kids.length
      ? folderRows(kids, q ? null : rest, exact)
      : `<div class="hollow">${escape_(q
          ? T("In this scope, none of the subfolders has a matching file.")
          : T("There are no further folders to show under this one."))}</div>`;

    drawKinds();
    drawBiggest();
    drawJump();
    // Only if opened: drawing this tab must not start a hunt unasked.
    if (window.SCOUR.prefs().dupes_open !== dupesOpen) {
      showDupes(!!window.SCOUR.prefs().dupes_open, false);
    }
    if (dupesOpen) drawDupes(0);
  }

  /* From the sidebar's own faceting, filter included: a panel beside a filtered
   * headline must not count something else. The age edges travel with it because
   * that is what lifts the 200,000-row sampling cap, so the counts are exact. */
  async function drawKinds() {
    const q = ((scope ? `under:${scope} ` : "") + filter()).trim();
    let res;
    try {
      res = await SERVICE.sidebar(q, barEdges());
    } catch (e) {
      console.error("scour: the kind panel did not load:", e);
      return;
    }
    const g = (res.groups || []).find((x) => x.by === "kind");
    const all = (g ? g.facets : []).filter((f) => f.count > 0);
    const few = fold(all, (x) => x.count, KEPT_KINDS);
    const rows = few.kept.map((x) => ({
      key: x.key, count: x.count,
      label: (kinds().find(([, t]) => t === x.key) || [x.key])[0],
    }));
    if (few.rest) {
      rows.push({ key: "", count: few.rest.value,
                  label: T("the other {n} kinds", { n: fmt(few.rest.count) }) });
    }
    const restAt = few.rest ? rows.length - 1 : -1;
    const pcts = shares(rows.map((r) => r.count), 1);
    /* The middle says how many kinds there are, not how many slices: the
       folded ones are kinds too. */
    document.getElementById("k-ring").innerHTML = rows.length
      ? ringHtml(rows, restAt, fmt(all.length),
                 T("kinds").toLocaleUpperCase(document.documentElement.lang))
      : "";
    document.getElementById("p-kind").innerHTML = legendHtml(rows, restAt, pcts);
    /* Said only when the service says the walk stopped short. */
    document.getElementById("k-cap").textContent =
      res.capped ? T("{n} sampled rows", { n: fmt(res.total) }) : "";
  }

  /* An ordinary search: the index sorts by size without walking. */
  async function drawBiggest() {
    const el = document.getElementById("p-big");
    const q = ((scope ? `under:${scope} ` : "") + "file: " + filter()).trim();
    let res;
    try {
      res = await SERVICE.search(q, "size", true, BIG_FILES, 0);
    } catch (e) {
      console.error("scour: the largest-files panel did not load:", e);
      return;
    }
    const top = Math.max(1, ...res.rows.map((r) => r.size));
    el.innerHTML = res.rows.map((r) => {
      const path = r.dir + "/" + r.name;
      /* The folder after the name, and muted: under this scope it is the part
         below it, elsewhere the reader's own spelling of the path. */
      const where = r.dir === scope ? ""
        : scope && r.dir.startsWith(scope + "/") ? r.dir.slice(scope.length + 1)
        : shorten(r.dir);
      return `<button type="button" data-file="${escape_(path)}" data-size="${r.size}"` +
        ` title="${escape_(shorten(path))}">` +
        `<i style="width:${((r.size / top) * 100).toFixed(1)}%"></i>` +
        `<span class="nm">${escape_(r.name)}` +
        (where ? `<small>${escape_(where)}</small>` : "") + `</span>` +
        `<span class="v">${escape_(bytes(r.size))}</span></button>`;
    }).join("") || `<p class="note">—</p>`;
  }

  /* The same menu the search list opens, on the same code path. */
  const bigEl = document.getElementById("p-big");
  function fileMenu(e, el) {
    /* Any click on the document closes the menu, so this one must not reach
       it: the menu it opens would shut in the same tick. */
    e.stopPropagation();
    window.SCOUR.rowMenu(e, el.dataset.file,
      { is_dir: false, size: +el.dataset.size || 0 }, () => drawBiggest());
  }
  for (const what of ["contextmenu", "click"]) {
    bigEl.addEventListener(what, (e) => {
      const el = e.target.closest("[data-file]");
      if (el) fileMenu(e, el);
    });
  }

  /* A kind, from the ring or from its legend, into the search with this scope. */
  for (const id of ["k-ring", "p-kind"]) {
    document.getElementById(id).addEventListener("click", (e) => {
      const el = e.target.closest("[data-kind]");
      if (el) toSearch("kind:" + el.dataset.kind);
    });
  }

  /* Two numbers, always both: what shares a size against what was read end to
   * end, 39.16 GiB against 4.16 GiB here. Nothing is read unasked. */
  let dupeRun = 0;
  /* The whole speed knob: a unique size rules out only 6.2% of files, so
     below a megabyte the candidates are a million files covering 28 GB. */
  const FLOORS = [[1, "1 MB"], [10, "10 MB"], [100, "100 MB"], [1024, "1 GB"]];
  let dupeFloor = 10;
  /* Copies shown before the rest are asked for: a group of fifty would
     otherwise swallow the panel. */
  const FIRST = 6;
  /* What the panel last asked for, so a redraw after a move asks the same. */
  let dupeBudget = 0;
  /* The last answer, largest saving first, and what is drawn over it. */
  let dupeGroups = [];
  let dupeFacts = { unconfirmed: 0, read: 0 };
  /* Which copy to keep, and which groups show every path. Keyed by the
     group's own paths, so a group that lost one is a new group and its mark
     goes back to the newest — which is what it should be after a move. */
  const dupeKept = new Map();
  const dupeAll = new Set();
  /* One group at a time: files are moving, and a second press is not a queue. */
  let sweeping = "";

  /* What deleting all but one gives back, from the group's own numbers: this
     is the figure the reader is looking at, not a total the service capped. */
  const reclaim = (g) => g.size * Math.max(0, g.paths.length - 1);
  const keyOf = (g) => g.size + "|" + g.paths.join("|");

  /* The newest copy is the one worth keeping. A time of 0 is "not stated" —
     see `TIMED` in `dupes.rs` — and loses to anything that was. */
  const newest = (g) => {
    let at = -1, best = g.paths[0] || "";
    g.paths.forEach((p, i) => {
      const t = (g.mtimes && g.mtimes[i]) || 0;
      if (t > at) { at = t; best = p; }
    });
    return best;
  };
  const keptOf = (g) => {
    const held = dupeKept.get(keyOf(g));
    return held && g.paths.includes(held) ? held : newest(g);
  };

  async function drawDupes(budgetMb) {
    const mine = ++dupeRun;
    const sum = document.getElementById("d-sum");
    const btn = document.getElementById("d-confirm");
    dupeBudget = budgetMb || 0;
    sum.textContent = budgetMb ? T("reading…") : "…";
    btn.disabled = true;
    let res;
    try {
      res = await SERVICE.dupes(scope, budgetMb, dupeFloor);
    } catch (e) {
      if (mine !== dupeRun) return;
      console.error("scour: the duplicates panel did not load:", e);
      sum.textContent = T("the service cannot be reached");
      document.getElementById("d-list").textContent = "";
      btn.disabled = false;
      return;
    }
    if (mine !== dupeRun) return;
    btn.disabled = false;
    dupeGroups = res.groups.slice().sort((a, b) => reclaim(b) - reclaim(a));
    dupeFacts = { unconfirmed: res.unconfirmed, read: res.read };
    paintDupes();
  }

  /* Draws what `drawDupes` fetched. A mark, an expansion and a finished move
     all come back through here; only a move asks the service again. */
  function paintDupes() {
    const groups = dupeGroups;
    const total = groups.reduce((n, g) => n + reclaim(g), 0);
    const sure = groups.filter((g) => g.certainty === "content").length;
    /* Said plainly: a candidate by size is not a duplicate. */
    const how = !groups.length ? ""
      : sure === groups.length ? " · " + T("all confirmed identical by reading")
      : sure ? " · " + T("{n} confirmed identical, the rest share a size only", { n: fmt(sure) })
      : " · " + T("candidates by size — nothing was read");
    document.getElementById("d-sum").textContent =
      T("{n} groups · {size} could be freed", { n: fmt(groups.length), size: bytes(total) }) +
      how +
      (dupeFacts.read ? " · " + T("{n} read", { n: bytes(dupeFacts.read) }) : "") +
      /* No filter, and it says so: narrowing hides half of a pair. */
      (filter() ? " · " + T("independent of the filter") : "");
    /* Never dropped: a partial answer that looks complete deletes files. */
    document.getElementById("d-note").innerHTML = dupeFacts.unconfirmed
      ? T("Being the same size is not being the same file. <b>{n} groups were not confirmed by reading</b> — those are candidates, not duplicates.", { n: fmt(dupeFacts.unconfirmed) })
        + " " + T("Nothing should be deleted before it is confirmed.")
      : T("All of them were compared by reading.");

    /* The engine's three certainties; the keys name the CSS class. */
    const word = {
      content: T("identical"), edges: T("same ends"), size: T("same size only"),
    };
    document.getElementById("d-list").innerHTML = groups.map((g) => {
      const key = keyOf(g);
      const kept = keptOf(g);
      const all = dupeAll.has(key);
      const head = g.paths.slice(0, FIRST);
      /* The kept copy is always on screen: a group whose mark is hidden in
         the tail looks like a group with no mark at all. */
      const shown = all ? g.paths
        : head.includes(kept) ? head : [kept, ...head.slice(0, FIRST - 1)];
      /* By path, not by position: `shown` is not always a prefix. */
      const timeOf = (p) => (g.mtimes && g.mtimes[g.paths.indexOf(p)]) || 0;
      const rows = shown.map((p) => {
        const at = timeOf(p);
        return `<li class="${p === kept ? "kept" : ""}">
          <button type="button" class="mark" data-keep="${escape_(p)}"
            aria-pressed="${p === kept}" aria-label="${escape_(T("Keep this one"))}"
            title="${escape_(T("Keep this one"))}"></button>
          <button type="button" class="path" data-path="${escape_(p)}">${escape_(shorten(p))}</button>
          <span class="when">${escape_(at ? stamp(at) : "—")}</span>
        </li>`;
      }).join("");
      /* Kept once expanded, or there is no way back to the short list. */
      const rest = all || g.paths.length > shown.length
        ? `<li><button type="button" class="rest" data-more="1">${escape_(all
            ? T("show fewer")
            : "… " + T("{n} more", { n: fmt(g.paths.length - shown.length) }))}</button></li>`
        : "";
      /* Only a group read end to end may be swept; the reason is in the tip. */
      const off = g.certainty !== "content" || sweeping === key;
      return `<div class="dupe" data-key="${escape_(key)}">
        <div class="top">
          <span class="save">${bytes(reclaim(g))}</span>
          <span class="of">×${g.paths.length} · ${bytes(g.size)}</span>
          <span class="sure ${g.certainty}">${escape_(word[g.certainty] || g.certainty)}</span>
        </div>
        <ul>${rows}${rest}</ul>
        <div class="deeds"><button type="button" data-sweep="1"${off ? " disabled" : ""}${
          g.certainty === "content" ? "" : ` title="${escape_(T("Confirm by reading first"))}"`
        }>${escape_(T("Move the other {n} to trash", { n: fmt(g.paths.length - 1) }))}</button></div>
      </div>`;
    }).join("") ||
      `<div class="dupe"><div class="top"><span class="of">${escape_(T("no duplicates in this scope"))}</span></div></div>`;
  }

  /* What the last move did. Outside `d-list`, so a redraw does not wipe it. */
  const saidEl = document.getElementById("d-said");
  function say(text) {
    saidEl.textContent = text || "";
    saidEl.hidden = !text;
  }

  const dlist = document.getElementById("d-list");
  const groupAt = (el) => {
    const box = el.closest(".dupe");
    return box ? dupeGroups.find((g) => keyOf(g) === box.dataset.key) : null;
  };

  /* The row menu is the search list's own — one table, one code path — and it
     hands back what to redraw, since this tab cannot see the search rows. */
  function menuFor(e, el) {
    const g = groupAt(el);
    window.SCOUR.rowMenu(e, el.dataset.path,
      g ? { is_dir: false, size: g.size } : null,
      (moved, r) => {
        if (r && r.refused && r.refused.length) {
          say(T("{n} could not be moved", { n: fmt(r.refused.length) }) + " — " + r.refused[0]);
        } else if (r && r.gone !== undefined) {
          say(T("{n} moved to trash", { n: fmt(r.gone) }));
        }
        drawDupes(dupeBudget);
      });
  }

  dlist.addEventListener("contextmenu", (e) => {
    const el = e.target.closest("[data-path]");
    if (el) menuFor(e, el);
  });

  dlist.addEventListener("click", (e) => {
    const mark = e.target.closest("[data-keep]");
    if (mark) {
      const g = groupAt(mark);
      if (g) { dupeKept.set(keyOf(g), mark.dataset.keep); paintDupes(); }
      return;
    }
    if (e.target.closest("[data-more]")) {
      const g = groupAt(e.target);
      if (!g) return;
      const key = keyOf(g);
      if (dupeAll.has(key)) dupeAll.delete(key); else dupeAll.add(key);
      paintDupes();
      return;
    }
    if (e.target.closest("[data-sweep]")) { sweep(groupAt(e.target)); return; }
    const path = e.target.closest("[data-path]");
    if (path) {
      /* Any click on the document closes the menu, so this one must not
         reach it: the menu it opens would shut in the same tick. */
      e.stopPropagation();
      menuFor(e, path);
    }
  });

  /* Paths travel in the request line, which is cut past 16 KB. Whole paths
     only, 6,000 characters of them a request. */
  function batches(paths) {
    const out = [];
    let now = [], len = 0;
    for (const p of paths) {
      const cost = encodeURIComponent(p).length + 3;
      if (now.length && len + cost > 6000) { out.push(now); now = []; len = 0; }
      now.push(p);
      len += cost;
    }
    if (now.length) out.push(now);
    return out;
  }

  /* The one press here that moves files. The button is disabled for a group
     nothing was read of; this asks again, because a redraw is not a fence. */
  async function sweep(g) {
    if (!g || sweeping || g.certainty !== "content") return;
    const key = keyOf(g);
    const kept = keptOf(g);
    const others = g.paths.filter((p) => p !== kept);
    if (!others.length) return;
    if (!confirm(T("Move {n} files to trash? The one under {path} stays.",
                   { n: fmt(others.length), path: shorten(kept) }))) return;
    sweeping = key;
    paintDupes();
    let gone = 0;
    const refused = [];
    for (const batch of batches(others)) {
      try {
        const r = await SERVICE.trash(batch);
        gone += r.gone || 0;
        if (r.refused) refused.push(...r.refused);
      } catch (err) {
        /* One batch failing is not the rest of them failing. */
        refused.push(...batch.map((p) => p + ": " + (err.message || err)));
      }
    }
    sweeping = "";
    say(refused.length
      ? T("{gone} moved to trash · {n} stayed: {which}",
          { gone: fmt(gone), n: fmt(refused.length), which: refused.slice(0, 3).join(" · ") })
      : T("{n} moved to trash", { n: fmt(gone) }));
    drawDupes(dupeBudget);
  }

  /* Remembered by the service, so every face starts from the same answer. */
  let dupesOpen = false;
  const dupeParts = ["d-sum", "d-floor", "d-confirm", "d-note", "d-list"];
  function showDupes(open, ask) {
    dupesOpen = open;
    const opener = document.getElementById("d-open");
    opener.textContent = open ? T("hide") : T("show");
    opener.setAttribute("aria-expanded", String(open));
    for (const id of dupeParts) {
      const el = document.getElementById(id);
      if (el) el.hidden = !open;
    }
    // A closed section says nothing about a move it no longer shows.
    if (!open) say("");
    if (open && ask) drawDupes(0);
  }
  showDupes(false, false);
  document.getElementById("d-open").addEventListener("click", () => {
    showDupes(!dupesOpen, true);
    window.SCOUR.pref({ dupes_open: dupesOpen });
  });

  const floorBox = document.getElementById("d-floor");
  function drawFloors() {
    floorBox.innerHTML = FLOORS.map(([mb, label]) =>
      `<button type="button" data-mb="${mb}" aria-pressed="${mb === dupeFloor}"
        title="${escape_(T("do not look at anything smaller than this"))}">${label}+</button>`).join("");
  }
  floorBox.addEventListener("click", (e) => {
    const b = e.target.closest("[data-mb]");
    if (!b) return;
    dupeFloor = +b.dataset.mb;
    drawFloors();
    // Free again: a floor change is a different set of files.
    drawDupes(0);
  });
  drawFloors();

  document.getElementById("d-confirm").onclick = () => drawDupes(2048);

  /* Into the search tab with this scope, and with `extra` in front of whatever
     was typed. A `kind:` of its own replaces one already in the query, or the
     two would fight; `under:` always does. */
  function toSearch(extra) {
    const box = document.getElementById("q");
    let rest = box.value.replace(/under:\S+\s*/g, "");
    if (extra) rest = rest.replace(/kind:\S+\s*/g, "");
    const parts = [];
    if (scope) parts.push("under:" + scope);
    if (extra) parts.push(extra);
    if (rest.trim()) parts.push(rest.trim());
    box.value = parts.join(" ");
    box.dispatchEvent(new Event("input"));
    show("search");
  }

  function drawJump() {
    document.getElementById("p-jump").innerHTML =
      `<div class="line"><span>under:</span><span style="font-size:10.5px">${escape_(scope || "/")}</span></div>` +
      `<button class="facet" style="padding-left:0;margin-top:6px" id="jump">
         <span class="dot" style="background:var(--t1)"></span>
         <span class="nm">${escape_(T("Search in this scope"))}</span></button>`;
    document.getElementById("jump").onclick = () => toSearch("");
  }

  document.querySelector(".report").addEventListener("click", (e) => {
    const b = e.target.closest("button[data-path]");
    if (b) { scope = b.dataset.path; drawReport(); }
    /* Through `input`: the search tab listens on the same element. */
    if (e.target.closest("#unfilter")) {
      qBox.value = "";
      qBox.dispatchEvent(new Event("input"));
    }
  });

  /* Held off a quarter second: this is a roll-up over every matching row.
   * Nothing while the search tab is up — `show("report")` reads the box. */
  let waiting = 0;
  qBox.addEventListener("input", () => {
    if (document.getElementById("view-report").hidden) return;
    clearTimeout(waiting);
    waiting = setTimeout(drawReport, 250);
  });

  function show(which) {
    const s = which === "search";
    document.getElementById("view-search").hidden = !s;
    document.getElementById("view-report").hidden = s;
    document.getElementById("tab-search").setAttribute("aria-selected", String(s));
    document.getElementById("tab-report").setAttribute("aria-selected", String(!s));
    document.querySelector(".meter").style.display = s ? "" : "none";
    window.SCOUR.drawTabs();
    if (s) document.dispatchEvent(new Event("scour-search-shown"));
    else drawReport();
  }
  document.getElementById("tab-search").onclick = () => show("search");
  document.getElementById("tab-report").onclick = () => show("report");

  /* `show("report")` redraws the tab when it opens, so only what no later
   * draw rewrites is done here: the floor titles and the show/hide word. */
  window.SCOUR.relabel = () => {
    drawFloors();
    showDupes(dupesOpen, false);
  };
})();

