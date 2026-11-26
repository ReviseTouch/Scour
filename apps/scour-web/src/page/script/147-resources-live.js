  /* ---------------- the resources panel, live ----------------
   * Asked every two seconds while open, never while closed; the presses, and
   * how the panel opens and closes. */

  const RES_EVERY = 2000;     /* ms between answers while open */
  const RES_SAID_MS = 3000;   /* how long a row says what was done */
  const RES_BACK_MS = 30000;  /* a restart not back by then is given up on */

  /* One answer in: the processor's share against the one before it, and
     whether a restart has come back — its uptime went back, or an answer
     failed in between. */
  function resTake(r) {
    if (RES.restart && Date.now() - RES.restart.at > RES_BACK_MS) RES.restart = null;
    if (!r) {
      RES.failed = true;
      if (RES.restart) RES.restart.lost = true;
      return;
    }
    RES.failed = false;
    const s = { uptime_ms: r.uptime_ms, service_ms: r.cpu.service_ms, busy: r.cpu.busy, total: r.cpu.total };
    if (RES.restart && (RES.restart.lost || r.uptime_ms < RES.restart.uptime)) RES.restart = null;
    if (RES.prev && s.uptime_ms < RES.prev.uptime_ms) { RES.line = []; RES.now = null; }
    const sh = RES.prev ? resShares(RES.prev, s) : null;
    if (sh) {
      RES.now = sh;
      RES.line.push(sh.service);
      if (RES.line.length > RES_KEEP) RES.line.shift();
    }
    RES.prev = s;
    RES.last = r;
    /* An ask stays open only over a row that can still be emptied. */
    const asked = r.files.find((h) => h.kind === RES.ask);
    if (asked && resRefused(asked.busy)) RES.ask = "";
  }

  async function resPoll() {
    clearTimeout(RES.timer);
    RES.timer = 0;
    if (!RES.open || document.hidden) return;
    if (RES.inflight) { RES.again = true; return; }
    RES.inflight = true;
    let r = null;
    try { r = await SERVICE.get("resources", {}); } catch { r = null; }
    RES.inflight = false;
    if (!RES.open) return;
    resTake(r);
    resDraw();
    if (RES.again) { RES.again = false; resPoll(); return; }
    RES.timer = setTimeout(resPoll, RES_EVERY);
  }

  /* The presses answer `{ ok }` or an error the bridge or the service wrote. */
  async function resPost(route, params) {
    try {
      const r = await fetch(`/api/${route}?${new URLSearchParams({ ...params, t: TOKEN })}`,
        { method: "POST", cache: "no-store" });
      const j = await r.json().catch(() => ({}));
      return { ok: r.ok && !j.error, status: r.status, error: j.error || `${route}: ${r.status}` };
    } catch (e) {
      return { ok: false, status: 0, error: String((e && e.message) || e) };
    }
  }
  /* Said in the row for a while, then the panel asks again. */
  function resSay(key, text, bad) {
    RES.said = { key, text, bad };
    clearTimeout(RES.saidTimer);
    RES.saidTimer = setTimeout(() => {
      RES.said = null;
      if (RES.open) { resDraw(); resPoll(); }
    }, RES_SAID_MS);
    resDraw();
  }
  async function resGo(key) {
    if (RES.doing) return;
    const sort = key === "sort";
    const act = sort ? resSort() : resThing(key).act;
    RES.doing = key;
    resDraw();
    const got = sort ? await resPost("index-do", { do: "reorganize" }) : await resPost("clear", { what: key });
    RES.doing = "";
    RES.ask = "";
    if (got.ok) resSay(key, act.done, false);
    else resSay(key, got.status === 409 && key === "page" ? resRefused("open") : got.error, true);
  }
  async function resRestart() {
    if (RES.restart || !RES.last) return;
    RES.restart = { at: Date.now(), uptime: RES.last.uptime_ms, lost: false };
    resDraw();
    const got = await resPost("restart", {});
    if (!got.ok) { RES.restart = null; resSay("restart", got.error, true); }
  }
  async function resShow(key) {
    const got = await resPost("held-open", { what: key });
    if (!got.ok) resSay(key, got.error, true);
  }

  /* Over the list, its foot a step above the status bar, no taller than
     the list it covers. */
  function resPlace() {
    const top = document.querySelector(".meter").getBoundingClientRect().top;
    const list = document.getElementById("view-search").getBoundingClientRect().top;
    resBox.style.bottom = Math.round(innerHeight - top + 8) + "px";
    resBox.style.maxHeight = Math.max(240, Math.round(top - list - 16)) + "px";
  }
  function resOpen(open) {
    if (open === RES.open) return;
    RES.open = open;
    resBox.hidden = !open;
    resDoor.setAttribute("aria-expanded", String(open));
    RES.ask = "";
    clearTimeout(RES.timer);
    RES.timer = 0;
    if (!open) return;
    RES.prev = null; RES.now = null; RES.line = [];
    resPlace();
    resDraw();
    resPoll();
  }
  /* What changes with the language: the door's hint, and an open panel. */
  function resRelabel() {
    resDoor.title = resWords().OPEN_HINT;
    if (RES.open) { RES.drawn = {}; resDraw(); }
  }

  resDoor.addEventListener("click", () => resOpen(!RES.open));
  resBox.addEventListener("click", (e) => {
    const b = e.target.closest("button");
    if (!b || b.disabled) return;
    const d = b.dataset;
    if (b.id === "rp-x") { resOpen(false); resDoor.focus(); }
    else if (d.rpAsk !== undefined) {
      RES.ask = RES.ask === d.rpAsk ? "" : d.rpAsk;
      resDraw();
      const go = RES.ask && resBox.querySelector(`[data-rp-go="${RES.ask}"]`);
      if (go) go.focus();
    } else if (d.rpNo !== undefined) {
      const was = RES.ask;
      RES.ask = "";
      resDraw();
      const back = resBox.querySelector(`[data-rp-ask="${was}"]`);
      if (back) back.focus();
    } else if (d.rpGo) resGo(d.rpGo);
    else if (d.rpOpen) resShow(d.rpOpen);
    else if (d.rpRestart !== undefined) resRestart();
  });
  /* Esc closes an open ask first, then the panel, and nothing else. */
  document.addEventListener("keydown", (e) => {
    if (!RES.open || e.key !== "Escape") return;
    e.preventDefault();
    e.stopImmediatePropagation();
    if (RES.ask) {
      const was = RES.ask;
      RES.ask = "";
      resDraw();
      const back = resBox.querySelector(`[data-rp-ask="${was}"]`);
      if (back) back.focus();
      return;
    }
    const inside = resBox.contains(document.activeElement);
    resOpen(false);
    if (inside) resDoor.focus();
  }, true);
  document.addEventListener("pointerdown", (e) => {
    if (RES.open && !resBox.contains(e.target) && !resDoor.contains(e.target)) resOpen(false);
  }, true);
  document.getElementById("tab-report").addEventListener("click", () => resOpen(false));
  document.addEventListener("visibilitychange", () => { if (!document.hidden && RES.open) resPoll(); });
  addEventListener("resize", () => { if (RES.open) resPlace(); });
  resRelabel();

