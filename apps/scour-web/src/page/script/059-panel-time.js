  /* ---------------- the time panel ----------------
   * Presets with their counts, a scale and a Between / Before / After
   * switch, the histogram of the query's ages on that scale, its two ends,
   * and the choice in words. One `facets` call an opening and a scale: the
   * presets' edges and the scale's, one walk of every matching row. */

  function timePanelOpen(t) {
    const pick = t.value ? timePick(t.value) : null;
    const s = pick && pick.selected;
    Object.assign(PANEL, { scale: s ? s.scale : "day", mode: s ? s.mode : "between", drag: null, anchor: null });
  }

  const timeKeyOf = () => "panel age " + PANEL.scale + " " + timeAsk(q.value);

  function timePanelFill() {
    const ask = timeAsk(q.value);
    if (READS.test(ask)) return;
    const scale = PANEL.scale;
    panelAsk(timeKeyOf(), () => SERVICE.get("facets", {
      q: ask, by: "age,age", edges: PRESET_EDGES.join(",") + ";" + scaleEdges(scale).join(","),
    }).then((r) => {
      // The presets are the same on every scale: kept while another is asked.
      lensSeen.set("panel presets " + ask, facetPairs(r.groups[0]));
      return { presets: facetPairs(r.groups[0]), bars: facetPairs(r.groups[1]), total: r.total, now: Math.floor(Date.now() / 1000) };
    }));
    timeCountFill();
  }

  /* The count of a choice the bars cannot show: one `count`, once. */
  function timeCountFill() {
    const t = panelTerm();
    if (!t.value || timeShown(t) || READS.test(q.value)) return;
    const whole = withTerm(timeAsk(q.value), q.value.slice(t.start, t.end));
    panelAsk("panel count " + whole, () => SERVICE.get("count", { q: whole, cap: COUNT_ALL }).then((r) => r.total));
  }

  /* The run of bars the term is on the shown scale, or null. */
  function timeShown(t) {
    const pick = t.value ? timePick(t.value) : null;
    return pick && pick.selected && pick.selected.scale === PANEL.scale ? pick.selected : null;
  }

  /* A run from bar `a` to bar `b` in the switch's mode: Before reaches the
     oldest bar, After now. */
  function timeRun(a, b) {
    const n = TIME_SCALE[PANEL.scale].bars, lo = Math.min(a, b), hi = Math.max(a, b);
    const [first, last] = PANEL.mode === "before" ? [0, hi] : PANEL.mode === "after" ? [lo, n - 1] : [lo, hi];
    return { scale: PANEL.scale, mode: PANEL.mode, first, last };
  }

  const localDay = (at) => Math.floor((at + localOffset(at)) / DAY);

  function timePanelHtml() {
    const t = panelTerm();
    const data = panelHeard(timeKeyOf());
    const reads = READS.test(timeAsk(q.value));
    const pick = t.value ? timePick(t.value) : null;
    const presets = data ? data.presets : panelHeard("panel presets " + timeAsk(q.value));
    const counts = presets ? presetCounts(presets) : null;
    const pkb = panelZone("presets", TIME_PRESETS.length);
    const chips = TIME_PRESETS.map((p, i) => ({
      label: T(p.label), n: reads ? undefined : counts ? counts[i] : null,
      on: !!pick && pick.preset === i && !t.not, no: !!pick && pick.preset === i && t.not,
    }));
    const skb = panelZone("switch", TIME_SCALES.length + TIME_MODES.length);
    const seg = (label, i, on) => '<button type="button" tabindex="-1" data-i="' + i + '" class="' +
      (on ? "on" : "") + (skb(i) ? " kb" : "") + '">' + escape_(T(label)) + "</button>";
    const switches = '<div class="pp-row" data-zone="switch"><span class="segs">' +
      TIME_SCALES.map((s, i) => seg(TIME_SCALE[s].label, i, PANEL.scale === s)).join("") + '</span><span class="segs">' +
      TIME_MODES.map((m, j) => seg(MODE_LABEL[m], TIME_SCALES.length + j, PANEL.mode === m)).join("") + "</span></div>";
    const hkb = panelZone("hist", 1);
    const d = PANEL.drag;
    const run = d ? timeRun(d.a, d.b) : timeShown(t);
    return '<div class="pp">' +
      '<div class="lhead"><b>' + escape_(T("When it last changed")) + "</b><span>" +
      escape_(reads ? T("reads files") : T("results it would leave")) + "</span></div>" +
      panelChipsHtml(chips, "presets", pkb) + switches + timeHistHtml(data, run, hkb(0)) +
      timeSayHtml(t, data, run, d) +
      '<div class="lfoot pp-keys"><span>' + escape_(T("← → move the newer end, Shift the older")) + "</span><span>" +
      escape_(T("Or type one: 24h, 2w, 2026-03-01, >2026-06-01")) + "</span></div></div>";
  }

  /* The bars, oldest first, and the dates their ends fall on. */
  function timeHistHtml(data, run, kb) {
    const at = data ? data.now : now;
    const h = histogram(PANEL.scale, data ? data.bars : [], at);
    const form = TIME_SCALE[PANEL.scale].ends;
    const said = (day, f) => dateSaid({ day, form: f || form });
    const bars = h.bars.map((b, i) => {
      const inRun = run && i >= run.first && i <= run.last;
      const height = b.count ? Math.max(6, Math.round(100 * b.count / Math.max(1, h.peak))) : 2;
      const from = localDay(b.from), to = localDay(b.to - 1);
      const when = from === to ? said(from, "DayMonth") : said(from) + " – " + said(to);
      return '<i class="' + (inRun ? "in" : "") + (data ? "" : " wait") + '" style="height:' + height + '%" title="' +
        escape_(when + (data ? " · " + fmt(b.count) : "")) + '"></i>';
    }).join("");
    // The run as a band behind its bars, seen where they are too low to.
    const n = h.bars.length;
    const band = run ? '<span class="run" style="left:' + (100 * run.first / n).toFixed(3) + "%;width:" +
      (100 * (run.last - run.first + 1) / n).toFixed(3) + '%"></span>' : "";
    return '<div class="hist' + (kb ? " kb" : "") + '" data-zone="hist">' + band + bars + "</div>" +
      '<div class="hl"><span>' + escape_(said(localDay(h.from))) + "</span><span>" + escape_(said(localDay(h.to))) + "</span></div>";
  }

  /* The choice in words, and its count from the bars when they hold it. */
  function timeSayHtml(t, data, run, dragging) {
    const term = dragging ? timeTerm(t, run) : q.value.slice(t.start, t.end);
    const v = term.replace(/^!?[^:]*:/, "");
    if (!v) return panelSayHtml("", null);
    const words = timeWords(timeSaid(v, now, localOffset(now))) || v;
    let n = null;
    if (data && run) {
      const h = histogram(PANEL.scale, data.bars, data.now);
      n = h.bars.slice(run.first, run.last + 1).reduce((s, b) => s + b.count, 0) + (run.mode === "before" ? h.older : 0);
      if (term.startsWith("!")) n = data.total - n;
    } else if (!dragging) {
      n = panelHeard("panel count " + withTerm(timeAsk(q.value), term));
    }
    return panelSayHtml(words, n, term.startsWith("!"));
  }

  function timeWrite(sel) {
    const t = panelTerm();
    panelReplace(sel ? timeTerm(t, sel) : "", "dm");
  }

  function timePress(zone, i, exclude) {
    const t = panelTerm();
    if (zone === "presets") {
      const pick = t.value ? timePick(t.value) : null;
      const same = pick && pick.preset === i && t.not === exclude;
      const next = same ? "" : timePresetTerm({ not: exclude }, i);
      const s = !same && timePick(TIME_PRESETS[i].value).selected;
      if (s && s.scale !== PANEL.scale) PANEL.scale = s.scale;
      PANEL.anchor = null;
      panelReplace(next, "dm");
      return;
    }
    if (zone !== "switch") return;
    if (i < TIME_SCALES.length) {
      PANEL.scale = TIME_SCALES[i];
      PANEL.anchor = null;
      lensFill();
      return;
    }
    const was = timeShown(t);
    PANEL.mode = TIME_MODES[i - TIME_SCALES.length];
    if (!was) { lensDraw(); return; }
    // Before is what is older than the run, After the run and what is newer;
    // between the two they turn about the same edge.
    const n = TIME_SCALE[PANEL.scale].bars;
    const m = PANEL.mode;
    let first = was.first, last = was.last;
    if (m === "before") [first, last] = [0, was.mode === "before" ? was.last : was.first - 1];
    if (m === "after") [first, last] = [was.mode === "before" ? was.last + 1 : was.first, n - 1];
    if (last < 0 || first > n - 1) { lensDraw(); return; }
    timeWrite({ scale: PANEL.scale, mode: m, first, last });
  }

  /* ← and → move the newer end of the run, with Shift the older; Before has
     only a newer end and After only an older. */
  function timeKey(d, older) {
    const n = TIME_SCALE[PANEL.scale].bars;
    const was = timeShown(panelTerm());
    // Nothing chosen yet: the newest bar, or for Before the one under it.
    let first = was ? was.first : n - 1, last = was ? was.last : PANEL.mode === "before" ? n - 2 : n - 1;
    if (was) {
      const moveOlder = PANEL.mode === "after" || (PANEL.mode === "between" && older);
      if (moveOlder) first = Math.max(0, Math.min(n - 1, first + d));
      else last = Math.max(0, Math.min(n - 1, last + d));
      if (first > last) moveOlder ? (last = first) : (first = last);
    }
    PANEL.anchor = null;
    timeWrite(timeRun(first, last));
  }

  /* The bar under a point of the screen, the ends held past either side. */
  function timeBarAt(x) {
    const el = lensBox.querySelector(".hist");
    if (!el) return null;
    const r = el.getBoundingClientRect(), cs = getComputedStyle(el);
    const pl = parseFloat(cs.paddingLeft) || 0, pr = parseFloat(cs.paddingRight) || 0;
    const n = el.querySelectorAll("i").length;
    return Math.max(0, Math.min(n - 1, Math.floor((x - r.left - pl) / Math.max(1, r.width - pl - pr) * n)));
  }

  /* Drag across the bars, or click one and then another: the run between.
     The pointer is held by the lens, so a drag that leaves it still ends. */
  lensBox.addEventListener("pointerdown", (e) => {
    if (!PANEL || PANEL.name !== "time" || e.button !== 0 || !e.target.closest(".hist")) return;
    e.preventDefault();
    const i = timeBarAt(e.clientX);
    if (i === null) return;
    PANEL.drag = { a: i, b: i, moved: false };
    lensBox.setPointerCapture(e.pointerId);
    lensDraw();
  });
  lensBox.addEventListener("pointermove", (e) => {
    const d = PANEL && PANEL.drag;
    if (!d) return;
    const i = timeBarAt(e.clientX);
    if (i === null || i === d.b) return;
    d.b = i;
    d.moved = true;
    lensDraw();
  });
  lensBox.addEventListener("pointerup", () => {
    const d = PANEL && PANEL.drag;
    if (!d) return;
    PANEL.drag = null;
    let [a, b] = [d.a, d.b];
    if (!d.moved && PANEL.anchor !== null && PANEL.anchor !== a) [a, b, PANEL.anchor] = [PANEL.anchor, a, null];
    else PANEL.anchor = d.moved ? null : a;
    timeWrite(timeRun(a, b));
  });
  lensBox.addEventListener("pointercancel", () => { if (PANEL && PANEL.drag) { PANEL.drag = null; lensDraw(); } });

