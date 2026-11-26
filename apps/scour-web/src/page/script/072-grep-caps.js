  /* ---------------- the content field ----------------
   * The pattern drawn behind its input: every piece a capsule in the content
   * colours, an excluded one red — the one being written too, so nothing
   * moves when the caret comes or goes. An operator is not drawn as itself:
   * `!` is the "without" head, `~` the "similar" head or a ≈, quotes are
   * “ ”, `;` the "or". Every letter of the pattern stays in the layer once
   * and in order — an operator's at no width (`.z`) beside the art that says
   * it — so a text offset is always somewhere. The text is the only model. */

  const gz = (t) => (t ? '<span class="z">' + escape_(t) + "</span>" : "");

  /* Where alternative `a` begins and ends in the text, its own operators
     (`~` drawn as ≈, its quotes) inside. */
  function grepAltSpan(chars, a, mark) {
    const q = a.phrase ? 1 : 0;
    const from = a.similar && mark ? a.start - 1 - q : a.start - q;
    const closed = a.phrase && chars[a.end] === '"';
    return [from, closed ? a.end + 1 : a.end, closed];
  }

  /* One alternative: ≈, its quotes, its letters — or a slot for them. */
  function grepAltHtml(chars, a, j, mark) {
    const q = a.phrase ? 1 : 0;
    let out = "";
    if (a.similar && mark) out += gz(chars[a.start - 1 - q]) + '<i class="art gsim">≈</i>';
    if (a.phrase) out += gz('"') + '<i class="art gq">“</i>';
    out += a.start === a.end ? '<span class="art gslot" data-c="' + a.start + '"></span>'
      : escape_(chars.slice(a.start, a.end).join(""));
    if (a.phrase && chars[a.end] === '"') out += '<i class="art gq">”</i>' + gz('"');
    return '<span class="gw' + (a.start === a.end ? " hole" : "") + '" data-alt="' + j + '">' + out + "</span>";
  }

  /* A piece as its capsule: what opens it (`!`, `(`, a lone `~`) at no width
     before its head, each alternative with what joins it to the one before
     inside it, what closes it (`)`) at its end. */
  function grepCapHtml(chars, p, k, live) {
    const head = PIECE_HEAD[p.kind];
    const mark = p.kind !== "similar";
    const spans = p.alts.map((a) => grepAltSpan(chars, a, mark));
    const first = spans.length ? spans[0][0] : p.end;
    let body = "", at = first;
    p.alts.forEach((a, j) => {
      if (j) body += '<span class="art gor">' + escape_(T("or")) + "</span>";
      const alt = grepAltHtml(chars, a, j, mark);
      body += at < spans[j][0] ? alt.replace('">', '">' + gz(chars.slice(at, spans[j][0]).join(""))) : alt;
      at = spans[j][1];
    });
    return '<span class="gc gk-' + p.kind + (p.alts.length > 1 ? " many" : "") + (live ? " live" : "") +
      '" data-k="' + k + '" data-a="' + p.start + '" data-b="' + p.end + '">' +
      gz(chars.slice(p.start, first).join("")) +
      (head ? '<span class="art gh">' + escape_(T(head)) + "</span>" : "") +
      '<span class="gb">' + body + "</span>" + gz(chars.slice(at, p.end).join("")) + "</span>";
  }

  /* The whole layer for `text` with the caret at column `caret` (-1: none,
     or a selection). Spaces after a capsule are a quiet "and", its room kept
     and its word written when a capsule follows or the caret is there to
     start one; anything else outside a capsule is letters. */
  function grepLayerHtml(text, caret) {
    const chars = Array.from(text);
    const pieces = patternPieces(text);
    const live = caret < 0 ? null : patternPieceAt(pieces, caret);
    const gap = (a, b, before) => {
      const run = chars.slice(a, b).join("");
      if (!a || !pieces.length || /\S/u.test(run)) return escape_(run);
      const said = before || (caret > a && caret <= b);
      return '<span class="art gand' + (said ? "" : " mute") + '">' + escape_(T("and")) + "</span>" + gz(run);
    };
    let html = "", at = 0;
    pieces.forEach((p, k) => {
      if (at < p.start) html += gap(at, p.start, true);
      html += grepCapHtml(chars, p, k, k === live);
      at = p.end;
    });
    return at < chars.length ? html + gap(at, chars.length, false) : html;
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
      const el = n.parentElement;
      const kind = el.classList.contains("z") ? "z" : el.closest(".gw") ? "l" : "t";
      GSEGS.push({ node: n, start: acc, end: acc + n.nodeValue.length, kind, cap: el.closest(".gc") });
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

  /* Where a segment's offset `i` is drawn: an operator's at its no-width
     place, a letter's left edge, or the right edge of the one before. */
  function grepSegX(seg, i) {
    if (seg.kind === "z") return seg.node.parentElement.getBoundingClientRect().left;
    if (i < seg.end - seg.start) { const r = grepRects(seg, i, i + 1)[0]; if (r) return r.left; }
    const r = grepRects(seg, i - 1, i).pop();
    return r ? r.right : seg.node.parentElement.getBoundingClientRect().left;
  }

  /* Where the caret is drawn for an input offset: in a slot waiting for its
     word, after a word's last letter, at the end of a capsule it closes, or
     before what stands there. */
  function grepCaretBox(off) {
    const text = grepInput.value;
    const col = grepCol(text, off);
    const slot = grepShadow.querySelector('.gslot[data-c="' + col + '"]');
    if (slot) return slot.getBoundingClientRect().left + 3;
    const word = GSEGS.find((s) => s.kind === "l" && s.end === off && s.end > s.start);
    if (word) return grepSegX(word, off - word.start);
    const cap = [...grepShadow.querySelectorAll(".gc")].find((el) => grepUnit(text, +el.dataset.b) === off);
    const last = cap && GSEGS.filter((s) => s.cap === cap).pop();
    if (last) return last.kind === "z" ? last.node.parentElement.getBoundingClientRect().right : grepSegX(last, last.end - last.start);
    const at = GSEGS.find((s) => off >= s.start && off < s.end);
    if (at) return grepSegX(at, off - at.start);
    const end = GSEGS[GSEGS.length - 1];
    if (end) return end.kind === "z" ? end.node.parentElement.getBoundingClientRect().right : grepSegX(end, end.end - end.start);
    return grepShadow.getBoundingClientRect().left - grepShadow.scrollLeft;
  }

  /* The caret, or the selection's box; the layer scrolled to keep the caret in sight. */
  function drawGrepDecor() {
    grepDecor.innerHTML = "";
    if (document.activeElement !== grepInput) return;
    const box = grepDecor.getBoundingClientRect();
    const a = grepInput.selectionStart ?? 0, c = grepInput.selectionEnd ?? 0;
    const sb = grepShadow.getBoundingClientRect();
    const moving = grepInput.selectionDirection === "backward" ? a : c;
    let x = grepCaretBox(moving);
    if (x > sb.right - 2 || x < sb.left) {
      grepShadow.scrollLeft += x > sb.right - 2 ? x - sb.right + 24 : x - sb.left - 24;
      x = grepCaretBox(moving);
    }
    if (a === c) {
      grepDecor.innerHTML = '<div class="qcaret" style="left:' + (x - box.left) + 'px;top:7px;height:22px"></div>';
      return;
    }
    const lo = grepCaretBox(Math.min(a, c)), hi = grepCaretBox(Math.max(a, c));
    grepDecor.innerHTML = '<div class="qsel" style="left:' + (lo - box.left) + "px;top:6px;width:" +
      Math.max(0, hi - lo) + 'px;height:24px"></div>';
  }

  /* The input offset nearest a point: a column in a capsule as anywhere. */
  function grepOffsetAt(x) {
    const text = grepInput.value;
    const n = Array.from(text).length;
    let best = 0, gap = Infinity;
    for (let col = 0; col <= n; col++) {
      const u = grepUnit(text, col);
      const d = Math.abs(grepCaretBox(u) - x);
      if (d < gap) { gap = d; best = u; }
    }
    return best;
  }

  /* Backspace in an empty capsule, a space (it stays in its capsule), a
     quote, Tab (out of the capsule, for a new one): the pattern's own
     (`pattern::edit`). True when it was answered here. */
  const GREP_KEYS = { Backspace: patternBackspace, " ": patternSpace, '"': patternQuote, Tab: patternLeave };
  function grepPatternKey(e) {
    if (GREP.mode !== "text" || e.ctrlKey || e.altKey || e.metaKey || e.isComposing) return false;
    const answer = GREP_KEYS[e.key];
    if (!answer || (e.key === "Tab" && e.shiftKey)) return false;
    const a = grepInput.selectionStart ?? 0;
    if (a !== grepInput.selectionEnd) return false;
    const text = grepInput.value, caret = grepCol(text, a);
    const done = answer(text, caret);
    if (!done) return false;
    e.preventDefault();
    const [after, at] = done;
    if (after !== text) grepSetText(after, grepUnit(after, at));
    else { grepInput.setSelectionRange(grepUnit(text, at), grepUnit(text, at)); drawGrepShadow(); }
    return true;
  }

  /* The field's text changed from outside the keyboard: drawn, read again. */
  function grepSetText(value, caretUnit) {
    grepInput.value = value;
    grepInput.setSelectionRange(caretUnit, caretUnit);
    drawGrepShadow();
    grepLaunchSync();
    holdTab();
    drawGrepMeter(true);
    grepSoon(false);
  }

  let grepDrag = null;
  grepInput.addEventListener("mousedown", (e) => {
    if (e.button !== 0 || GREP.mode !== "text") return;
    e.preventDefault();
    grepInput.focus({ preventScroll: true });
    const v = grepInput.value, off = grepOffsetAt(e.clientX);
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
    const off = grepOffsetAt(e.clientX), a = grepDrag.anchor;
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

