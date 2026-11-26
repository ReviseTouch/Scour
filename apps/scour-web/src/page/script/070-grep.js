  /* ---------------- On-demand Grep capsule & micro-triggers ---------------- */
  const grepBar = document.getElementById("grep-bar");
  const grepInput = document.getElementById("grep-q");
  const grepShadow = document.getElementById("grep-shadow");

  const grepView = document.getElementById("grep-view");
  const grepCaseBtn = document.getElementById("grep-case");
  const grepBinBtn = document.getElementById("grep-bin");
  let GREP = { mode: "text", exact: false, binary: false, markup: false, files: [], next: null, gen: 0, stop: null, timer: 0 };

  /* What the inside line says at its end: how many files a pattern would be
     read in, that they are being read, or how many hold it. */
  const grepMeter = document.getElementById("grep-meter");
  // `var`: a total can land before this line has run, and must find a name.
  var grepReady = true;
  function drawGrepMeter(reading) {
    if (grepBar.hidden) return;
    const n = (k) => fmt(k);
    grepMeter.textContent = !grepInput.value
      ? T("{n} files to read", { n: n(LIST.total) + (LIST.exact ? "" : "+") })
      : reading ? T("reading…")
      : T("in {n} files", { n: n(GREP.files.length) + (GREP.next != null ? "+" : "") });
  }

  function setGrepOpen(open) {
    grepBar.hidden = !open;
    holdTab();
    syncTriggers(q.value);
    if (!open) {
      if (GREP.stop) GREP.stop.abort();
      clearTimeout(GREP.timer);
      GREP.files = [];
      GREP.next = null;
      grepView.hidden = true;
      scrollEl.hidden = false;
      q.focus();
      return;
    }
    grepInput.focus();
    grepInput.select();
    drawGrepMeter(false);
    if (grepInput.value.trim()) grepSoon(false);
  }

  /* Another tab's pattern: the line open with it, or closed and empty. The
     caret stays in the query, where a tab switch leaves it. */
  function putGrep(text) {
    grepInput.value = text;
    drawGrepShadow();
    if (!text) {
      if (!grepBar.hidden) setGrepOpen(false);
      return;
    }
    grepBar.hidden = false;
    syncTriggers(q.value);
    drawGrepMeter(false);
    grepSoon(false);
  }

  function grepSoon(append) {
    clearTimeout(GREP.timer);
    if (grepBar.hidden) return;
    const pattern = grepInput.value;
    if (!pattern) {
      if (GREP.stop) GREP.stop.abort();
      grepView.hidden = true;
      scrollEl.hidden = false;
      drawGrepMeter(false);
      return;
    }
    drawGrepMeter(true);
    GREP.timer = setTimeout(() => runGrep(append), append ? 0 : 180);
  }

  function runGrep(append) {
    const pattern = grepInput.value;
    if (!pattern || grepBar.hidden) return;
    const mine = ++GREP.gen;
    if (GREP.stop) GREP.stop.abort();
    GREP.stop = new AbortController();
    const offset = append && GREP.next != null ? GREP.next : 0;
    if (!append) {
      GREP.files = [];
      GREP.next = null;
      grepView.innerHTML = '<div class="grep-sum">' + escape_(T("Scanning matching files…")) + "</div>";
      grepView.hidden = false;
      scrollEl.hidden = true;
    }
    SERVICE.grep({
      q: q.value,
      pattern,
      mode: GREP.mode,
      exact: GREP.exact ? "1" : "0",
      binary: GREP.binary ? "1" : "0",
      markup: GREP.markup ? "1" : "0",
      sort: sortKey,
      desc: sortDesc ? "1" : "0",
      offset,
      top: 50,
      hits: 5,
    }, GREP.stop.signal).then((res) => {
      if (mine !== GREP.gen) return;
      GREP.files = append ? GREP.files.concat(res.files || []) : (res.files || []);
      GREP.next = res.next ?? null;
      drawGrepMeter(false);
      drawGrep(res);
    }).catch((err) => {
      if (err && err.name === "AbortError") return;
      if (mine !== GREP.gen) return;
      grepView.innerHTML = '<div class="grep-sum">' + escape_(String(err.message || err)) + "</div>";
    });
  }

  function drawGrep(res) {
    GREP.lastRes = res || GREP.lastRes || {};
    res = GREP.lastRes;
    grepView.hidden = false;
    scrollEl.hidden = true;
    const skips = [];
    if (res.binary) skips.push(T("{n} binary skipped", { n: fmt(res.binary) }));
    if (res.too_big) skips.push(T("{n} too large", { n: fmt(res.too_big) }));
    if (res.unreadable) skips.push(fmt(res.unreadable) + " " + T("unreadable"));
    if (res.header_only) skips.push(T("{n} media skipped", { n: fmt(res.header_only) }));
    if (res.no_reader) skips.push(T("{n} documents not read", { n: fmt(res.no_reader) }));
    if (res.locked) skips.push(T("{n} locked", { n: fmt(res.locked) }));
    if (res.offline) skips.push(T("{n} not downloaded", { n: fmt(res.offline) }));
    if (res.no_text) skips.push(T("{n} without text", { n: fmt(res.no_text) }));
    if (res.broken) skips.push(T("{n} would not open", { n: fmt(res.broken) }));
    const sortBtn = (key, msgid, num) => {
      const on = sortKey === key;
      const arr = on ? ' <span class="arrow">' + (sortDesc ? "▾" : "▴") + "</span>" : "";
      return '<button type="button" class="' + (num ? "num " : "") + (on ? "on" : "") +
        '" data-gsort="' + key + '">' + escape_(T(msgid)) + arr + "</button>";
    };
    const head = '<div class="grep-top"><div class="grep-sum">' +
      "<span>" + escape_(T("{f} files · {s} scanned · {b} read", {
        f: fmt(GREP.files.length), s: fmt(res.scanned || 0), b: bytes(res.read || 0),
      })) + " · " + MS.format((res.took_us || 0) / 1000) + " ms</span>" +
      (skips.length ? '<span class="gs-skip">' + escape_(skips.join(" · ")) + "</span>" : "") +
      (GREP.next != null ? '<button type="button" class="gs-more" id="grep-more">' + escape_(T("Read more")) + " →</button>" : "") +
      "</div>" +
      '<div class="grep-cols grep-thead">' +
      '<span></span>' +
      sortBtn("name", "Name", false) +
      sortBtn("path", "Location", false) +
      '<span class="gc-kind">' + sortBtn("kind", "Type", false) + "</span>" +
      sortBtn("modified", "Modified", false) +
      sortBtn("size", "Size", true) +
      '<span style="text-align:right">#</span>' +
      "</div></div>";
    const cards = !GREP.files.length
      ? '<div class="empty">' + escape_(T("No files held that pattern in the scanned batch.")) + "</div>"
      : GREP.files.map((gf, fi) => {
          const slash = gf.path.lastIndexOf("/");
          const nm = slash >= 0 ? gf.path.slice(slash + 1) : gf.path;
          const dir = slash > 0 ? gf.path.slice(0, slash) : "/";
          const ktok = GLYPHS.has(gf.kind) ? gf.kind : "file";
          const kword = KIND_KEY[gf.kind] || T("File");
          const whenStr = gf.mtime ? ago(gf.mtime) : "—";
          const whenTip = gf.mtime ? exact(gf.mtime) : "";
          const szStr = gf.size >= 0 ? bytes(gf.size) : "—";
          const lines = (gf.hits || []).map((h) =>
            '<div class="grep-line" data-gfi="' + fi + '">' +
            '<span class="gl-no">' + (h.line != null ? h.line : "@" + h.offset) + "</span>" +
            '<span class="gl-txt">' + segsHtml(h) + "</span></div>"
          ).join("");
          return '<div class="grep-card" data-gfi="' + fi + '">' +
            '<div class="grep-cols gc-head" data-gfi="' + fi + '" data-open="1">' +
            '<span class="gc-ico k-' + ktok + '"></span>' +
            '<span class="gc-nm" title="' + escape_(nm) + '">' + escape_(nm) + "</span>" +
            '<span class="gc-dir" data-greveal="' + fi + '" title="' + escape_(dir) + '">' + escape_(homeForm(dir)) + "</span>" +
            '<span class="gc-kind">' + escape_(kword) + "</span>" +
            '<span class="gc-when" title="' + escape_(whenTip) + '">' + escape_(whenStr) + "</span>" +
            '<span class="gc-sz">' + escape_(szStr) + "</span>" +
            '<span class="gc-cnt">' + fmt((gf.hits || []).length) + (gf.more ? "+" : "") + "</span></div>" +
            lines +
            (gf.more ? '<div class="grep-more-note">' + escape_(T("More matches in file…")) + "</div>" : "") +
            "</div>";
        }).join("");
    grepView.innerHTML = head + cards;
  }

  /* A found line's parts, each with whether it is a find: the service's
     `segs`, or the three pieces an older one sends. */
  function lineSegs(found) {
    if (found.segs && found.segs.length) return found.segs;
    return [[found.pre || "", false], [found.hit || "", true], [found.post || "", false]]
      .filter(([t]) => t !== "");
  }

  /* The same, as HTML for the grep panel's cards: escaped, every find in a
     `<mark>`, the part before the first find cut to its last 48. */
  function segsHtml(h) {
    return lineSegs(h).map(([text, isFind], k) => {
      const t = k === 0 && !isFind ? text.slice(-48) : text;
      return isFind ? "<mark>" + escape_(t) + "</mark>" : escape_(t);
    }).join("");
  }

  function applySortChoice(nextKey) {
    if (sortKey === nextKey) sortDesc = !sortDesc;
    else { sortKey = nextKey; sortDesc = ["modified", "created", "accessed", "size", "disk"].includes(sortKey); }
    keep({ sort: sortKey, descending: sortDesc });
    render();
    if (!grepBar.hidden && grepInput.value) runGrep(false);
  }

  grepView.addEventListener("click", (e) => {
    const sb = e.target.closest("[data-gsort]");
    if (sb) { applySortChoice(sb.dataset.gsort); return; }
    if (e.target.closest("#grep-more")) { grepSoon(true); return; }
    const rev = e.target.closest("[data-greveal]");
    if (rev) {
      const gf = GREP.files[+rev.dataset.greveal];
      if (gf) launch(gf.path, "folder");
      return;
    }
    const row = e.target.closest("[data-gfi]");
    const gf = row && GREP.files[+row.dataset.gfi];
    if (!gf) return;
    const slash = gf.path.lastIndexOf("/");
    const dir = slash > 0 ? gf.path.slice(0, slash) : "/";
    const nm = slash >= 0 ? gf.path.slice(slash + 1) : gf.path;
    setPeek(true, false);
    drawPeek({
      path: dir,
      name: nm,
      is_dir: false,
      size: gf.size || 0,
      mtime: gf.mtime || now,
      kind: KIND_KEY[gf.kind] || "",
      ktoken: gf.kind || "file",
      found: gf.hits,
    });
  });
  grepView.addEventListener("contextmenu", (e) => {
    const row = e.target.closest("[data-gfi]");
    const gf = row && GREP.files[+row.dataset.gfi];
    if (!gf) return;
    openRowMenu(e, gf.path, { is_dir: false, size: gf.size || 0 }, () => runGrep(false));
  });
  grepView.addEventListener("dblclick", (e) => {
    const row = e.target.closest("[data-gfi]");
    const gf = row && GREP.files[+row.dataset.gfi];
    if (gf) launch(gf.path, "file");
  });
  grepInput.addEventListener("input", () => { drawGrepShadow(); grepLaunchSync(); holdTab(); grepSoon(false); });
  grepInput.addEventListener("keydown", (e) => {
    if (grepLaunchKey(e)) return;
    if (grepPatternKey(e)) return;
    if (e.key === "Escape") { e.preventDefault(); setGrepOpen(false); }
    else if (e.key === "Enter") { e.preventDefault(); runGrep(false); }
  });
  document.querySelector(".grep-seg").addEventListener("click", (e) => {
    const b = e.target.closest("[data-gmode]");
    if (!b) return;
    GREP.mode = b.dataset.gmode;
    for (const x of document.querySelectorAll("[data-gmode]")) x.setAttribute("aria-pressed", String(x === b));
    drawGrepShadow();
    grepSoon(false);
  });
  grepCaseBtn.addEventListener("click", () => {
    GREP.exact = !GREP.exact;
    grepCaseBtn.setAttribute("aria-pressed", String(GREP.exact));
    grepSoon(false);
  });
  grepBinBtn.addEventListener("click", () => {
    GREP.binary = !GREP.binary;
    grepBinBtn.setAttribute("aria-pressed", String(GREP.binary));
    grepSoon(false);
  });
  const grepMarkupBtn = document.getElementById("grep-markup");
  grepMarkupBtn.addEventListener("click", () => {
    GREP.markup = !GREP.markup;
    grepMarkupBtn.setAttribute("aria-pressed", String(GREP.markup));
    grepSoon(false);
  });
  document.getElementById("grep-close").addEventListener("click", () => setGrepOpen(false));
  document.getElementById("q-grep").addEventListener("click", () => setGrepOpen(grepBar.hidden));

  function openOrFocusLensField(fieldName) {
    q.focus();
    const v = q.value;
    const found = terms(v).find(([a, b]) => {
      const sp = fieldSplit(v.slice(a, b).replace(/^!/, ""));
      const f = sp && LENS_FIELDS[fold(sp[0])];
      return f === fieldName || (fieldName === "under" && f === "parent");
    });
    if (found) {
      const end = v[found[1] - 1] === '"' ? found[1] - 1 : found[1];
      q.setSelectionRange(end, end);
      paint(q.value);
      lensSync(true);
      return;
    }
    const lead = v.length && !/\s$/.test(v) ? " " : "";
    const add = fieldName + ":";
    q.value = v + lead + add;
    q.setSelectionRange(q.value.length, q.value.length);
    render();
    lensSync(true);
  }
  document.getElementById("q-loc").addEventListener("mousedown", (e) => { e.preventDefault(); openOrFocusLensField("under"); });
  document.getElementById("q-time").addEventListener("mousedown", (e) => { e.preventDefault(); openOrFocusLensField("dm"); });
  document.getElementById("q-fuzzy").addEventListener("mousedown", (e) => {
    e.preventDefault();
    q.focus();
    const t = lensTerm(q.value, q.selectionStart) || terms(q.value).map(([a, b]) => lensTerm(q.value, b)).find((x) => x && !x.named && x.value.length >= 2);
    if (t && lensKindOf(t) === "word") {
      q.setSelectionRange(t.b, t.b);
      paint(q.value);
      lensSync(true);
      return;
    }
    openOrFocusLensField("fuzzy");
  });
  document.addEventListener("keydown", (e) => {
    if ((e.ctrlKey || e.metaKey) && !e.altKey && (e.key === "g" || e.key === "G" || (e.shiftKey && (e.key === "f" || e.key === "F")))) {
      e.preventDefault();
      setGrepOpen(grepBar.hidden || document.activeElement !== grepInput ? true : false);
    }
  });

