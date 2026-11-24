  /* Nothing below here learns where the rows came from. */
  function draw(text, p, hits, res) {
    now = Math.floor(Date.now() / 1000);

    /* The query changed, so the marked letters moved in every row. */
    for (const tr of pool) tr.__stamp = "";
    repaint(true);

    /* The query-scoped state, not this capped reply: a sort keeps the exact
       total the sidebar paid for. */
    const shown = LIST.exact ? fmt(LIST.total) : fmt(LIST.total) + "+";
    document.getElementById("count").textContent = withIndexTotal(shown);
    drawCost(res.took_us, res.rows_visited);

    syncFacets(p);
  }

  /* Kept, not only printed: a language switch rewrites both halves. */
  let LAST_COST = null;
  /* How far a `content:` query's reading has got; null on any other query. */
  let READING = null;
  const READ_SAID = {
    running: "reading files: {n}",
    paused: "stopped half-way: {n} files read",
    done: "{n} files read",
  };
  function drawCost(us, rowsVisited) {
    if (us !== undefined) LAST_COST = { us, rowsVisited };
    if (!LAST_COST) return;
    /* From `Intl`: a hand-written comma prints `0,29 ms` beside `4,864 rows`. */
    document.getElementById("ms").textContent = MS.format(LAST_COST.us / 1000) + " ms";
    const rows = document.getElementById("rows");
    rows.textContent = READING
      ? T(READ_SAID[READING.stage], { n: fmt(READING.files) })
      : T("{n} rows read", { n: fmt(LAST_COST.rowsVisited) });
    rows.classList.toggle("reading", !!READING && READING.stage === "running");
    document.getElementById("read-on").hidden = !READING || READING.stage !== "paused";
    document.getElementById("read-again").hidden = !READING;
  }
  /* A window of a query that reads files: the list is as long as what has
     been found, and that is the newest word on it. */
  function tellReading(res) {
    READING = res.reading;
    const done = res.reading.stage === "done";
    if (LIST.total !== res.total || LIST.exact !== done) setTotal(res.total, done);
    const shown = done ? fmt(res.total) : fmt(res.total) + "+";
    document.getElementById("count").textContent = withIndexTotal(shown);
    drawCost();
  }
  for (const [id, from] of [["read-on", ""], ["read-again", "start"]]) {
    document.getElementById(id).addEventListener("click", () => {
      fetch(`/api/reread?${new URLSearchParams({ from, t: TOKEN })}`, {
        method: "POST",
        cache: "no-store",
      }).catch((e) => console.error("scour: the reading was not told:", e));
    });
  }

  /* Three views of the same rows, so one walk. They wait for the typing to
   * stop where the rows do not: a window is 7 to 13 ms, a sidebar 1,291 ms for
   * `r` against 29 ms for `rapor`, and the next wait is what this cost. */
  const sidebarSoon = afterQuiet(() => {
    /* Checked here: after the reply, the old walk has already been spent. */
    if (LIST.query !== null && LIST.query === q.value) sidebar(LIST.query);
  }, 220);

  let sidebarGeneration = 0;
  /* What the last one cost, so the live refresh can decline to repeat it. */
  let sidebarCost = 0;

  /* A rail answers what you would get instead, so counting it through its own
   * filter leaves one key and a column of zeros. Only its own terms come out,
   * matched by `field()`; null when there was nothing to take. */
  function without(text, names) {
    const toks = tokenise(text);
    const kept = toks.filter((tok) => {
      if (/^\s+$/.test(tok)) return true;
      const bare = tok.startsWith("!") ? tok.slice(1) : tok;
      const sp = fieldSplit(bare);
      const f = sp && field(sp[0]);
      return !(f && names.includes(f.name) && accepts(f, sp[1]));
    });
    /* By count, not by text: a dropped term leaves its space behind. */
    if (kept.length === toks.length) return null;
    return kept.join("").trim();
  }

  function sidebar(text) {
    /* Before the call: a stale answer costs CPU even unpainted. */
    if (text !== q.value) return Promise.resolve();
    const ours = ++sidebarGeneration;
    const began = Date.now();
    /* A stale walk must not poison the budget: broad queries finish late. */
    const charge = () => {
      if (ours === sidebarGeneration && text === q.value)
        sidebarCost = Date.now() - began;
    };
    /* A re-sort advances `generation` but does not change these counts. */
    const current = () => ours === sidebarGeneration && text === q.value;

    const forKind = without(text, ["kind"]);
    const forAge = without(text, ["dm"]);

    /* One request per distinct query, and every call keeps `by=kind,age`:
     * `NativeIndex::facets` scans unbounded only for an age band, so `by=kind`
     * alone samples 200,000 rows — images 1,430 against 210,551 exact. */
    const asked = new Map();
    const ask = (which) => {
      if (!asked.has(which)) asked.set(which, SERVICE.sidebar(which, BAR_EDGES));
      return asked.get(which);
    };
    const inForce = ask(text);
    const kindFrom = forKind === null ? inForce : ask(forKind);
    const ageFrom = forAge === null ? inForce : ask(forAge);

    const groupOf = (res, by) => (res.groups || []).find((g) => g.by === by);

    /* The total belongs to the query in force, or it says `2.238.902` over two
     * hundred thousand rows. Through `setTotal`, which drops stale rows. */
    const total = inForce.then((res) => {
      if (!current()) return;
      setTotal(res.total, !res.capped);
      repaint(true);
      const shown = res.capped ? fmt(res.total) + "+" : fmt(res.total);
      document.getElementById("count").textContent = withIndexTotal(shown);
    });

    /* Drawn as each answer lands: the slowest would hold the rail blank. */
    const kinds = kindFrom.then((res) => {
      if (!current()) return;
      const g = groupOf(res, "kind");
      if (!g) return;
      KIND_COUNTS = Object.fromEntries(g.facets.map((f) => [f.key, f.count]));
      sidebarSettled();
      syncFacets(parse(q.value));
    });

    const ages = ageFrom.then((res) => {
      if (!current()) return;
      const g = groupOf(res, "age");
      if (!g) return;
      const by = Object.fromEntries(g.facets.map((f) => [f.key, f.count]));
      const counts = BAR_EDGES.map((e) => by[String(e)] || 0).reverse();
      /* Older than the last edge belongs to the last bar: the scale ends
         at two years, the files do not. */
      counts[0] += by.older || 0;
      paintBars(counts, res.capped);
    });

    /* Charged for the whole set: one call is a third of the true price. */
    return Promise.all([total, kinds, ages]).then(charge, (e) => {
      charge();
      console.error("scour: sidebar did not draw:", e);
    });
  }

  /* Say so, rather than an empty list that reads as "nothing matched". */
  function offline(err) {
    /* The search's `catch` also catches anything the drawing throws. */
    console.error("scour:", err);
    LIST.query = null;
    clearSel();
    clearRows();
    for (const request of LIST.pending.values()) request.stop.abort();
    LIST.pending.clear();
    body.innerHTML = "";
    /* Which shows the message; the sentence under it says why. */
    setTotal(0, true);
    painted = "";
    repaint(true);
    document.getElementById("count").textContent = "—";
    document.getElementById("ms").textContent = "—";
    document.getElementById("rows").textContent =
      String(err.message || err).includes("403")
        ? T("connection refused — the key in the link is not this run's")
        : T("the service cannot be reached — is scourd running?");
  }


  /* Never drawn from the loaded rows: they are the two hundred newest matches,
   * so the chart would say "today" whatever was typed. The bands are sent
   * rather than assumed — the report wants six named periods. */
  const BAR_COUNT = 24;
  const BAR_SPAN_DAYS = 730;

  /* The upper edge of each bar, in days, logarithmic: a day, a week and a
     year all have room on one screen. */
  const BAR_EDGES = Array.from({ length: BAR_COUNT }, (_, i) =>
    Math.max(1, Math.round(((10 ** (1 - i / BAR_COUNT) - 1) / 9) * BAR_SPAN_DAYS))
  ).reverse();

  function paintBars(counts, capped) {
    const max = Math.max(1, ...counts);
    const edges = [...BAR_EDGES].reverse();
    bars.innerHTML = counts.map((v, i) => {
      const h = v === 0 ? 2 : Math.max(3, Math.round((v / max) * 24));
      const days = edges[i];
      const oldest = i === 0;
      const c = v === 0 ? "--line" : bandOf(now - days * DAY);
      /* The oldest bar has no upper bound: `dm:730d` would be everything. */
      const span = oldest
        ? T("older than {days} days", { days })
        : T("up to {days} days", { days });
      const title = T("{n} results · {span}", { n: fmt(v), span })
        + (capped ? T(" (sampled)") : "");
      const label = oldest
        ? T("{n} results, older than {days} days", { n: v, days })
        : T("{n} results, the last {days} days", { n: v, days });
      return `<button type="button" ${oldest ? "" : `data-days="${days}"`}
                title="${escape_(title)}"
                aria-label="${escape_(label)}">
                <i style="height:${h}px;background:var(${c})"></i></button>`;
    }).join("");
  }

  /* `glyph` is a kind's token, or `size` for the weight the menu draws. */
  function facetRow(label, term, count, share, on, glyph) {
    const fill = share > 0
      ? `<span class="fill" style="width:calc((100% - 24px) * ${share.toFixed(4)})"></span>` : "";
    const ico = glyph === "size" && MENU_ICONS.weight
      ? `<span class="mi" aria-hidden="true">${MENU_ICONS.weight}</span>`
      : `<span class="ico k-${GLYPHS.has(glyph) ? glyph : "file"}" aria-hidden="true"></span>`;
    return `<button class="facet${share > 0 ? " bar" : ""}" type="button"
        data-term="${term}" aria-pressed="${on}">${fill}${ico}
      <span class="nm">${label}</span><span class="ct">${count === null || count === undefined ? "" : fmt(count)}</span></button>`;
  }

  /* By kind token, under the query with its `kind:` term taken out, so a row
     can read more than the list is showing. */
  let KIND_COUNTS = null;

  /* Dimmed rather than blanked: a zero reads as an answer, and the gap is
   * 247 ms for a kind and 556 ms for an age band. */
  function markSidebarStale() {
    /* Nothing to dim on the first query: the rail is an honest blank. */
    if (KIND_COUNTS === null) return;
    document.querySelector(".rail").classList.add("settling");
  }

  function sidebarSettled() {
    document.querySelector(".rail").classList.remove("settling");
  }

  function syncFacets(p) {
    const count = ([, key]) => (KIND_COUNTS ? (KIND_COUNTS[key] || 0) : null);
    const top = Math.max(1, ...KINDS.map((r) => count(r) || 0));
    document.getElementById("kinds").innerHTML =
      KINDS.map((row) => {
        const [tr, key] = row;
        const n = count(row);
        return facetRow(tr, "kind:" + key, n, n === null ? 0 : n / top, p.kind === key, key);
      }).join("");
    document.getElementById("scopes").innerHTML =
      SCOPES.map(([tr, path, n]) => facetRow(tr, "under:" + path, n, 0, p.under === path, "folder")).join("");
    document.getElementById("sizes").innerHTML =
      SIZES.map(([tr, v, n]) => facetRow(tr, "size:" + v, n, 0, p.size === v, "size")).join("");
  }

