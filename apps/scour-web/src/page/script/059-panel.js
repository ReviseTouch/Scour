  /* ---------------- the panels ----------------
   * The time, type and size lenses: chips with a count each, a histogram
   * for time, tiles for kinds, and a line that says the choice. Drawn first,
   * counted after, from the text; every press writes the text through the
   * models above. Their own state — the scale, which chip the keys are on —
   * lives for one opening. */

  const PANEL_OF = { time: "time", size: "size", kind: "type", ext: "type" };
  let PANEL = null;
  /* A write of the panel's own is under way: the lens stays this opening. */
  let panelHolding = false;

  /* A panel opens on term `t`: its answers are asked afresh, its keys rest. */
  function panelOpen(name, t) {
    for (const k of [...lensSeen.keys()]) if (k.startsWith("panel ")) lensSeen.delete(k);
    PANEL = { name, keys: false, zone: 0, at: 0, zones: [] };
    if (name === "time") timePanelOpen(t);
    if (name === "type") PANEL.zone = LENS.kind === "ext" ? 1 : 0;
  }

  /* The lens's term as the models read it: its value without the spaces of a list. */
  function panelTerm() {
    return placeTerms(q.value).find((x) => x.start === LENS.a) ||
      { start: LENS.a, end: LENS.b, not: LENS.term.not, field: LENS.term.field, value: LENS.term.value };
  }

  /* Write what a press made, the caret at `caret`, and keep the panel open. */
  function panelWrite(text, caret) {
    panelHolding = true;
    try {
      q.value = text;
      q.setSelectionRange(caret, caret);
      render();
      lensSync(true);
    } finally {
      panelHolding = false;
    }
  }

  /* `term` in place of the lens's (`lens::choice_settle`): written as
     nothing, an empty `field:` stands where it was, so the panel stays. */
  function panelSettle(term, field) {
    const [text, caret] = choiceSettle(q.value, panelTerm(), term, field);
    panelWrite(text, caret);
  }

  /* One answer, asked once an opening: `ask` is called only on a miss. Kept
     by its query, so a late one is drawn only where that query still holds. */
  function panelAsk(key, ask) {
    if (lensSeen.has(key)) return;
    lensSeen.set(key, null);
    ask()
      .then((r) => {
        lensSeen.set(key, r);
        if (PANEL) lensDraw();
      })
      .catch(() => { if (lensSeen.get(key) === null) lensSeen.delete(key); });
  }
  const panelHeard = (key) => (lensSeen.has(key) ? lensSeen.get(key) : null);
  /* A facet group's facets as `[key, count]` pairs. */
  const facetPairs = (g) => ((g && g.facets) || []).map((f) => [String(f.key), f.count]);

  function panelFill() {
    if (PANEL.name === "time") timePanelFill();
    else if (PANEL.name === "type") typePanelFill();
    else sizePanelFill();
  }

  function panelHtml() {
    PANEL.zones = [];
    const body = PANEL.name === "time" ? timePanelHtml() : PANEL.name === "type" ? typePanelHtml() : sizePanelHtml();
    PANEL.zone = Math.max(0, Math.min(PANEL.zone, PANEL.zones.length - 1));
    const z = PANEL.zones[PANEL.zone];
    PANEL.at = z ? Math.max(0, Math.min(PANEL.at, z.n - 1)) : 0;
    return body;
  }

  /* A zone the keys move through, in the order drawn; its focus, when there. */
  function panelZone(name, n, cols) {
    PANEL.zones.push({ name, n, cols: cols || 0 });
    const here = PANEL.keys && PANEL.zones.length - 1 === PANEL.zone;
    return (i) => here && i === PANEL.at;
  }

  const panelNum = (n) => (n === null || n === undefined ? "…" : n < 0 ? fmt(-n) + "+" : fmt(n));

  /* A row of chips, each a label and its count: on (ringed), left out (red,
     struck), nothing to count (dimmed, still pressable). */
  function panelChipsHtml(chips, zone, kb, cls) {
    return '<div class="pp-chips' + (cls ? " " + cls : "") + '" data-zone="' + zone + '">' + chips.map((c, i) =>
      '<button type="button" tabindex="-1" class="pc' + (c.on ? " on" : "") + (c.no ? " no" : "") +
      (c.n === 0 ? " zero" : "") + (kb(i) ? " kb" : "") + (c.cls ? " " + c.cls : "") + '" data-i="' + i + '"' +
      (c.title ? ' title="' + escape_(c.title) + '"' : "") + ">" + escape_(c.label) +
      (c.n !== undefined ? "<small>" + escape_(panelNum(c.n)) + "</small>" : "") + "</button>").join("") + "</div>";
  }

  /* The choice in words, struck through when left out, and how many it leaves. */
  function panelSayHtml(words, n, not) {
    if (!words) return '<div class="pp-say quiet">' + escape_(T("Click or drag across bars to pick a range")) + "</div>";
    const span = "\u0000";
    const said = n === null || n === undefined ? span : T("{n} results · {span}", { n: panelNum(n), span });
    return '<div class="pp-say">' + escape_(said).replace(span, (not ? "<s>" : "") + escape_(words) + (not ? "</s>" : "")) + "</div>";
  }

  /* The keys while a panel is open; true when one was the panel's. Up and
     down go into it and between its rows, left and right along a row once
     in, Space and Enter press, Alt+Enter leaves out. */
  function panelKey(e) {
    const z = PANEL.zones[PANEL.zone];
    if (!PANEL.keys) {
      if (e.key === "ArrowDown" && !e.altKey) { PANEL.keys = true; PANEL.at = panelStart(z); lensDraw(); return true; }
      if (e.key === "Enter") { lensExit(); return true; }
      return false;
    }
    if (!z) return false;
    const cols = z.cols || z.n;
    const go = (zone, at) => { PANEL.zone = zone; PANEL.at = at; lensDraw(); return true; };
    if (e.key === "ArrowDown" && !e.altKey) {
      if (PANEL.at + cols < z.n && z.cols) return go(PANEL.zone, PANEL.at + cols);
      return PANEL.zone + 1 < PANEL.zones.length ? go(PANEL.zone + 1, panelStart(PANEL.zones[PANEL.zone + 1])) : true;
    }
    if (e.key === "ArrowUp" && !e.altKey) {
      if (z.cols && PANEL.at >= cols) return go(PANEL.zone, PANEL.at - cols);
      if (PANEL.zone === 0) { PANEL.keys = false; lensDraw(); return true; }
      return go(PANEL.zone - 1, panelStart(PANEL.zones[PANEL.zone - 1]));
    }
    if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
      const d = e.key === "ArrowRight" ? 1 : -1;
      if (z.name === "hist") { timeKey(d, e.shiftKey); return true; }
      return go(PANEL.zone, Math.max(0, Math.min(z.n - 1, PANEL.at + d)));
    }
    if (e.key === " " || e.key === "Enter") {
      if (z.name === "hist") { if (e.key === "Enter") lensExit(); return true; }
      panelPress(z.name, PANEL.at, e.altKey);
      return true;
    }
    return false;
  }

  /* Where the keys land in a zone: its chosen chip, else its first. */
  function panelStart(z) {
    if (!z) return 0;
    const on = lensBox.querySelector('[data-zone="' + z.name + '"] .on[data-i]');
    return on ? Math.min(+on.dataset.i, z.n - 1) : 0;
  }

  /* After a draw: how many tiles a row of a grid zone holds, as laid out. */
  function panelMeasure() {
    for (const z of PANEL.zones) {
      if (!z.cols) continue;
      const tiles = [...lensBox.querySelectorAll('[data-zone="' + z.name + '"] [data-i]')];
      const top = tiles.length ? tiles[0].offsetTop : 0;
      z.cols = Math.max(1, tiles.filter((el) => el.offsetTop === top).length);
    }
  }

  /* A press on the n-th of a zone, by key or pointer. */
  function panelPress(zone, i, exclude) {
    if (PANEL.name === "time") timePress(zone, i, exclude);
    else if (PANEL.name === "type") typePress(zone, i, exclude);
    else sizePress(i, exclude);
  }

  /* The panels' clicks; true when one was theirs. */
  function panelMouse(e) {
    const el = e.target.closest("[data-zone] [data-i]");
    if (!el) return !!e.target.closest(".pp");
    const zone = el.closest("[data-zone]").dataset.zone;
    const i = +el.dataset.i;
    const at = PANEL.zones.findIndex((z) => z.name === zone);
    if (at >= 0) { PANEL.zone = at; PANEL.at = i; }
    panelPress(zone, i, e.altKey);
    return true;
  }

  /* Typing goes back to the text: the keys leave the panel. */
  q.addEventListener("input", () => { if (PANEL) PANEL.keys = false; }, true);

