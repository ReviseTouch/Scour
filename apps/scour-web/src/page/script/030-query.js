  /* Two or more letters and nothing else, so `C:/Users` is a path. */
  function fieldSplit(t) {
    const i = t.indexOf(":");
    if (i < 0) return null;
    const name = t.slice(0, i);
    if ([...name].length < 2 || !/^\p{L}+$/u.test(name)) return null;
    return [name, t.slice(i + 1)];
  }

  /* Terms and the whitespace between, without splitting a quoted run. */
  function tokenise(text) {
    const out = [];
    let i = 0;
    while (i < text.length) {
      const start = i;
      const space = /\s/.test(text[i]);
      let quoted = false;
      while (i < text.length) {
        const c = text[i];
        if (c === '"') quoted = !quoted;
        else if (!quoted && /\s/.test(c) !== space) break;
        i++;
      }
      out.push(text.slice(start, i));
    }
    return out;
  }

  /* Cut the query into roles; the pieces cover it byte for byte. */
  function spans(text) {
    const out = [];
    const toks = tokenise(text).filter(Boolean);
    /* A list is a context: in `ext:rs ; toml` the spaces are separators. */
    let list = null, expectValue = false;
    const openList = (t) => {
      const f = fieldOf(t);
      return isListField(f) && !t.includes("|") ? f : null;
    };
    toks.forEach((tok, i) => {
      if (/^\s+$/.test(tok)) {
        const next = toks.slice(i + 1).find((t) => !/^\s+$/.test(t));
        const joins = list && next && canJoinList(next) && (expectValue || next.startsWith(";"));
        out.push([joins ? "sep" : "space", tok]);
        if (!joins) { list = null; expectValue = false; }
        return;
      }
      if (list && canJoinList(tok) && (expectValue || tok.startsWith(";"))) {
        let rest = tok;
        if (rest.startsWith(";")) { out.push(["sep", ";"]); rest = rest.slice(1); expectValue = true; }
        if (rest) expectValue = rest.endsWith(";");
        valueSpans(out, rest, list);
        return;
      }
      if (tok === "|") { out.push(["or", tok]); list = null; expectValue = false; return; }
      if (tok === ";") { out.push(["space", tok]); list = null; expectValue = false; return; }
      if (tok === "!") { out.push(["not", tok]); list = null; expectValue = false; return; }
      const pieces = cutSemicolons(tok);
      pieces.forEach((piece, pIdx) => {
        if (pIdx > 0) out.push(["space", ";"]);
        if (!piece) return;
        const quoted = piece.startsWith('"');
        piece.split("|").forEach((alt, j) => {
          if (j > 0) out.push([quoted ? "phrase" : "or", "|"]);
          alternative(out, alt);
        });
      });
      const tail = pieces[pieces.length - 1] || "";
      list = openList(tail);
      expectValue = !!list && tail.endsWith(";");
    });
    return out;
  }

  function fieldOf(tok) {
    if (tok.startsWith('"')) return null;
    const sp = fieldSplit(tok.startsWith("!") ? tok.slice(1) : tok);
    return sp ? field(sp[0]) : null;
  }

  function alternative(out, text) {
    let rest = text;
    if (rest.startsWith("!")) { out.push(["not", "!"]); rest = rest.slice(1); }
    if (!rest) return;
    const split = fieldSplit(rest);
    if (!split) return plain(out, rest);
    const [name, value] = split;
    const f = field(name);
    if (!f) { out.push(["unknown", rest]); return; }
    out.push(["field", name]);
    out.push(["colon", ":"]);
    valueSpans(out, value, f);
  }

  function valueSpans(out, value, f) {
    if (!value) return;                       // being typed — not yet wrong
    if (!accepts(f, value)) { out.push(["bad", value]); return; }
    let v = value;
    if (f.takes === "size" || f.takes === "time") {
      const m = /^(<=|>=|<|>|=)/.exec(v);
      if (m) { out.push(["cmp", m[1]]); v = v.slice(m[1].length); }
    }
    if (isListField(f)) {
      v.split(";").forEach((part, i) => {
        if (i > 0) out.push(["sep", ";"]);
        if (part) out.push(["value", part]);
      });
      return;
    }
    if (v) out.push(["value", v]);
  }

  function plain(out, text) {
    if (text.startsWith('"')) {
      out.push(["quote", '"']);
      const closed = text.length > 1 && text.endsWith('"');
      const inner = text.slice(1, closed ? -1 : undefined);
      if (inner) out.push(["phrase", inner]);
      if (closed) out.push(["quote", '"']);
      return;
    }
    out.push([/[*?]/.test(text) ? "glob" : "text", text]);
  }

  /* Term boundaries from the colouring's own spans, never a second
     tokenisation. `|` is a combiner between two terms, not part of either,
     and trailing list separators (`kind:doc ;`) stay outside the pill. */
  function terms(text) {
    const out = [];
    let at = 0, start = null, end = null;
    for (const [role, t] of spans(text)) {
      if (role === "space" || role === "or") {
        if (start !== null && end !== null) out.push([start, end]);
        start = null;
        end = null;
      } else if (role !== "sep") {
        if (start === null) start = at;
        end = at + t.length;
      }
      at += t.length;
    }
    if (start !== null && end !== null) out.push([start, end]);
    return out;
  }

  /* Where a term is drawn: the coloured layer holds one element per term, so
     its boxes are the browser's own, across a wrapped line as well. */
  function termEl(a) { return shadow.querySelector('.term[data-a="' + a + '"]'); }

  function termAt(clientX, clientY) {
    for (const el of shadow.querySelectorAll(".term")) {
      for (const r of el.getClientRects()) {
        if (clientX >= r.left - 2 && clientX <= r.right + 2 &&
            clientY >= r.top - 3 && clientY <= r.bottom + 3) return [+el.dataset.a, +el.dataset.b];
      }
    }
    return null;
  }

  const acts = document.getElementById("qacts");
  const hintBox = document.getElementById("hints");
  const queryHeader = document.querySelector(".query");
  const qfield = q.parentElement;
  let hoverTerm = null;

  /* A plain word long enough to be misspelt can be read a letter off. */
  function fuzzyable(raw) {
    const body = raw.replace(/^!/, "");
    if (/^fuzzy:/i.test(body)) return "off";
    return /^[^\s:"*?/|()]{3,}$/u.test(body) ? "on" : null;
  }

  function markHover() {
    for (const el of shadow.querySelectorAll(".term.hov")) el.classList.remove("hov");
    const el = hoverTerm && termEl(hoverTerm[0]);
    if (el) el.classList.add("hov");
  }

  function drawActs(term) {
    hoverTerm = term;
    /* The capsule under the pointer opens room at its right end for the
       buttons, so they stand over nothing and a value can still be pressed:
       marked first, measured after. */
    markHover();
    const hovEl = term && termEl(term[0]);
    const rects = term ? [...(hovEl?.getClientRects() || [])] : [];
    if (!term || !rects.length || term[1] > q.value.length) {
      hoverTerm = null;
      acts.innerHTML = "";
      markHover();
      return;
    }
    const raw = q.value.slice(term[0], term[1]);
    const negated = raw.startsWith("!");
    const fz = fuzzyable(raw);
    acts.innerHTML =
      '<div class="pair">' +
      (fz ? '<button type="button" class="fz" aria-pressed="' + (fz === "off") + '">≈</button>' : "") +
      '<button type="button" class="neg">' + (negated ? "+" : "!") + "</button>" +
      '<button type="button" class="drop">✕</button></div>';
    const pair = acts.firstElementChild;
    const [neg, drop] = pair.querySelectorAll(".neg, .drop");
    neg.title = negated ? T("Stop excluding this") : T("Exclude this term");
    drop.title = T("Remove this term");
    const fzb = pair.querySelector(".fz");
    if (fzb) fzb.title = fz === "off" ? T("Read it as typed") : T("Find it a letter off too");
    /* Over the right end of the term's last line, inside the field. */
    const box = qfield.getBoundingClientRect();
    const last = rects[rects.length - 1];
    const w = pair.offsetWidth;
    const mid = last.top + last.height / 2 - box.top;
    /* Above the term, a capsule's as a word's: nothing is laid over what it
       holds, and nothing in the line moves. */
    pair.style.left = Math.max(0, Math.min(last.right - box.left - w + 4, box.width - w)) + "px";
    pair.style.top = (mid - 18 - 24) + "px";
    markHover();
  }

  function editTerm(term, replacement) {
    const [a, b] = term;
    let before = q.value.slice(0, a), after = q.value.slice(b);
    if (!replacement) {
      before = before.replace(/\s*\|\s*$/, (m) => (after.trim() ? " " : ""));
      if (!before.trim()) after = after.replace(/^\s*\|\s*/, "");
      if (before.endsWith(" ") && after.startsWith(" ")) after = after.slice(1);
      else if (before.endsWith(" ") && !after) before = before.slice(0, -1);
      else if (!before && after.startsWith(" ")) after = after.slice(1);
    }
    q.value = before + replacement + after;
    const caret = (before + replacement).length;
    q.setSelectionRange(caret, caret);
    render();
    hideHints();
    lensClose();
    // Kept under the pointer, so a second press undoes the first in place.
    if (replacement) drawActs([before.length, caret]);
    else drawActs(null);
  }

  queryHeader.addEventListener("mousemove", (e) => {
    if (acts.contains(e.target)) return;
    // Between a term and its buttons the pointer crosses air: keep the pair.
    const pair = acts.firstElementChild;
    if (hoverTerm && pair) {
      const p = pair.getBoundingClientRect();
      if (e.clientX >= p.left - 8 && e.clientX <= p.right + 8 &&
          e.clientY >= p.top - 4 && e.clientY <= p.bottom + 10) return;
    }
    const t = termAt(e.clientX, e.clientY);
    const same = t && hoverTerm && t[0] === hoverTerm[0] && t[1] === hoverTerm[1];
    if (!same) drawActs(t);
  });
  q.addEventListener("input", () => drawActs(null));
  q.addEventListener("keydown", () => drawActs(null));
  queryHeader.addEventListener("mouseleave", () => drawActs(null));
  q.addEventListener("scroll", () => drawActs(null));

  acts.addEventListener("mousedown", (e) => {
    const b = e.target.closest("button");
    if (!b || !hoverTerm) return;
    e.preventDefault();
    const [a, end] = hoverTerm;
    const term = q.value.slice(a, end);
    if (b.classList.contains("drop")) return editTerm(hoverTerm, "");
    if (b.classList.contains("fz")) {
      const neg = term.startsWith("!") ? "!" : "";
      const body = term.replace(/^!/, "");
      return editTerm(hoverTerm, neg + (/^fuzzy:/i.test(body) ? body.slice(6) : "fuzzy:" + body));
    }
    // Negation toggles rather than accumulating: `!!a` means nothing.
    editTerm(hoverTerm, term.startsWith("!") ? term.slice(1) : "!" + term);
  });

  /* The roles that make a query worth reading back: anything here means the
     text is more than words to look for. */
  const TELLING = new Set([
    "field", "colon", "cmp", "quote", "phrase", "glob", "regex", "not", "or",
    "unknown_field", "bad_value",
  ]);

  /* Words written as operators that this language does not read as any: the
     one case where a query is wrong and nothing is marked wrong. */
  const MISTAKEN = new Set(["or", "and", "not", "ve", "veya", "değil"]);

  /* Which roles are punctuation rather than something typed to search for.
     The engine's own list is `Role::is_syntax`. */
  const SYNTAX = new Set(["colon", "sep", "quote", "cmp", "not", "or"]);

  /* The engine says and the page draws: spans, sentence and completions from
   * one `explain` call, and this file maps role to colour. */
  let readGeneration = 0;
  let LAST_SPANS = [];
  /* The reading this keystroke asked for. Under a millisecond on the service,
     but it is one of the six connections, shared with the search. */
  let reading = null;

  function refreshReading(text) {
    const mine = ++readGeneration;
    // In bytes: see `jsIndexByByte`. A code-unit index puts the caret inside a
    // character as soon as one `İ` precedes it, and the service slices there.
    const where = document.activeElement === q ? (q.selectionStart ?? null) : null;
    const caret = where === null ? null : bytesBefore(text, where);
    if (reading) reading.abort();
    reading = new AbortController();
    SERVICE.explain(text, caret, reading.signal)
      .then((res) => {
        if (mine !== readGeneration) return;
        LAST_SPANS = res.spans || [];
        paint(text);
        qread.textContent = res.description || "";
        const wrong = LAST_SPANS.some(
          (s) => s.role === "unknown_field" || s.role === "bad_value",
        );
        offerFix(text);
        drawNarrow();
        qread.classList.toggle("warn", wrong);
        /* Shown only when it has something to say: any part of the query
         * more than a plain word, or wrong. */
        // Byte offsets, so through the map, as `paintSpans` does.
        const js = jsIndexByByte(text);
        qread.hidden =
          !wrong &&
          !LAST_SPANS.some((s) => TELLING.has(s.role)) &&
          !LAST_SPANS.some(
            (s) =>
              s.role === "text" &&
              MISTAKEN.has(text.slice(js(s.start), js(s.start + s.len)).toLowerCase()),
          );
        drawNarrow();
        /* The reading lives in an `auto` grid row, so a longer one wraps after
           the list has painted, with no resize or scroll to notice it. */
        repaint(true);
      })
      .catch((e) => {
        // Let go of by the next keystroke: the normal case while typing.
        if (e.name === "AbortError") return;
        console.error("scour: query explanation did not draw:", e);
      });
  }

  /* A field name one letter off a real one — `siz:`, `udner:` — is searched
     for as text. The reading says so; this offers the spelling it meant. */
  function nearField(name) {
    const n = fold(name);
    const one = (a, b) => {
      if (Math.abs(a.length - b.length) > 1 || a === b) return false;
      let i = 0;
      while (i < a.length && a[i] === b[i]) i++;
      if (a.length === b.length) return a.slice(i + 1) === b.slice(i + 1) ||
        (a[i] === b[i + 1] && a[i + 1] === b[i] && a.slice(i + 2) === b.slice(i + 2));
      return a.length > b.length ? a.slice(i + 1) === b.slice(i) : a.slice(i) === b.slice(i + 1);
    };
    const f = FIELDS.find((x) => one(n, x.name)) || FIELDS.find((x) => x.alias.some((a) => one(n, a)));
    return f ? f.name : null;
  }
  function offerFix(text) {
    for (const [a, b] of terms(text)) {
      const raw = text.slice(a, b);
      const body = raw.replace(/^!/, "");
      const sp = body.startsWith('"') ? null : fieldSplit(body);
      if (!sp || field(sp[0])) continue;
      const near = nearField(sp[0]);
      if (!near) continue;
      const fixed = (raw.startsWith("!") ? "!" : "") + near + ":" + sp[1];
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "fix";
      btn.textContent = T("mean {f}?", { f: near + ":" });
      btn.addEventListener("mousedown", (e) => {
        e.preventDefault();
        const at = q.value.indexOf(raw);
        if (at < 0) return;
        q.value = q.value.slice(0, at) + fixed + q.value.slice(at + raw.length);
        q.setSelectionRange(at + fixed.length, at + fixed.length);
        render();
      });
      qread.append(btn);
      return;
    }
  }

  /* Role → colour is in the style sheet, keyed on the role as the engine
     spells it. A role with no rule inherits the layer's own colour. */

  /* Bytes against UTF-16 code units — `İ` is one unit and two bytes —
   * reconciled here and nowhere else. Built per call. */
  const ENCODER = new TextEncoder();
  const bytesBefore = (text, i) => ENCODER.encode(text.slice(0, i)).length;

  function jsIndexByByte(text) {
    const at = new Map([[0, 0]]);
    let b = 0;
    for (let i = 0; i < text.length; ) {
      const cp = text.codePointAt(i);
      const wide = cp > 0xffff;
      b += cp < 0x80 ? 1 : cp < 0x800 ? 2 : wide ? 4 : 3;
      i += wide ? 2 : 1;
      at.set(b, i);
    }
    /* An offset that is not a boundary means the two are looking at different
       strings. The end still draws; `undefined` slices from `NaN`. */
    return (off) => (at.has(off) ? at.get(off) : text.length);
  }

  function paintSpans(text) {
    if (!LAST_SPANS.length) return [["text", text, false]];
    const js = jsIndexByByte(text);
    const out = [];
    let at = 0;
    for (const s of LAST_SPANS) {
      const start = js(s.start);
      const end = js(s.start + s.len);
      if (start > at) out.push(["text", text.slice(at, start), false]);
      const run = text.slice(start, end);
      // A quoted value's quotes are the capsule's frame, drawn as its label.
      const m = (s.role === "value" || s.role === "phrase") && /^(")(.*?)(")?$/s.exec(run);
      if (m && run.length > 1) {
        out.push(["quote", m[1], !!s.not]);
        if (m[2]) out.push([s.role, m[2], !!s.not]);
        if (m[3]) out.push(["quote", m[3], !!s.not]);
      } else {
        out.push([s.role, run, !!s.not]);
      }
      at = end;
    }
    if (at < text.length) out.push(["text", text.slice(at), false]);
    return out;
  }

  /* What a term is drawn as. A filter is a capsule; an excluded word too; a
     plain word, and a term the parser cannot read, are not — the wavy line
     is what says the second. */
  function termClass(raw) {
    const neg = raw.startsWith("!");
    const body = neg ? raw.slice(1) : raw;
    const sp = body.startsWith('"') ? null : fieldSplit(body);
    const f = sp && field(sp[0]);
    if (sp && !f) return "word";
    /* A value that cannot be read yet keeps its capsule — a filter being
       written — and the wavy line on its value says the rest. */
    if (f && sp[1] && f.takes !== "none" && !accepts(f, sp[1])) return "cap unread";
    // A filter with nothing after its colon is left out of the search: drawn
    // idle until it holds something.
    if (f && f.takes !== "none" && !(sp[1] || "").replace(/"/g, "").trim()) return "cap idle";
    if (f) return "cap" + (f.name === "content" ? " read" : "") + (neg ? " neg" : "");
    return neg ? "cap neg" : "word";
  }

  /* What an empty filter asks for, said beside it. */
  function capHint(raw) {
    const body = raw.startsWith("!") ? raw.slice(1) : raw;
    const sp = fieldSplit(body);
    const f = sp && field(sp[0]);
    const id = { dm: "when?", dc: "when?", da: "when?", size: "how big?", kind: "which type?",
      ext: "pdf, rs…", under: "which folder?", parent: "which folder?",
      content: "text inside the files" }[f ? f.name : ""];
    return id ? T(id) : "";
  }

  /* A filter with nothing after its colon, which the caret is not in: where
     it is, or null. */
  function emptyFilter(v, caret) {
    for (const [a, b] of terms(v)) {
      if (!/\bidle\b/.test(termClass(v.slice(a, b)))) continue;
      if (caret == null || !(caret > a && caret <= b)) return [a, b];
    }
    return null;
  }

  /* A filter with nothing in it does not stay behind: leaving it takes it
     out. The space in front of it stays, so the caret is left outside the
     term before it. True when one went. */
  function dropEmpty(stayingIn) {
    const v = q.value;
    const t = emptyFilter(v, stayingIn);
    if (!t) return false;
    const [a, b] = t;
    const cur = q.selectionStart;
    let rest = v.slice(b);
    const head = v.slice(0, a);
    if ((!head || head.endsWith(" ")) && rest.startsWith(" ")) rest = rest.slice(1);
    const next = head + rest;
    const caret = cur > b ? cur - (v.length - next.length) : Math.min(cur, a);
    lensClose();
    q.value = next;
    q.setSelectionRange(caret, caret);
    render();
    return true;
  }

  function capHeadOf(text, a, b) {
    const raw = text.slice(a, b);
    const neg = raw.startsWith("!");
    const body = neg ? raw.slice(1) : raw;
    const sp = body.startsWith('"') ? null : fieldSplit(body);
    if (!sp || !field(sp[0])) return -1;
    return a + (neg ? 1 : 0) + sp[0].length + 1;
  }

  /* The pictures a capsule's header is drawn as, on the capsule's own tint:
     the same strokes the window draws. */
  const SVG = (d) => '<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">' + d + "</svg>";
  const FOLDER_ART = SVG('<path d="M2.5 13V5.5a1 1 0 0 1 1-1h3l1 1.5h5a1 1 0 0 1 1 1V13z"/>');
  const FIELD_ART = {
    under: FOLDER_ART, parent: FOLDER_ART,
    dm: SVG('<circle cx="8" cy="8" r="5.5"/><path d="M8 5v3l2 1.5"/>'),
    kind: SVG('<path d="M8 2.2l3 5H5z"/><circle cx="4.8" cy="11.2" r="2.3"/><rect x="9.3" y="9" width="4.5" height="4.5" rx="0.8"/>'),
    ext: SVG('<path d="M2.5 3.5v4.1l6 6 5-5-6-6H3.5a1 1 0 0 0-1 1z"/><circle cx="5.5" cy="5.5" r="1"/>'),
    size: SVG('<path d="M2.5 6V2.5H6M10 2.5h3.5V6M13.5 10v3.5H10M6 13.5H2.5V10"/><rect x="5.5" y="5.5" width="5" height="5" rx="0.8"/>'),
    content: SVG('<path d="M3 4h10M3 7h10M3 10h6"/><circle cx="11.5" cy="11.5" r="1.8"/><path d="M12.8 12.8l1.2 1.2"/>'),
    fuzzy: SVG('<path d="M2.5 6.5c1.4-1.4 2.6-1.4 4 0s2.6 1.4 4 0 2.1-1 3-.6"/><path d="M2.5 10.5c1.4-1.4 2.6-1.4 4 0s2.6 1.4 4 0 2.1-1 3-.6"/>'),
  };
  const FIELD_LABEL = { under: "Location", parent: "Location", dm: "Modified", kind: "Type", ext: "Extension", size: "Size", content: "Inside" };
  const fieldLabel = (f) => T(FIELD_LABEL[f] || f);

  /* The canonical name of the field a capsule is for, or null. */
  function capField(text, a, b) {
    const raw = text.slice(a, b);
    const sp = fieldSplit(raw.startsWith("!") ? raw.slice(1) : raw);
    const f = sp && field(sp[0]);
    return f ? f.name : null;
  }

  /* What a filter holds, in the words its lens offers it in: `7d` is "last 7
     days", `doc` "Document". Empty when there are none, and the letters show. */
  function saidValue(text, a, b, fld) {
    const raw = text.slice(a, b);
    const sp = fieldSplit(raw.startsWith("!") ? raw.slice(1) : raw);
    const v = ((sp && sp[1]) || "").replace(/^"|"$/g, "");
    if (!v) return "";
    try {
      const band = (list) => { const m = list.find((x) => x[0] === v); return m ? T(m[1]) : ""; };
      if (fld === "dm") return band(LENS_TIMES);
      if (fld === "size") return band(LENS_SIZES);
      if (fld === "kind" && KIND_VALUES.includes(v)) return KIND_KEY[v] || "";
    } catch (_) { /* the lists are not made yet: the letters show */ }
    return "";
  }

  /* Every text node of the drawn layer, in order, with where it starts in the
     query. The pictures hold no query text, so the offsets are the textarea's. */
  let SEGS = [];
  function indexSegments() {
    SEGS = [];
    let acc = 0;
    const walk = document.createTreeWalker(shadow, NodeFilter.SHOW_TEXT, {
      acceptNode: (n) => (n.parentElement && n.parentElement.closest(".art") ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT),
    });
    for (let n = walk.nextNode(); n; n = walk.nextNode()) {
      const len = n.nodeValue.length;
      SEGS.push({ node: n, start: acc, end: acc + len });
      acc += len;
    }
  }

  function charRects(seg, i, j) {
    const r = document.createRange();
    r.setStart(seg.node, i);
    r.setEnd(seg.node, j);
    return [...r.getClientRects()].filter((x) => x.width > 0.1);
  }

  /* Where the caret is drawn for an offset: the middle of the line, left of
     the character there, or right of the one before. A capsule's hidden
     letters have no width, so there it is the capsule's own boxes. */
  function caretBox(off) {
    for (const s of SEGS) {
      if (off >= s.start && off < s.end) {
        const r = charRects(s, off - s.start, off - s.start + 1)[0];
        if (r) return { x: r.left, y: r.top + r.height / 2 };
      }
    }
    for (const s of SEGS) {
      if (off > s.start && off <= s.end) {
        const r = charRects(s, off - s.start - 1, off - s.start).pop();
        if (r) return { x: r.right, y: r.top + r.height / 2 };
      }
    }
    for (const el of shadow.querySelectorAll(".term.cap")) {
      const a = +el.dataset.a, b = +el.dataset.b;
      if (off < a || off > b) continue;
      const said = el.querySelector(".said-art");
      if (said && said.offsetParent) {
        const r = said.getBoundingClientRect();
        return { x: off < b ? r.left + 10 : r.right - 11, y: r.top + r.height / 2 };
      }
      const val = el.querySelector(".val");
      if (val && val.offsetParent) {
        const r = val.getBoundingClientRect();
        return { x: r.left + (parseFloat(getComputedStyle(val).paddingLeft) || 0), y: r.top + r.height / 2 };
      }
      const r = el.getBoundingClientRect();
      return { x: r.left, y: r.top + r.height / 2 };
    }
    return null;
  }

  const decor = document.getElementById("qdecor");
  let decorAgain = false;
  /* The caret, or the selection's boxes, over the layer. */
  function drawDecor() {
    decor.innerHTML = "";
    if (document.activeElement !== q) return;
    const box = qfield.getBoundingClientRect();
    const a = q.selectionStart ?? 0, c = q.selectionEnd ?? 0;
    let html = "";
    if (a === c) {
      const p = caretBox(a);
      if (!p) return;
      /* Past three lines the layer scrolls: keep the caret in sight. */
      if (!decorAgain && (p.y - 14 < box.top || p.y + 14 > box.bottom)) {
        shadow.scrollTop += p.y - 14 < box.top ? p.y - 14 - box.top : p.y + 14 - box.bottom;
        decorAgain = true;
        drawDecor();
        decorAgain = false;
        return;
      }
      html = '<div class="qcaret" style="left:' + (p.x - box.left) + "px;top:" + (p.y - box.top - 11) + 'px;height:22px"></div>';
    } else {
      const lo = Math.min(a, c), hi = Math.max(a, c);
      const whole = [];
      for (const el of shadow.querySelectorAll(".term.cap")) {
        if (lo <= +el.dataset.a && hi >= +el.dataset.b) {
          whole.push([+el.dataset.a, +el.dataset.b]);
          for (const r of el.getClientRects()) {
            html += '<div class="qsel" style="left:' + (r.left - box.left) + "px;top:" + (r.top - box.top) +
              "px;width:" + r.width + "px;height:" + r.height + 'px"></div>';
          }
        }
      }
      for (const s of SEGS) {
        const from = Math.max(lo, s.start), to = Math.min(hi, s.end);
        if (from >= to || whole.some(([wa, wb]) => s.start >= wa && s.end <= wb)) continue;
        for (const r of charRects(s, from - s.start, to - s.start)) {
          const mid = r.top + r.height / 2;
          html += '<div class="qsel" style="left:' + (r.left - box.left) + "px;top:" + (mid - box.top - 14) +
            "px;width:" + r.width + 'px;height:28px"></div>';
        }
      }
    }
    decor.innerHTML = html;
  }

  /* The offset of the query under a point of the screen. The textarea stands
     over the layer and sees nothing of it, so it steps aside for the question. */
  function offsetAtPoint(x, y) {
    const was = q.style.pointerEvents;
    q.style.pointerEvents = "none";
    shadow.style.pointerEvents = "auto";
    const el = document.elementFromPoint(x, y);
    let pos = null;
    if (document.caretPositionFromPoint) {
      const p = document.caretPositionFromPoint(x, y);
      if (p) pos = { node: p.offsetNode, off: p.offset };
    } else if (document.caretRangeFromPoint) {
      const r = document.caretRangeFromPoint(x, y);
      if (r) pos = { node: r.startContainer, off: r.startOffset };
    }
    q.style.pointerEvents = was;
    shadow.style.pointerEvents = "";
    const art = el && el.closest && el.closest(".art");
    if (art) {
      const term = art.closest(".term");
      if (term) {
        if (art.classList.contains("said-art")) {
          const b = +term.dataset.b;
          return q.value[b - 1] === '"' ? b - 1 : b;
        }
        return +term.dataset.h;
      }
    }
    if (!pos) return q.value.length;
    let seg = SEGS.find((s) => s.node === pos.node);
    if (seg) return seg.start + Math.min(pos.off, seg.end - seg.start);
    /* An element: the point is between its children. */
    const kid = pos.node.childNodes && pos.node.childNodes[pos.off];
    if (kid) {
      seg = SEGS.find((s) => kid === s.node || (kid.contains && kid.contains(s.node)));
      if (seg) return seg.start;
    }
    const last = [...SEGS].reverse().find((s) => pos.node.contains && pos.node.contains(s.node));
    return last ? last.end : q.value.length;
  }

  function paint(text) {
    // A caret sits between characters, so both neighbours are candidates:
    // backspace takes the one behind, Delete the one ahead.
    const caret = document.activeElement === q ? (q.selectionStart ?? -1) : -1;
    const ts = terms(text);
    const locked = new Set();
    for (const [a, b] of ts) {
      const h = capHeadOf(text, a, b);
      if (h > a) {
        for (let p = a; p < h; p++) locked.add(p);
        if (text[h] === '"') locked.add(h);
        if (b > h + 1 && text[b - 1] === '"') locked.add(b - 1);
      }
    }
    const touch = new Set();
    if (caret >= 0 && q.selectionEnd === caret) {
      if (caret > 0 && !locked.has(caret - 1)) touch.add(caret - 1);
      if (caret < text.length && !locked.has(caret)) touch.add(caret);
    }
    const piece = (role, t, not, at) => {
      // An excluded run needs an element of its own, even the space inside
      // `!ext:rs; toml`: the layer's own colour means the opposite.
      const plain = role === "text" && !not;
      const cls = "r-" + role + (not ? " excluded" : "");
      if (SYNTAX.has(role) && [...t].some((_, k) => touch.has(at + k))) {
        // Only the touched character is marked: `<=` is one span, and
        // backspace takes one of its two characters.
        return [...t].map((ch, k) =>
          '<span class="' + cls + (touch.has(at + k) ? " caret-touch" : "") + '">' +
          escape_(ch) + "</span>").join("");
      }
      return plain ? escape_(t) : '<span class="' + cls + '">' + escape_(t) + "</span>";
    };
    /* The runs are the engine's; the terms around them are cut here, so a
       run that crosses a term's edge is drawn in two. A capsule is a header,
       and the value it holds; the header's letters and a value said in words
       sit in the text at no width, so every offset of the query is somewhere. */
    let k = 0, open = false, headOpen = false, valOpen = false, headAt = -1, at = 0, html = "", tm = null;
    const closeHead = () => {
      html += "</span>";
      headOpen = false;
      html += '<span class="val"' + (tm.hint ? ' data-hint="' + escape_(tm.hint) + '"' : "") + ">";
      valOpen = true;
    };
    const closeTerm = () => {
      if (headOpen) closeHead();
      if (valOpen) {
        html += "</span>";
        valOpen = false;
        if (tm.said) html += '<span class="art said-art">' + escape_(tm.said) + "</span>";
      }
      html += "</span>";
      open = false;
      tm = null;
      k++;
    };
    for (const [role, t, not] of paintSpans(text)) {
      let i = 0;
      while (i < t.length) {
        const pos = at + i;
        if (headOpen && pos >= headAt) closeHead();
        if (open && pos >= ts[k][1]) closeTerm();
        while (!open && k < ts.length && ts[k][1] <= pos) k++;
        if (!open && k < ts.length && pos >= ts[k][0]) {
          const [a, b] = ts[k];
          let cls = termClass(text.slice(a, b));
          const isCap = /\bcap\b/.test(cls);
          const edit = isCap && caret > a && caret <= b && q.selectionEnd === caret;
          if (edit) cls += " edit";
          headAt = isCap ? capHeadOf(text, a, b) : -1;
          tm = { hint: "", said: "", cap: isCap };
          let headHtml = "";
          if (isCap && headAt > a) {
            const fld = capField(text, a, b);
            const art = fld && FIELD_ART[fld];
            const neg = text[a] === "!";
            if (art) {
              headHtml = '<span class="cap-head"><span class="art head-art">' +
                (neg ? '<span class="bang">!</span>' : "") + art +
                (fld === "fuzzy" ? "" : '<span class="nm">' + escape_(fieldLabel(fld)) + "</span>") + "</span>";
              if (!edit) tm.said = saidValue(text, a, b, fld);
              if (tm.said) cls += " said";
            } else headHtml = '<span class="cap-head letters">';
            if (/\bidle\b/.test(cls)) tm.hint = capHint(text.slice(a, b));
          }
          html += '<span class="term ' + cls + '" data-a="' + a + '" data-b="' + b + '" data-h="' + headAt + '">';
          open = true;
          if (headAt > a) { html += headHtml; headOpen = true; }
          else tm = { hint: "", said: "", cap: isCap };
        }
        let next = at + t.length;
        if (headOpen) next = Math.min(next, headAt);
        if (open) next = Math.min(next, ts[k][1]);
        else if (k < ts.length) next = Math.min(next, ts[k][0]);
        html += piece(role, t.slice(i, next - at), not, pos);
        i = next - at;
      }
      at += t.length;
    }
    if (open) closeTerm();
    shadow.innerHTML = html;
    fitQuery();
    markHover();
  }

  /* The query is laid out by the browser, one to three lines of it, and the
     caret and selection are drawn over what it made. */
  function fitQuery() {
    indexSegments();
    drawDecor();
  }

