  /* ---------------- the size panel ----------------
   * Five presets, each with its own `count` — there is no size facet — and
   * the choice in words. A press writes the preset, Alt leaves it out, and
   * pressing the one held takes it away. */

  /* Each preset's count query, and the held term's own. */
  function sizeAsks() {
    const t = panelTerm();
    const held = q.value.slice(t.start, t.end);
    const own = held.replace(/^!?[^:]*:/, "") || isEmptyTerm(q.value, t) ? withTerm(withoutField(q.value, "size"), held) : "";
    return { presets: sizeCounts(q.value), own };
  }

  function sizePanelFill() {
    const { presets, own } = sizeAsks();
    if (READS.test(presets[0])) return;
    for (const whole of own ? [...presets, own] : presets) {
      panelAsk("panel count " + whole, () => SERVICE.get("count", { q: whole, cap: COUNT_ALL }).then((r) => r.total));
    }
  }

  function sizePanelHtml() {
    const t = panelTerm();
    const { presets, own } = sizeAsks();
    const reads = READS.test(presets[0]);
    const held = sizePresetOf(q.value, t);
    const kb = panelZone("presets", SIZE_PRESETS.length);
    const chips = SIZE_PRESETS.map((p, i) => ({
      label: T(p.label), n: reads ? undefined : panelHeard("panel count " + presets[i]), title: p.term,
      on: held === i && !t.not, no: held === i && t.not,
    }));
    const value = q.value.slice(t.start, t.end).replace(/^!?[^:]*:/, "");
    const words = held !== null ? T(SIZE_PRESETS[held].label) : value;
    return '<div class="pp">' +
      '<div class="lhead"><b>' + escape_(T("Size")) + "</b><span>" +
      escape_(reads ? T("reads files") : T("results it would leave")) + "</span></div>" +
      panelChipsHtml(chips, "presets", kb) +
      (own && words ? panelSayHtml(words, reads ? undefined : panelHeard("panel count " + own), t.not) : "") +
      '<div class="lfoot">' + escape_(T("Or type one: >10mb, <1kb, 1mb..50mb")) + "</div></div>";
  }

  function sizePress(i, exclude) {
    const t = panelTerm();
    const same = sizePresetOf(q.value, t) === i && t.not === exclude;
    panelReplace(same ? "" : sizePresetTerm({ not: exclude }, i), "size");
  }

