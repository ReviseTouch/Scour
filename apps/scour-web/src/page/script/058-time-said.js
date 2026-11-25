  /* ---------------- the time in words ----------------
   * `scour_ui::lens::time_said` in the page's words: a `dm:` value as its
   * capsule says it. The words are msgids and the dates are days since the
   * epoch with how much of them to say; `dateSaid` writes one in the page's
   * language. Pure but for that. */

  const SAID_BEFORE = "before {date}", SAID_AFTER = "after {date}";
  const SAID_SINCE = "since {date}", SAID_UNTIL = "until {date}";

  /* Days since 1970-01-01 to `[year, month, day]`, and back. */
  function civil(day) {
    const d = new Date(day * DAY * 1000);
    return [d.getUTCFullYear(), d.getUTCMonth() + 1, d.getUTCDate()];
  }
  const daysFromCivil = (y, m, d) => Math.round(Date.UTC(y, m - 1, d) / 1000 / DAY);
  /* The last day of the month `day` is in. */
  function monthEnd(day) {
    const [y, m] = civil(day);
    return daysFromCivil(m === 12 ? y + 1 : y, m === 12 ? 1 : m + 1, 1) - 1;
  }

  /* The msgid of "within the last n" in a unit. */
  function saidLast(n, unit) {
    if (unit === "hour") return n === 1 ? "last hour" : "last {n} hours";
    return { day: "last {n} days", week: "last {n} weeks", month: "last {n} months", year: "last {n} years" }[unit];
  }

  /* `YYYY-MM-DD` as days since the epoch, as the language reads one. */
  function saidIso(s) {
    const p = s.split("-");
    if (p.length !== 3 || !p.every((x) => /^[+-]?\d+$/.test(x))) return null;
    const [y, m, d] = p.map(Number);
    return m >= 1 && m <= 12 && d >= 1 && d <= 31 ? daysFromCivil(y, m, d) : null;
  }

  /* One end of a range: an amount back from now, or a calendar day. */
  function saidPoint(s) {
    const r = timeRel(s);
    if (r) return { back: r };
    const on = saidIso(s);
    return on === null ? null : { on };
  }

  /* `lens::time_said(value, now, offset)`: `{ label }`, `{ count, n }`,
     `{ date }`, `{ bound, date }` or `{ from, to }`, dates as `{ day, form }`;
     null for what is no time, or a range that holds nothing. `offset` is
     seconds east of UTC: a relative bound is the local day it falls on now. */
  function timeSaid(value, now, offset) {
    const [cmp, rest] = timeCmp(value.trim().toLowerCase());
    const range = splitOnce(rest, "..");
    const at = saidClock(now, offset);
    if (cmp === "" && range) {
      const [lo, hi] = range;
      if (lo && !hi) return at.bound(">=", lo);
      if (!lo && hi) return at.bound("<=", hi);
      return lo ? at.range(lo, hi) : null;
    }
    if (range) return null;
    return cmp === "" ? at.plain(rest) : at.bound(cmp, rest);
  }

  function saidClock(now, offset) {
    const back = (r) => Math.floor((now - r.secs + offset) / DAY);
    const year = civil(Math.floor((now + offset) / DAY))[0];
    const dayOf = (p) => (p.back ? back(p.back) : p.on);
    /* A day said alone: its year only when it is not this one. */
    const single = (day) => ({ day, form: civil(day)[0] === year ? "DayMonth" : "DayMonthYear" });
    const windowOf = (r) => {
      const p = TIME_PRESETS.find((x) => timeRel(x.value)?.secs === r.secs);
      return p ? { label: p.label } : { count: saidLast(r.n, r.unit), n: r.n };
    };
    /* A bound with as little of its date as is true: "before 2025". */
    const boundAt = (msgid, day) => {
      const [, m, d] = civil(day);
      const starts = msgid === SAID_BEFORE || msgid === SAID_SINCE;
      const edge = starts ? d === 1 : day === monthEnd(day);
      const form = !edge ? single(day).form : m === (starts ? 1 : 12) ? "Year" : "MonthYear";
      return { bound: msgid, date: { day, form } };
    };
    return {
      plain(s) {
        const p = saidPoint(s);
        return !p ? null : p.back ? windowOf(p.back) : { date: single(p.on) };
      },
      bound(cmp, s) {
        const p = saidPoint(s);
        if (!p) return null;
        if (p.back && (cmp === ">" || cmp === ">=")) return windowOf(p.back);
        if (p.back && (cmp === "<" || cmp === "<=")) return boundAt(SAID_BEFORE, back(p.back));
        if (p.back) return null;
        if (cmp === "=") return { date: single(p.on) };
        const msgid = { "<": SAID_BEFORE, ">=": SAID_SINCE, ">": SAID_AFTER }[cmp] || SAID_UNTIL;
        return boundAt(msgid, p.on);
      },
      range(lo, hi) {
        const a = saidPoint(lo), b = saidPoint(hi);
        if (!a || !b) return null;
        const from = dayOf(a), to = dayOf(b);
        if (from > to) return null;
        const [fy, fm, fd] = civil(from), [ty, tm, td] = civil(to);
        if (from === to) return { date: single(from) };
        if (fd === 1 && fm === 1 && fy === ty && tm === 12 && td === 31) return { date: { day: from, form: "Year" } };
        if (fd === 1 && fy === ty && fm === tm && to === monthEnd(to)) return { date: { day: from, form: "MonthYear" } };
        const form = fy === ty && fm === tm ? "Day" : fy === ty ? "DayMonth" : single(from).form;
        return { from: { day: from, form }, to: single(to) };
      },
    };
  }

  /* ---- the face's part: a day in the page's language ---- */

  const DATE_FORM = {
    Day: { day: "numeric" },
    DayMonth: { day: "numeric", month: "short" },
    DayMonthYear: { day: "numeric", month: "short", year: "numeric" },
    MonthYear: { month: "short", year: "numeric" },
    Year: { year: "numeric" },
  };
  const dateFormat = (form) => new Intl.DateTimeFormat(LANG, { ...DATE_FORM[form], timeZone: "UTC" });
  const dateSaid = (d) => dateFormat(d.form).format(new Date(d.day * DAY * 1000));

  /* What `timeSaid` gave, as words: "last 7 days", "12–25 Nov", "before 2025". */
  function timeWords(said) {
    if (!said) return "";
    if (said.label) return T(said.label);
    if (said.count) return T(said.count, { n: fmt(said.n) });
    if (said.bound) return T(said.bound, { date: dateSaid(said.date) });
    if (said.date) return dateSaid(said.date);
    // One month: the locale joins the two days ("12–25 Nov", "Nov 12 – 25").
    if (said.from.form === "Day") {
      const f = dateFormat(said.to.form);
      if (f.formatRange) return f.formatRange(new Date(said.from.day * DAY * 1000), new Date(said.to.day * DAY * 1000));
    }
    return dateSaid(said.from) + " – " + dateSaid(said.to);
  }

  /* Seconds east of UTC here at `at`, as `format::local_offset` gives it. */
  const localOffset = (at) => -new Date(at * 1000).getTimezoneOffset() * 60;

