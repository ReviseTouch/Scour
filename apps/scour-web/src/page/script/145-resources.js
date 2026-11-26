  /* ---------------- the resources model ----------------
   * `scour_ui::resources` in the page's words: the lines of the panel the
   * status bar opens, and the arithmetic on the service's running totals.
   * Each word goes through `T` as a literal where it stands, so the catalogue
   * test sees it; the page test holds every one to the Rust. Placeholders are
   * filled by `resFill`, after translation. */

  /* `THINGS`, in the order the service sends them; `what` is `Do`. */
  function resThings() {
    return [
      { token: "index", label: T("Index"), note: T("{n} entries · parts: {parts}"),
        act: { what: "reset", button: T("Start over…"),
          ask: T("The index is emptied and every source is walked again. That takes a few minutes, and searches find less until it is done. Settings, rules and tabs stay."),
          go: T("Empty and walk again"), done: T("The index was emptied; the walk has started."), danger: true } },
      { token: "texts", label: T("Content search cache"), note: T("{n} documents · up to {limit}"),
        act: { what: "forget-texts", button: T("Empty…"),
          ask: T("Text read out of PDF and Office documents is deleted. Content search is slower until it has read them again."),
          go: T("Empty it"), done: T("Content search cache emptied."), danger: true } },
      { token: "previews", label: T("Previews"), note: T("documents turned into PDF"),
        act: { what: "clear-previews", button: T("Empty…"),
          ask: T("Previews are deleted; each is made again the first time its document is looked at."),
          go: T("Empty it"), done: T("Previews emptied."), danger: true } },
      { token: "page", label: T("Page window's profile"), note: T("its browser's data"),
        act: { what: "clear-page", button: T("Empty…"),
          ask: T("The page window's own browser data is deleted: its cache and its window size."),
          go: T("Empty it"), done: T("Profile emptied."), danger: true } },
      { token: "state", label: T("Settings and logs"), note: T("kept"),
        act: { what: "open", button: T("Open folder"), ask: "", go: "", done: "", danger: false } },
    ];
  }

  /* `UNSORTED`'s button, and `RESTART`. */
  function resSort() {
    return { what: "sort", button: T("Put in order…"),
      ask: T("{n} rows are put in order. It takes about a minute and makes the index smaller; searches are not measurably faster for it."),
      go: T("Put in order"), done: T("Putting the rows in order."), danger: false };
  }
  function resRestartAct() {
    return { what: "restart", button: T("Restart the service"), ask: "", go: "", done: T("Restarting…"), danger: false };
  }

  /* `thing`: the line for a wire token, or null for one this page does not know. */
  function resThing(token) {
    return resThings().find((t) => t.token === token) || null;
  }

  /* `refused`: what a row says instead of its button, for the wire's `busy`. */
  function resRefused(busy) {
    return busy === "open" ? T("Close the page window first: its browser is using this profile.") : null;
  }

  /* `RUNNING`, kind and label, in the panel's order. */
  function resRunning() {
    return [["window", T("Window")], ["page", T("Page bridge")], ["terminal", T("Terminal")],
            ["mcp", T("MCP servers")], ["extractor", T("Document reader")]];
  }

  /* The panel's other words, by the Rust constants' names. */
  function resWords() {
    return {
      UNSORTED: T("Unsorted rows"),
      MCP_NOTE: T("Every open Claude session starts one of its own."),
      OLDEST: T("The oldest has been running for {u}."),
      TITLE: T("Scour on this machine"),
      ON_DISK: T("On disk"),
      MEMORY: T("Memory"),
      PROCESSOR: T("Processor"),
      PROGRAMS: T("Running"),
      SERVICE: T("Service"),
      PEAK: T("at most {n} since it started"),
      MACHINE_MEMORY: T("{used} of {total} in use"),
      OTHERS: T("other programs"),
      CORE: T("{n}% of one core"),
      MACHINE_CPU: T("machine {n}% busy · {t} threads · load {l}"),
      FOOT: T("Service running · version {v} · up {u}"),
      OPEN_HINT: T("What Scour keeps on this machine"),
      UNREADABLE: T("this system does not say"),
    };
  }

  /* `{name}` filled in a word already translated. */
  function resFill(s, vars) {
    for (const k of Object.keys(vars)) s = s.split("{" + k + "}").join(vars[k]);
    return s;
  }

  /* `shares`: between two answers, the service's processor time as a share
     of one core and the machine's busy share, in percent; null when no time
     passed or the service started again in between. */
  function resShares(before, now) {
    const wall = now.uptime_ms - before.uptime_ms;
    if (!(wall > 0)) return null;
    const less = (a, b) => Math.max(0, a - b);
    const total = less(now.total, before.total);
    return {
      service: less(now.service_ms, before.service_ms) * 100 / wall,
      machine: total === 0 ? 0 : less(now.busy, before.busy) * 100 / total,
    };
  }

  /* `uptime`: the largest unit and the next, as a word and its `{a}`, `{b}`. */
  function resUptime(ms) {
    const s = Math.floor(ms / 1000);
    if (s < 60) return [T("{a} s"), s, 0];
    if (s < 3600) return [T("{a} min {b} s"), Math.floor(s / 60), s % 60];
    if (s < 86400) return [T("{a} h {b} min"), Math.floor(s / 3600), Math.floor((s % 3600) / 60)];
    return [T("{a} d {b} h"), Math.floor(s / 86400), Math.floor((s % 86400) / 3600)];
  }

  /* `bars`: each size's share of the largest; nothing that is something is
     drawn as nothing. */
  function resBars(sizes) {
    const max = sizes.reduce((m, b) => Math.max(m, b), 0);
    return sizes.map((b) => (b === 0 || max === 0 ? 0 : Math.max(b / max, 0.01)));
  }

  /* `scour_ui::format::decimal_mark` and `compact_bytes`: two units and one
     decimal, rounded in whole tenths so a tie goes to the even one, as Rust's. */
  function decimalMark(lang) {
    return lang.startsWith("tr") ? "," : ".";
  }
  function compactBytes(n, decimal) {
    const unit = n >= 1073741824 ? 1073741824 : 1048576;
    let tenths = Math.floor((n * 10) / unit);
    const left = n * 10 - tenths * unit;
    if (left * 2 > unit || (left * 2 === unit && tenths % 2 === 1)) tenths++;
    const said = Math.floor(tenths / 10) + "." + (tenths % 10) + (unit === 1048576 ? " MB" : " GB");
    return decimal === "." ? said : said.replace(".", decimal);
  }

