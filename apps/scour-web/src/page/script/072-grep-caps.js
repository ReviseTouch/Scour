  /* ---------------- the content field ----------------
   * The pattern drawn behind its input: every piece but the one under the
   * caret a capsule in the content colours, that one plain letters. Every
   * letter of the pattern stays in the layer once and in order — a capsule's
   * at no width, beside the art that says it — so a text offset is always
   * somewhere, as in the query bar. The text is the only model. */

  /* The piece under the caret: its letters, each operator in its colour. */
  function grepLettersHtml(chars, p) {
    const ops = new Set(p.ops);
    let out = "";
    for (let i = p.start; i < p.end; i++) {
      const c = chars[i];
      if (!ops.has(i)) { out += escape_(c); continue; }
      const role = c === "!" ? "not" : c === '"' ? "quote" : c === "~" ? "tilde" : "or";
      out += '<span class="gop ' + role + '">' + escape_(c) + "</span>";
    }
    return '<span class="glive">' + out + "</span>";
  }

  /* A piece at rest: its head, then each alternative — a phrase in its
     quotes, a forgiving one after `≈` — joined by "or". */
  function grepCapHtml(chars, p, k) {
    const raw = chars.slice(p.start, p.end).join("");
    const head = PIECE_HEAD[p.kind];
    const alt = (a, j) => {
      const w = chars.slice(a.start, a.end).join("");
      return '<span class="gw" data-alt="' + j + '">' + (a.similar ? '<i class="gsim">≈</i>' : "") +
        escape_(a.phrase ? "“" + w + "”" : w) + "</span>";
    };
    const body = p.alts.length
      ? p.alts.map((a, j) => (j ? '<span class="gor">' + escape_(T("or")) + "</span>" : "") + alt(a, j)).join("")
      : '<span class="gw">' + escape_(raw) + "</span>";
    return '<span class="gc gk-' + p.kind + (p.alts.length > 1 ? " many" : "") + '" data-k="' + k + '" data-a="' + p.start +
      '" data-b="' + p.end + '"><span class="z">' + escape_(raw) + '</span><span class="art">' +
      (head ? '<span class="gh">' + escape_(T(head)) + "</span>" : "") + '<span class="gb">' + body + "</span></span></span>";
  }

  /* The whole layer for `text` with the caret at column `caret` (-1: none,
     or a selection). A quiet "and" between two capsules; none beside the
     letters, so typing a piece never moves its own line. */
  function grepLayerHtml(text, caret) {
    const chars = Array.from(text);
    const pieces = patternPieces(text);
    const live = caret < 0 ? null : patternPieceAt(pieces, caret);
    let html = "", at = 0;
    pieces.forEach((p, k) => {
      html += escape_(chars.slice(at, p.start).join(""));
      if (k > 0 && k !== live && k - 1 !== live) html += '<span class="art gand">' + escape_(T("and")) + "</span>";
      html += k === live ? grepLettersHtml(chars, p) : grepCapHtml(chars, p, k);
      at = p.end;
    });
    return html + escape_(chars.slice(at).join(""));
  }

  /* Columns against the input's UTF-16 offsets. */
  const grepCol = (text, unit) => Array.from(text.slice(0, unit)).length;
  const grepUnit = (text, col) => Array.from(text).slice(0, col).join("").length;

  /* ---- drawn ---- */

  const grepDecor = document.getElementById("grep-decor");
  const grepActs = document.getElementById("grep-acts");
  /* Every text node of the layer outside the art, with its input offsets. */
  let GSEGS = [];

  function drawGrepShadow() {
    const text = grepInput.value;
    const raw = GREP.mode !== "text";
    grepBar.classList.toggle("raw", raw);
    if (raw) { grepShadow.textContent = ""; GSEGS = []; grepDecor.innerHTML = ""; return; }
    const focused = document.activeElement === grepInput;
    const s = grepInput.selectionStart ?? 0;
    const caret = focused && s === grepInput.selectionEnd ? grepCol(text, s) : -1;
    grepShadow.innerHTML = grepLayerHtml(text, caret);
    GSEGS = [];
    let acc = 0;
    const walk = document.createTreeWalker(grepShadow, NodeFilter.SHOW_TEXT, {
      acceptNode: (n) => (n.parentElement && n.parentElement.closest(".art") ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT),
    });
    for (let n = walk.nextNode(); n; n = walk.nextNode()) {
      GSEGS.push({ node: n, start: acc, end: acc + n.nodeValue.length });
      acc += n.nodeValue.length;
    }
    if (GPILL) markGrepHover();
    drawGrepDecor();
  }

  function grepRects(seg, i, j) {
    const r = document.createRange();
    r.setStart(seg.node, i);
    r.setEnd(seg.node, j);
    return [...r.getClientRects()].filter((x) => x.width > 0.1);
  }

  /* Where the caret is drawn for an input offset: left of the letter there,
     right of the one before, or the start of the line. */
  function grepCaretBox(off) {
    for (const s of GSEGS) {
      if (off < s.start || off >= s.end) continue;
      const r = grepRects(s, off - s.start, off - s.start + 1)[0];
      if (r) return r.left;
    }
    for (const s of GSEGS) {
      if (off <= s.start || off > s.end) continue;
      const r = grepRects(s, off - s.start - 1, off - s.start).pop();
      if (r) return r.right;
    }
    return grepShadow.getBoundingClientRect().left - grepShadow.scrollLeft;
  }

  /* The caret, or the selection's boxes; the layer scrolled to keep the caret in sight. */
  function drawGrepDecor() {
    grepDecor.innerHTML = "";
    if (document.activeElement !== grepInput) return;
    const box = grepDecor.getBoundingClientRect();
    const a = grepInput.selectionStart ?? 0, c = grepInput.selectionEnd ?? 0;
    if (a === c) {
      let x = grepCaretBox(a);
      const sb = grepShadow.getBoundingClientRect();
      if (x > sb.right - 2 || x < sb.left) {
        grepShadow.scrollLeft += x > sb.right - 2 ? x - sb.right + 24 : x - sb.left - 24;
        x = grepCaretBox(a);
      }
      grepDecor.innerHTML = '<div class="qcaret" style="left:' + (x - box.left) + 'px;top:7px;height:22px"></div>';
      return;
    }
    const text = grepInput.value;
    const lo = Math.min(a, c), hi = Math.max(a, c);
    const boxes = [];
    for (const el of grepShadow.querySelectorAll(".gc")) {
      if (grepUnit(text, +el.dataset.a) < hi && grepUnit(text, +el.dataset.b) > lo) boxes.push(el.getBoundingClientRect());
    }
    for (const s of GSEGS) {
      const from = Math.max(lo, s.start), to = Math.min(hi, s.end);
      if (from < to) boxes.push(...grepRects(s, from - s.start, to - s.start));
    }
    grepDecor.innerHTML = boxes.map((r) => '<div class="qsel" style="left:' + (r.left - box.left) + "px;top:6px;width:" +
      r.width + 'px;height:24px"></div>').join("");
  }

  /* The input offset under a point: a capsule's end, or the letter there. */
  function grepOffsetAt(x, y) {
    grepInput.style.pointerEvents = "none";
    grepShadow.style.pointerEvents = "auto";
    const el = document.elementFromPoint(x, y);
    let pos = null;
    if (document.caretPositionFromPoint) {
      const p = document.caretPositionFromPoint(x, y);
      if (p) pos = { node: p.offsetNode, off: p.offset };
    } else if (document.caretRangeFromPoint) {
      const r = document.caretRangeFromPoint(x, y);
      if (r) pos = { node: r.startContainer, off: r.startOffset };
    }
    grepInput.style.pointerEvents = "";
    grepShadow.style.pointerEvents = "";
    const text = grepInput.value;
    const cap = el && el.closest && el.closest(".gc");
    if (cap && grepShadow.contains(cap)) return grepUnit(text, +cap.dataset.b);
    const seg = pos && GSEGS.find((s) => s.node === pos.node);
    if (seg) return seg.start + Math.min(pos.off, seg.end - seg.start);
    return x < grepShadow.getBoundingClientRect().left + 4 ? 0 : text.length;
  }

  /* The field's text changed from outside the keyboard: drawn, read again. */
  function grepSetText(value, caretUnit) {
    grepInput.value = value;
    grepInput.setSelectionRange(caretUnit, caretUnit);
    drawGrepShadow();
    grepLaunchSync();
    drawGrepMeter(true);
    grepSoon(false);
  }

  let grepDrag = null;
  grepInput.addEventListener("mousedown", (e) => {
    if (e.button !== 0 || GREP.mode !== "text") return;
    e.preventDefault();
    grepInput.focus({ preventScroll: true });
    const v = grepInput.value, off = grepOffsetAt(e.clientX, e.clientY);
    if (e.detail >= 3) { grepInput.setSelectionRange(0, v.length); return; }
    if (e.detail === 2) {
      let lo = off, hi = off;
      while (lo > 0 && !/\s/.test(v[lo - 1])) lo--;
      while (hi < v.length && !/\s/.test(v[hi])) hi++;
      grepInput.setSelectionRange(lo, hi);
      return;
    }
    const anchor = e.shiftKey ? (grepInput.selectionDirection === "backward" ? grepInput.selectionEnd : grepInput.selectionStart) : off;
    grepDrag = { anchor };
    grepInput.setSelectionRange(Math.min(anchor, off), Math.max(anchor, off), off < anchor ? "backward" : "forward");
  });
  document.addEventListener("mousemove", (e) => {
    if (!grepDrag) return;
    if (!(e.buttons & 1)) { grepDrag = null; return; }
    const off = grepOffsetAt(e.clientX, e.clientY), a = grepDrag.anchor;
    grepInput.setSelectionRange(Math.min(a, off), Math.max(a, off), off < a ? "backward" : "forward");
  });
  document.addEventListener("mouseup", () => { grepDrag = null; });
  document.addEventListener("selectionchange", () => {
    if (document.activeElement !== grepInput) return;
    drawGrepShadow();
    grepLaunchSync();
  });
  grepInput.addEventListener("focus", drawGrepShadow);
  grepInput.addEventListener("blur", () => { drawGrepShadow(); grepLaunchClose(); });
  addEventListener("resize", () => { if (!grepBar.hidden) drawGrepShadow(); });
  document.fonts.ready.then(() => { if (!grepBar.hidden) drawGrepShadow(); });

  /* ---- the pill over a capsule: the query bar's, its buttons only there ---- */

  /* The capsule under the pointer, and the alternative it points at. */
  let GPILL = null;

  function markGrepHover() {
    for (const el of grepShadow.querySelectorAll(".gc")) el.classList.toggle("hov", !!GPILL && +el.dataset.k === GPILL.k);
  }

  function grepCapAt(x, y) {
    for (const el of grepShadow.querySelectorAll(".gc")) {
      const r = el.getBoundingClientRect();
      if (x < r.left - 2 || x > r.right + 2 || y < r.top - 3 || y > r.bottom + 3) continue;
      const w = [...el.querySelectorAll(".gw[data-alt]")].find((g) => {
        const b = g.getBoundingClientRect();
        return x >= b.left - 3 && x <= b.right + 3;
      });
      return { k: +el.dataset.k, alt: w ? +w.dataset.alt : -1 };
    }
    return null;
  }

  function drawGrepPill(at) {
    GPILL = at;
    markGrepHover();
    const el = at && grepShadow.querySelector('.gc[data-k="' + at.k + '"]');
    const p = el && patternPieces(grepInput.value)[at.k];
    if (!p) { GPILL = null; grepActs.innerHTML = ""; markGrepHover(); return; }
    const pointed = p.alts[at.alt];
    const sim = pointed && !pointed.phrase ? at.alt : p.alts.findIndex((a) => !a.phrase);
    at.sim = sim;
    const not = p.kind === "not";
    const similar = sim >= 0 && p.alts[sim].similar;
    grepActs.innerHTML = '<div class="pair">' +
      (sim >= 0 ? '<button type="button" class="fz" aria-pressed="' + similar + '" title="' +
        escape_(similar ? T("Read it as typed") : T("Find it a letter off too")) + '">≈</button>' : "") +
      '<button type="button" class="neg" title="' + escape_(not ? T("Stop excluding this") : T("Exclude this term")) + '">' +
      (not ? "+" : "!") + "</button>" +
      '<button type="button" class="drop" title="' + escape_(T("Remove this term")) + '">✕</button></div>';
    const pair = grepActs.firstElementChild;
    const r = el.getBoundingClientRect(), bar = grepBar.getBoundingClientRect();
    const w = pair.offsetWidth;
    pair.style.left = Math.max(bar.left, Math.min(r.right - w + 4, bar.right - w)) + "px";
    pair.style.top = (r.top + r.height / 2 - 34) + "px";
  }

  grepBar.addEventListener("mousemove", (e) => {
    if (grepDrag || GREP.mode !== "text") return;
    const at = grepCapAt(e.clientX, e.clientY);
    if (!at) { if (GPILL) drawGrepPill(null); return; }
    if (!GPILL || GPILL.k !== at.k || GPILL.alt !== at.alt) drawGrepPill(at);
  });
  grepBar.addEventListener("mouseleave", (e) => { if (!grepActs.contains(e.relatedTarget)) drawGrepPill(null); });
  grepActs.addEventListener("mouseleave", (e) => { if (!grepBar.contains(e.relatedTarget)) drawGrepPill(null); });
  grepInput.addEventListener("keydown", () => { if (GPILL) drawGrepPill(null); });

  grepActs.addEventListener("mousedown", (e) => {
    const b = e.target.closest("button");
    if (!b || !GPILL) return;
    e.preventDefault();
    const text = grepInput.value, { k, sim } = GPILL;
    const out = b.classList.contains("drop") ? patternRemove(text, k)
      : b.classList.contains("fz") ? patternToggleSimilar(text, k, sim)
      : patternToggleNot(text, k);
    // The caret keeps its piece: it moves by what changed only when after this one.
    const p = patternPieces(text)[k];
    const caret = grepCol(text, grepInput.selectionStart ?? 0);
    const moved = caret >= p.end ? caret + Array.from(out).length - Array.from(text).length : caret;
    grepSetText(out, grepUnit(out, Math.max(0, Math.min(moved, Array.from(out).length))));
    // Kept under the pointer, so a second press undoes the first in place.
    drawGrepPill(b.classList.contains("drop") ? null : { k, alt: GPILL ? GPILL.alt : -1 });
  });

