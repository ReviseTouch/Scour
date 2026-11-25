  /* ---------------- the list capsule ----------------
   * A `kind:` or `ext:` term at rest drawn as the place capsule's parts are:
   * each value an inner chip — a kind by its name, an extension as written —
   * a quiet dot between, "+N" for those the line has no room for. Raw while
   * the person edits it. Every letter stays in the layer at no width, so each
   * offset is still a text offset; Backspace at a chip's edge takes that one
   * value out through `valueToggle`. */

  /* The list capsules of the last paint. */
  let LIST_DRAWN = [];

  /* Each value of a list term at [head, end]: its offsets, trimmed, empty ones left out. */
  function listValuesAt(text, head, end) {
    const out = [];
    let from = head;
    for (let i = head; i <= end; i++) {
      if (i < end && text[i] !== ";") continue;
      const raw = text.slice(from, i);
      const s = from + raw.length - raw.trimStart().length;
      if (raw.trim()) out.push({ start: s, end: s + raw.trim().length, value: raw.trim() });
      from = i + 1;
    }
    return out;
  }

  /* A value as its chip says it: a kind by its name, an extension without its dot. */
  const listLabel = (field, v) => (field === "kind" ? KIND_KEY[valueKey("kind", v)] || v : v.replace(/^\.+/, ""));

  /* The capsules of `text`'s kind and ext terms not being typed in at
     `caret` (a `Caret`, or null): as the page's own terms cut them. */
  function listCaps(text, caret) {
    const out = [];
    for (const [a, b] of terms(text)) {
      const raw = text.slice(a, b);
      const sp = raw.startsWith('"') ? null : fieldSplit(raw.replace(/^!/, ""));
      const field = sp ? LENS_FIELDS[fold(sp[0])] : null;
      if (field !== "kind" && field !== "ext") continue;
      const head = capHeadOf(text, a, b);
      const values = listValuesAt(text, head, b);
      // One term holds every chip, so resting at a chip's edge inside it is not editing it.
      if (!values.length || (caret && caret.editing && a < caret.at && caret.at <= b)) continue;
      const parts = values.map((v) => ({ ...v, label: listLabel(field, v.value) }));
      out.push({ list: true, start: a, end: b, head, field, not: raw.startsWith("!"), parts, shown: parts.length });
    }
    return out;
  }

  function listCapsDrawn(text, caret) {
    LIST_DRAWN = listCaps(text, caret);
    return LIST_DRAWN;
  }

  /* The whole capsule: the head, then each value's letters at no width, a
     shown value's chip after them, the hidden ones as one "+N". */
  function listCapHtml(text, cap) {
    const art = FIELD_ART[cap.field];
    let html = '<span class="term cap list' + (cap.not ? " neg" : "") + '" data-a="' + cap.start + '" data-b="' + cap.end +
      '" data-h="' + cap.head + '"><span class="cap-head"><span class="art head-art">' +
      (cap.not ? '<span class="bang">!</span>' : "") + art + '<span class="nm">' + escape_(fieldLabel(cap.field)) +
      "</span></span>" + placeZero(text.slice(cap.start, cap.head)) + '</span><span class="pbody">';
    let at = cap.head;
    const last = cap.parts[cap.parts.length - 1];
    cap.parts.forEach((p, k) => {
      html += placeZero(text.slice(at, p.start)) + placeZero(text.slice(p.start, p.end));
      const sep = k ? '<span class="art psep" data-at="' + p.start + '">·</span>' : "";
      if (k < cap.shown) html += sep + '<span class="art pv" data-k="' + k + '" data-at="' + p.end + '">' + escape_(p.label) + "</span>";
      else if (k === cap.shown) html += sep + '<span class="art pv more" data-at="' + last.end + '">+' + (cap.parts.length - k) + "</span>";
      at = p.end;
    });
    return html + placeZero(text.slice(at, cap.end)) + "</span></span>";
  }

  const listCapEl = (cap) => shadow.querySelector('.term.list[data-a="' + cap.start + '"]');

  /* After the layer is drawn: a capsule wider than a whole line shows fewer
     chips, the rest behind "+N"; on a line too narrow for one, "+N" alone.
     Measured against the line, not the room left on it, so a capsule that
     wraps is not folded for the line it left. */
  function listFit(text) {
    const cs = getComputedStyle(shadow);
    const line = shadow.clientWidth - (parseFloat(cs.paddingLeft) || 0) - (parseFloat(cs.paddingRight) || 0);
    for (const cap of LIST_DRAWN) {
      for (let el = listCapEl(cap); el && cap.shown > 0; el = listCapEl(cap)) {
        const body = el.querySelector(".pbody");
        const natural = el.getBoundingClientRect().width + (body ? body.scrollWidth - body.clientWidth : 0);
        if (natural <= line + 0.5) break;
        cap.shown--;
        el.outerHTML = listCapHtml(text, cap);
      }
    }
  }

  /* The drawn capsule an offset is inside, its first letter left out. */
  const listCapOf = (off) => LIST_DRAWN.find((c) => off > c.start && off <= c.end) || null;

  /* Where the caret is drawn for an offset of a list capsule: at the trailing
     edge of the value it ends or is in, or before the first. */
  function listCaret(off) {
    const cap = listCapOf(off);
    const el = cap && listCapEl(cap);
    if (!el) return null;
    const k = cap.parts.reduce((n, p, i) => (p.start < off ? i : n), -1);
    const chip = k < 0 ? el.querySelector(".pv") : k < cap.shown ? el.querySelector('.pv[data-k="' + k + '"]') : el.querySelector(".pv.more");
    const r = placeRect(chip);
    if (!r) return null;
    return { x: k < 0 ? r.left - 2 : r.right + 1, y: r.top + r.height / 2 };
  }

  /* Where the arrows stop over a list capsule at rest: the start of its values
     and each shown value's end; the hidden ones are one stop. */
  function listStops(cap) {
    const out = [cap.head];
    cap.parts.forEach((p, k) => { if (k < cap.shown || k === cap.parts.length - 1) out.push(p.end); });
    return out;
  }

  /* An arrow over a list capsule: to its next stop, resting. False when the
     key leaves the capsule and moves by a letter. */
  function listArrow(forward) {
    const c = q.selectionStart;
    if (c !== q.selectionEnd) return false;
    const cap = LIST_DRAWN.find((x) => c >= x.head && c <= x.end);
    if (!cap) return false;
    const stops = listStops(cap);
    const to = forward ? stops.find((s) => s > c) : [...stops].reverse().find((s) => s < c);
    if (to === undefined) return false;
    qEdit = null;
    q.setSelectionRange(to, to);
    lastQueryCaret = to;
    return true;
  }

  /* Backspace (`back`) at a chip's trailing edge, or Delete at its leading
     one, takes that value out of its term (`valueToggle`), never half a value.
     The caret rests at the edge of the chip before it. Null when the key is
     the text's own. */
  function listCapKey(back) {
    const v = q.value, c = q.selectionStart;
    if (c !== q.selectionEnd) return null;
    const cap = LIST_DRAWN.find((x) => c >= x.head && c <= x.end);
    if (!cap) return null;
    const edge = cap.parts.findIndex((p) => p.end === c);
    const k = back ? edge : c === cap.head ? 0 : edge < 0 ? -1 : edge + 1;
    if (k < 0 || k >= cap.parts.length) return null;
    const [out, at] = valueToggle(v, cap.field, cap.parts[k].value, cap.not);
    const next = listCaps(out, null).find((x) => x.start === cap.start && x.field === cap.field);
    if (!next) return [out, at];
    return [out, k > 0 ? next.parts[Math.min(k, next.parts.length) - 1].end : next.head];
  }

