  /* ---------------- the time model ----------------
   * `scour_ui::lens` `time.rs` and `time_pick.rs` in the page's words, kept
   * alike by hand: the page test runs the Rust tests' cases on these. Ages
   * count back from now as the service counts them: a row `floor(age / 86400)`
   * days old falls in the first edge `e` with `days <= e`. Pure. */

  /* Today (under 24 h), yesterday (under 48 h), 7, 30 and 365 days. */
  const PRESET_EDGES = [0, 1, 6, 29, 364];
  /* `LENS_TIMES` with the edge each one ends on, newest first. */
  const TIME_PRESETS = LENS_TIMES.map(([value, label], i) => ({ value, label, edge: PRESET_EDGES[i] }));

  /* A facet's bands by edge from its `[key, count]` pairs: one per edge, and
     last what is `older`. A key that is no edge counts nowhere. */
  function byEdge(edges, facets) {
    const out = new Array(edges.length + 1).fill(0);
    for (const [key, n] of facets) {
      const at = key === "older" ? edges.length : /^\d+$/.test(key) ? edges.indexOf(+key) : -1;
      if (at >= 0) out[at] += n;
    }
    return out;
  }

  /* Each preset's count: a preset is a window, its band and every newer one. */
  function presetCounts(facets) {
    let sum = 0;
    return byEdge(PRESET_EDGES, facets).slice(0, 5).map((n) => (sum += n));
  }

  /* What the time panel counts over: the query less its `dm:` terms. */
  const timeAsk = (text) => withoutField(text, "dm");

  /* The four scales: msgid, bars, days a bar, the unit a term counts them in,
     and how much of a date the axis ends say. */
  const TIME_SCALE = {
    day: { label: "Day", bars: 30, days: 1, unit: "d", ends: "DayMonth" },
    week: { label: "Week", bars: 26, days: 7, unit: "w", ends: "DayMonth" },
    month: { label: "Month", bars: 24, days: 30, unit: "m", ends: "MonthYear" },
    year: { label: "Year", bars: 10, days: 365, unit: "y", ends: "Year" },
  };
  const TIME_SCALES = ["day", "week", "month", "year"];
  /* Bar `k` from now holds ages `k` to `k + 1` units: its edge is `(k + 1) * days - 1`. */
  const scaleEdges = (s) => Array.from({ length: TIME_SCALE[s].bars }, (_, k) => (k + 1) * TIME_SCALE[s].days - 1);
  const MODE_LABEL = { between: "Between", before: "Before", after: "After" };
  const TIME_MODES = ["between", "before", "after"];

  /* `lens::histogram`: the bars oldest first, the last ending at `now`. */
  function histogram(scale, facets, now) {
    const n = TIME_SCALE[scale].bars;
    const bands = byEdge(scaleEdges(scale), facets);
    const unit = TIME_SCALE[scale].days * DAY;
    const bars = Array.from({ length: n }, (_, i) => {
      const k = n - 1 - i;
      return { count: bands[k], from: now - (k + 1) * unit, to: now - k * unit };
    });
    return {
      scale, bars, older: bands[n], peak: Math.max(0, ...bars.map((b) => b.count)),
      from: now - n * unit, to: now,
    };
  }

  /* The term preset `i` writes in place of `t`, its `!` kept. */
  const timePresetTerm = (t, i) => neg(t) + "dm:" + TIME_PRESETS[i].value;

  /* `lens::time_term`: a run of bars as a relative term, older bound first.
     `before` reaching now holds everything and writes nothing. */
  function timeTerm(t, s) {
    const n = TIME_SCALE[s.scale].bars, u = TIME_SCALE[s.scale].unit;
    const first = Math.min(s.first, s.last, n - 1), last = Math.min(Math.max(s.first, s.last), n - 1);
    const newer = n - 1 - last, older = n - first;
    let v;
    if (s.mode === "before") {
      if (newer === 0) return "";
      v = "<" + newer + u;
    } else if (s.mode === "after") v = ">" + older + u;
    else v = newer === 0 ? older + u : older + u + ".." + newer + u;
    return neg(t) + "dm:" + v;
  }

  const splitOnce = (s, sep) => {
    const i = s.indexOf(sep);
    return i < 0 ? null : [s.slice(0, i), s.slice(i + sep.length)];
  };

  /* `lens::time_remode`: the run a switch to `mode` leaves on its scale, and
     its term in place of `t`. Before and After turn about the run's older
     edge: Before is what is older, After the run and all that is newer;
     Between keeps the bars. Null when nothing is left on the axis. */
  function timeRemode(t, s, mode) {
    const n = TIME_SCALE[s.scale].bars;
    let first = s.first, last = s.last;
    if (s.mode !== mode && mode === "after") [first, last] = [s.mode === "before" ? s.last + 1 : s.first, n - 1];
    if (s.mode !== mode && mode === "before") {
      if (s.first < 1) return null;
      [first, last] = [0, s.first - 1];
    }
    if (first > last || last >= n) return null;
    const next = { scale: s.scale, mode, first, last };
    return [next, timeTerm(t, next)];
  }

  /* `lens::time_pick`: the preset and the bars a `dm:` value is, or null for
     one the panel cannot show. The bars are on the scale the value names,
     else the finest that fits; see `timeBarsOn`. */
  function timePick(value) {
    const found = timeFound(value, null);
    return found && (found[0] !== null || found[1] !== null) ? { preset: found[0], selected: found[1] } : null;
  }

  /* `lens::time_bars_on`: the bars a value covers on `scale`, whole bars only. */
  function timeBarsOn(value, scale) {
    const found = timeFound(value, scale);
    return found ? found[1] : null;
  }

  /* `[preset, selected]` of a value, its bars on `only` when given; null for no time. */
  function timeFound(value, only) {
    const [cmp, rest] = timeCmp(value.trim().toLowerCase());
    const range = splitOnce(rest, "..");
    const rel = timeRel;
    if (cmp === "" && range) {
      const [lo, hi] = range;
      const a = rel(lo), b = rel(hi);
      if (lo === "") return b && [null, timeBefore(b, only)];
      if (hi === "") return a && timeWindow(a, "between", only);
      return a && b && [null, timeBand(a, b, only)];
    }
    if (range) return null;
    const r = rel(rest);
    if (cmp === "") return r && timeWindow(r, "between", only);
    if (cmp === ">" || cmp === ">=") return r && timeWindow(r, "after", only);
    if (cmp === "<" || cmp === "<=") return r && [null, timeBefore(r, only)];
    return null;
  }

  function timeWindow(r, mode, only) {
    const preset = mode === "between"
      ? TIME_PRESETS.findIndex((p) => timeRel(p.value)?.secs === r.secs) : -1;
    const fit = timeFit([r], only, (n, b) => n[0] >= 1 && n[0] <= b);
    const selected = fit && { scale: fit[0], mode, first: TIME_SCALE[fit[0]].bars - fit[1][0], last: TIME_SCALE[fit[0]].bars - 1 };
    return [preset >= 0 ? preset : null, selected || null];
  }

  function timeBand(older, newer, only) {
    const fit = timeFit([older, newer], only, (n, b) => n[0] > n[1] && n[0] <= b);
    if (!fit) return null;
    const b = TIME_SCALE[fit[0]].bars;
    return { scale: fit[0], mode: "between", first: b - fit[1][0], last: b - 1 - fit[1][1] };
  }

  function timeBefore(r, only) {
    const fit = timeFit([r], only, (n, b) => n[0] >= 1 && n[0] < b);
    return fit && { scale: fit[0], mode: "before", first: 0, last: TIME_SCALE[fit[0]].bars - 1 - fit[1][0] };
  }

  /* The scale the amounts are whole bars of and fit: `only`, when given;
     else the unit written first when it is a week, month or year, then the
     finest that does. */
  function timeFit(rels, only, fits) {
    const unit = { week: "week", month: "month", year: "year" }[rels[0].unit];
    const written = unit && rels.every((r) => r.unit === rels[0].unit) ? [unit] : [];
    for (const scale of only ? [only] : [...written, ...TIME_SCALES]) {
      const bar = TIME_SCALE[scale].days * DAY;
      if (rels.some((r) => r.secs % bar !== 0)) continue;
      const n = rels.map((r) => r.secs / bar);
      if (fits(n, TIME_SCALE[scale].bars)) return [scale, n];
    }
    return null;
  }

  /* A leading comparison and the rest. */
  function timeCmp(v) {
    for (const p of [">=", "<=", ">", "<", "="]) if (v.startsWith(p)) return [p, v.slice(p.length)];
    return ["", v];
  }

  const REL_SECS = { hour: 3600, day: DAY, week: 7 * DAY, month: 30 * DAY, year: 365 * DAY };
  const REL_NAMED = {
    today: [1, "day"], bugun: [1, "day"], "bug\u00fcn": [1, "day"],
    yesterday: [2, "day"], dun: [2, "day"], "d\u00fcn": [2, "day"],
    week: [1, "week"], thisweek: [1, "week"], hafta: [1, "week"],
    month: [1, "month"], thismonth: [1, "month"], ay: [1, "month"],
    year: [1, "year"], thisyear: [1, "year"], yil: [1, "year"], "y\u0131l": [1, "year"],
  };
  const REL_UNITS = {
    h: "hour", s: "hour", saat: "hour", d: "day", g: "day", gun: "day", "g\u00fcn": "day",
    w: "week", hafta: "week", m: "month", ay: "month", y: "year", yil: "year", "y\u0131l": "year",
  };

  /* A relative amount as the language reads one, lower case: `{ n, unit, secs }`. */
  function timeRel(s) {
    const named = Object.hasOwn(REL_NAMED, s) ? REL_NAMED[s] : null;
    const m = named ? null : /^(\d+)(\D.*)$/su.exec(s);
    const [n, unit] = named || (m && Object.hasOwn(REL_UNITS, m[2]) ? [+m[1], REL_UNITS[m[2]]] : [null, null]);
    return unit ? { n, unit, secs: n * REL_SECS[unit] } : null;
  }

