  /* ---------------- the resources panel, drawn ----------------
   * What the status bar's index line opens, over the list: what Scour keeps
   * on this machine, its memory and processor, its programs, and the presses
   * that empty, put in order or restart. Each part is rewritten only when its
   * text changed; `147-resources-live.js` asks and presses. */

  const RES_KEEP = 30;        /* shares in the line: a minute at RES_EVERY */

  const resBox = document.getElementById("res");
  const resDoor = document.getElementById("path-note");
  const RES = {
    open: false, timer: 0, inflight: false, again: false,
    last: null, prev: null, now: null, line: [], failed: false,
    ask: "", doing: "", said: null, saidTimer: 0, restart: null,
    drawn: {},
  };

  const resBytes = (n) => compactBytes(n, decimalMark(LANG));
  const resNumber = (x, digits) => new Intl.NumberFormat(LANG,
    { minimumFractionDigits: digits, maximumFractionDigits: digits }).format(x);
  /* The short form, `%0,7` in Turkish: the language puts the sign. */
  const resPercent = (x) => new Intl.NumberFormat(LANG,
    { style: "percent", minimumFractionDigits: 1, maximumFractionDigits: 1 }).format(x / 100);
  function resUptimeSaid(ms) {
    const [word, a, b] = resUptime(ms);
    return resFill(word, { a: fmt(a), b: fmt(b) });
  }

  const RES_ICONS = {
    disk: '<rect x="1.8" y="4.5" width="12.4" height="7" rx="1.6"/><path d="M4.3 8h3.4"/><circle cx="11.4" cy="8" r=".7"/>',
    mem: '<rect x="2" y="4.5" width="12" height="7" rx="1"/><path d="M5 7v2M8 7v2M11 7v2M4 11.5V13M12 11.5V13M8 11.5V13"/>',
    cpu: '<rect x="4" y="4" width="8" height="8" rx="1.2"/><path d="M6.5 6.5h3v3h-3zM6 2v2M10 2v2M6 12v2M10 12v2M2 6h2M2 10h2M12 6h2M12 10h2"/>',
    mcp: '<path d="M6 2v3M10 2v3M4.5 5h7v2.5a3.5 3.5 0 0 1-7 0zM8 11v3"/>',
    window: '<rect x="2" y="3" width="12" height="10" rx="1.5"/><path d="M2 6h12"/>',
    page: '<circle cx="8" cy="8" r="5.7"/><path d="M2.3 8h11.4M8 2.3c2.2 2.4 2.2 9 0 11.4M8 2.3c-2.2 2.4-2.2 9 0 11.4"/>',
    terminal: '<rect x="2" y="3" width="12" height="10" rx="1.5"/><path d="M5 7l2 1.5L5 10M8.5 10.5h3"/>',
    extractor: '<path d="M4.5 2h4.5l3 3v9h-7.5z"/><path d="M9 2v3h3M6.5 8.5h3M6.5 11h3"/>',
  };
  const resIcon = (name) =>
    `<svg class="rp-i" viewBox="0 0 16 16" aria-hidden="true">${RES_ICONS[name] || ""}</svg>`;
  const resHead = (icon, word, value) =>
    `<div class="rp-bh">${resIcon(icon)}<span>${escape_(word)}</span>` +
    (value ? `<span class="n">${escape_(value)}</span>` : "") + "</div>";
  const resHasCpu = (r) => r.cpu.service_ms > 0 || r.cpu.total > 0;

  /* Scour's programs that are running, in `RUNNING`'s order. */
  function resPrograms(r) {
    return resRunning().map(([kind, label]) => [r.running.find((p) => p.kind === kind), label, kind])
      .filter(([p]) => p && p.count > 0);
  }
  /* What the pill and the memory heading say: the service and those programs. */
  const resOurs = (r) => r.memory.service + resPrograms(r).reduce((s, [p]) => s + p.bytes, 0);

  function resPills(r) {
    const w = resWords();
    const pill = (icon, word, value) =>
      `<span class="rp-pill">${resIcon(icon)}${escape_(word)} <b>${escape_(value)}</b></span>`;
    let html = pill("disk", w.ON_DISK, resBytes(r.files.reduce((s, h) => s + h.bytes, 0)));
    if (r.memory.service) html += pill("mem", w.MEMORY, resBytes(resOurs(r)));
    if (resHasCpu(r)) html += pill("cpu", w.PROCESSOR, RES.now ? resPercent(RES.now.service) : "—");
    return html;
  }

  /* ---- on disk: a row a thing, its press asking first inside the row ---- */
  function resAsk(key, act, sentence) {
    const off = RES.doing === key ? " disabled" : "";
    return `<div class="rp-ask${act.danger ? "" : " calm"}" role="group"><p>${escape_(sentence)}</p>` +
      `<div class="acts"><button type="button" class="rp-btn${act.danger ? " go" : ""}" data-rp-go="${key}"${off}>` +
      `${escape_(act.go)}</button><button type="button" class="rp-btn plain" data-rp-no="">` +
      `${escape_(T("Cancel"))}</button></div></div>`;
  }
  function resSaid(key) {
    const s = RES.said;
    return s && s.key === key
      ? `<div class="rp-said${s.bad ? " bad" : ""}" role="status">${escape_(s.text)}</div>` : "";
  }
  function resSortLine(r) {
    const s = resSort();
    const btn = r.unsorted > 0
      ? `<div class="acts"><button type="button" class="rp-btn" data-rp-ask="sort" aria-expanded="${RES.ask === "sort"}">` +
        `${escape_(s.button)}</button></div>` : "";
    return `<div class="rp-sub"><span>${escape_(resWords().UNSORTED)} <b>${escape_(fmt(r.unsorted))}</b></span>${btn}` +
      (RES.ask === "sort" ? resAsk("sort", s, resFill(s.ask, { n: fmt(r.unsorted) })) : "") +
      resSaid("sort") + "</div>";
  }
  function resRow(r, h, t, share) {
    const k = t.token, act = t.act, refused = resRefused(h.busy);
    const note = resFill(t.note, { n: fmt(h.items), parts: fmt(h.parts), limit: resBytes(h.limit) });
    let btn = "";
    if (act.what === "open") {
      btn = `<button type="button" class="rp-btn" data-rp-open="${k}">${escape_(act.button)}</button>`;
    } else if (!refused) {
      btn = `<button type="button" class="rp-btn${act.danger ? " danger" : ""}" data-rp-ask="${k}" ` +
        `aria-expanded="${RES.ask === k}">${escape_(act.button)}</button>`;
    }
    return `<div class="rp-row" data-k="${k}">` +
      `<div class="nm">${escape_(t.label)} <small>${escape_(note)}</small></div>` +
      `<div class="sz">${escape_(resBytes(h.bytes))}</div>` +
      `<div class="path" title="${escape_(h.path)}">${escape_(homeForm(h.path))}</div>` +
      `<div class="acts">${btn}</div>` +
      `<div class="bar"><i style="width:${(share * 100).toFixed(1)}%"></i></div>` +
      (k === "index" ? resSortLine(r) : "") +
      (refused ? `<div class="rp-said quiet">${escape_(refused)}</div>` : "") +
      (RES.ask === k && !refused ? resAsk(k, act, act.ask) : "") +
      resSaid(k) + "</div>";
  }
  function resDisk(r) {
    const shown = r.files.map((h) => [h, resThing(h.kind)]).filter(([, t]) => t);
    const bars = resBars(shown.map(([h]) => h.bytes));
    const total = shown.reduce((s, [h]) => s + h.bytes, 0);
    return resHead("disk", resWords().ON_DISK, resBytes(total)) +
      shown.map(([h, t], i) => resRow(r, h, t, bars[i])).join("");
  }

  /* ---- memory: the machine's, then the service's and each program's ---- */
  function resMemory(r) {
    const w = resWords(), m = r.memory;
    const running = resPrograms(r).filter(([p]) => p.bytes > 0);
    const ours = resOurs(r);
    let html = resHead("mem", w.MEMORY, m.service ? resBytes(ours) : "");
    if (!m.service && !m.total) return html + `<p class="rp-none">${escape_(w.UNREADABLE)}</p>`;
    if (m.total) {
      const used = Math.max(0, m.total - m.available), others = Math.max(0, used - ours);
      const share = (b) => Math.min(100, (b * 100) / m.total).toFixed(2);
      const said = resFill(w.MACHINE_MEMORY, { used: resBytes(used), total: resBytes(m.total) });
      html += `<div class="rp-sys"><div class="rp-sysbar" role="img" aria-label="${escape_(said)}">` +
        `<span class="us" style="width:${share(ours)}%"></span><span class="oth" style="width:${share(others)}%"></span></div>` +
        `<div class="rp-sysnote"><span><i class="us"></i>Scour ${escape_(resBytes(ours))}</span>` +
        `<span><i class="oth"></i>${escape_(w.OTHERS)} ${escape_(resBytes(others))}</span>` +
        `<span>${escape_(said)}</span></div></div>`;
    }
    const bars = resBars([m.peak, m.service, ...running.map(([p]) => p.bytes)]);
    const row = (word, small, bytes, share, ghost) =>
      `<div class="rp-mrow"><div class="w">${escape_(word)}${small ? ` <small>${escape_(small)}</small>` : ""}</div>` +
      `<div class="v">${escape_(resBytes(bytes))}</div><div class="mbar">` +
      (ghost ? `<u style="width:${(ghost * 100).toFixed(1)}%"></u>` : "") +
      `<i style="width:${(share * 100).toFixed(1)}%"></i></div></div>`;
    if (m.service) html += row(w.SERVICE, m.peak ? resFill(w.PEAK, { n: resBytes(m.peak) }) : "", m.service, bars[1], bars[0]);
    running.forEach(([p, label], i) => { html += row(label, "", p.bytes, bars[2 + i], 0); });
    return html;
  }

  /* ---- processor: the service's share of one core over the last minute ---- */
  function resSpark() {
    const line = RES.line, W = 300, TOP = 6, BOT = 58;
    const top = Math.max(5, Math.ceil(line.reduce((m, v) => Math.max(m, v), 0) * 1.25));
    const step = W / (RES_KEEP - 1), x0 = W - (line.length - 1) * step;
    const y = (v) => (BOT - (Math.min(v, top) / top) * (BOT - TOP)).toFixed(1);
    const d = line.map((v, i) => (i ? "L" : "M") + (x0 + i * step).toFixed(1) + " " + y(v)).join("");
    const drawn = line.length > 1
      ? `<path class="ar" d="${d}L${W} ${BOT}L${x0.toFixed(1)} ${BOT}Z"/><path class="ln" d="${d}"/>` : "";
    const end = line.length ? `<path class="end" d="M${W} ${y(line[line.length - 1])}h0"/>` : "";
    return `<div class="rp-spark"><span class="top">${escape_(resPercent(top))}</span>` +
      `<svg viewBox="0 0 ${W} 64" preserveAspectRatio="none" aria-hidden="true">` +
      `<path class="grid" d="M0 ${TOP}H${W}M0 ${BOT}H${W}"/>${drawn}${end}</svg></div>`;
  }
  function resCpu(r) {
    const w = resWords(), c = r.cpu, now = RES.now;
    if (!resHasCpu(r)) return resHead("cpu", w.PROCESSOR, "") + `<p class="rp-none">${escape_(w.UNREADABLE)}</p>`;
    let html = resHead("cpu", w.PROCESSOR, now ? resFill(w.CORE, { n: resNumber(now.service, 1) }) : "—") + resSpark();
    if (c.total && now) {
      html += `<p class="rp-fact">${escape_(resFill(w.MACHINE_CPU,
        { n: resNumber(now.machine, 1), t: fmt(c.threads), l: resNumber(c.load_x100 / 100, 2) }))}</p>`;
    }
    return html;
  }

  /* ---- Scour's programs: how many of each ---- */
  function resRun(r) {
    const w = resWords();
    return resHead("mcp", w.PROGRAMS, "") + resPrograms(r).map(([p, label, kind]) => {
      const oldest = kind === "mcp" && p.oldest_s
        ? " " + resFill(w.OLDEST, { u: resUptimeSaid(p.oldest_s * 1000) }) : "";
      return `<div class="rp-face">${resIcon(kind)}<span>${escape_(label)}</span>` +
        `<span class="c">${escape_(fmt(p.count))}</span>` +
        (kind === "mcp" ? `<small>${escape_(w.MCP_NOTE + oldest)}</small>` : "") + "</div>";
    }).join("");
  }

  function resFoot(r) {
    const act = resRestartAct();
    const said = RES.failed && !RES.restart
      ? `<span>${escape_(T("connecting…"))}</span>`
      : `<span${RES.restart ? ' class="dim"' : ""}><span class="rp-dot"></span>` +
        `${escape_(resFill(resWords().FOOT, { v: r.version, u: resUptimeSaid(r.uptime_ms) }))}</span>`;
    const btn = r.restartable
      ? `<div class="acts"><button type="button" class="rp-btn" data-rp-restart=""${RES.restart ? " disabled" : ""}>` +
        `${escape_(RES.restart ? act.done : act.button)}</button></div>` : "";
    return said + btn + resSaid("restart");
  }

  /* A part rewritten only when its text moved; the focused button, found
     again by its data attribute, keeps the focus. */
  function resPut(id, html) {
    if (RES.drawn[id] === html) return;
    const el = document.getElementById(id);
    const had = el.contains(document.activeElement) ? document.activeElement : null;
    const attr = had && [...had.attributes].find((a) => a.name.startsWith("data-rp-"));
    RES.drawn[id] = html;
    el.innerHTML = html;
    if (attr) {
      const again = el.querySelector(`[${attr.name}="${CSS.escape(attr.value)}"]`);
      if (again) again.focus();
    }
  }
  function resDraw() {
    const r = RES.last, w = resWords();
    resPut("rp-t", escape_(w.TITLE));
    resBox.setAttribute("aria-label", w.TITLE);
    if (!r) {
      resPut("rp-sum", "");
      resPut("rp-disk", `<p class="rp-none">${escape_(T("connecting…"))}</p>`);
      for (const id of ["rp-mem", "rp-cpu", "rp-run", "rp-foot"]) resPut(id, "");
      return;
    }
    resPut("rp-sum", resPills(r));
    resPut("rp-disk", resDisk(r));
    resPut("rp-mem", resMemory(r));
    resPut("rp-cpu", resCpu(r));
    resPut("rp-run", resRun(r));
    resPut("rp-foot", resFoot(r));
  }

