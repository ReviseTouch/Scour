  /* ---------------- the place capsule, edited ----------------
   * The caret over the capsules: whether the person is editing (`Caret`),
   * where it is drawn, where a press lands, what the arrows, Backspace,
   * Delete and the pill's `!` do there. The text is still the only model. */

  /* The term the person is editing, by its start, and the text then: set by
     typing or a press into a folder part; gone once the caret leaves that
     term or anything else writes the text. */
  let qEdit = null;
  /* What the last press landed on: a folder part (true: edited from there),
     a disk part (false: resting), or letters (null). */
  let pressEdits = null;

  function editingAt(at) {
    const t = placeTermAt(q.value, at);
    qEdit = t ? { value: q.value, at: t.start } : null;
  }

  /* The text's caret as the model takes it. */
  function queryCaret() {
    const at = q.selectionStart ?? 0;
    const t = qEdit && q.value === qEdit.value && q.selectionEnd === at ? placeTermAt(q.value, at) : null;
    if (qEdit && !(t && t.start === qEdit.at)) qEdit = null;
    return { at, editing: !!qEdit };
  }

  q.addEventListener("input", (e) => {
    if (/^(insert|delete)/.test(e.inputType || "")) editingAt(q.selectionStart ?? 0);
  });

  /* The drawn capsule an offset is inside, its first letter left out. */
  const placeCapOf = (off) => PLACE_DRAWN.find((c) => off > c.start && off <= c.end) || null;
  const placeMembers = (cap) => cap.parts.flatMap((p) => p.spot.terms).sort((x, y) => x.start - y.start);
  const placeMemberAt = (cap, off) => placeMembers(cap).find((t) => t.start < off && off <= t.end) || null;

  function placeRect(el) {
    const r = el && el.offsetParent !== null ? el.getBoundingClientRect() : null;
    return r && r.width ? r : null;
  }

  /* The "+N" that stands for part `k`, hidden. */
  function placeMoreEl(el, cap, k) {
    const block = capBlocks(cap).find(([first, n]) => k >= first && k < first + n);
    return block ? el.querySelector('.pv.more[data-blk="' + block[0] + '"]') : null;
  }

  /* Where the caret is drawn for an offset of a capsule: among a raw part's
     letters; at a collapsed part's trailing edge; on its block's "+N". */
  function placeCaret(off) {
    const cap = placeCapOf(off);
    const el = cap && placeCapEl(cap);
    if (!el) return null;
    const k = capPartAt(cap, off), p = cap.parts[k];
    const well = p.shown && p.raw && el.querySelector('.pw[data-k="' + k + '"]');
    if (well && placeMemberAt(cap, off)) {
      for (const s of SEGS) {
        if (!well.contains(s.node)) continue;
        if (off >= s.start && off < s.end) {
          const r = charRects(s, off - s.start, off - s.start + 1)[0];
          if (r) return { x: r.left, y: r.top + r.height / 2 };
        }
        if (off > s.start && off <= s.end) {
          const r = charRects(s, off - s.start - 1, off - s.start).pop();
          if (r) return { x: r.right, y: r.top + r.height / 2 };
        }
      }
      const r = well.getBoundingClientRect();
      return { x: r.left + 7, y: r.top + r.height / 2 };
    }
    const r = placeRect(p.shown ? el.querySelector('[data-k="' + k + '"]') : placeMoreEl(el, cap, k));
    return r ? { x: r.right + 1, y: r.top + r.height / 2 } : null;
  }

  /* The boxes a selection from `lo` to `hi` lights inside the capsules: each
     collapsed part, or "+N", one of whose members it touches. */
  function placeSelRects(lo, hi) {
    const out = [];
    for (const cap of PLACE_DRAWN) {
      const el = placeCapEl(cap);
      if (!el) continue;
      cap.parts.forEach((p, k) => {
        if (p.raw || !p.spot.terms.some((t) => lo < t.end && hi > t.start)) return;
        const r = placeRect(p.shown ? el.querySelector('[data-k="' + k + '"]') : placeMoreEl(el, cap, k));
        if (r && !out.some((x) => x.left === r.left && x.top === r.top)) out.push(r);
      });
    }
    return out;
  }

  /* A press that landed among a collapsed part's hidden letters goes where a
     press on the part goes. */
  function placeSnap(off) {
    const cap = placeCapOf(off);
    if (!cap || !placeMemberAt(cap, off)) return off;
    const p = cap.parts[capPartAt(cap, off)];
    if (p.raw) return off;
    pressEdits = p.spot.tile === null;
    return placePressAt(q.value, p);
  }

  /* The capsule for an edit at the caret, read afresh. */
  function placeCapNow() {
    const caret = queryCaret();
    const cap = placeCapsFor(q.value, caret).find((c) => caret.at >= c.start && caret.at <= c.end) || null;
    return cap && { cap, caret };
  }

  /* Backspace (`back`) or Delete next to a collapsed part takes the whole part
     (`Spot::remove`), never half a term; an empty member goes with its `|`.
     Null when the key is the text's own. */
  function placeCapKey(back) {
    const now = placeCapNow();
    if (!now) return null;
    const { cap, caret } = now, v = q.value, c = caret.at;
    if (back ? c === cap.start : c === cap.end) return null;
    const t = placeMemberAt(cap, c);
    const p = t && cap.parts[capPartAt(cap, c)];
    if (p && p.raw) {
      if (back && t.value === "" && c === t.end) return placeRemove(v, t);
      if (back || c < t.end) return null;
    }
    const k = back ? capPartAt(cap, c)
      : cap.parts.findIndex((x) => x.spot.terms.includes(placeMembers(cap).find((m) => m.start >= c)));
    return k === null || k < 0 ? null : spotRemove(cap.parts[k].spot, v);
  }

  /* Letters typed at a disk's edge, or between parts, start a word after the
     capsule; at a folder's edge they edit it. */
  function placeCapType(data) {
    const now = placeCapNow();
    if (!now) return null;
    const { cap, caret } = now, v = q.value, c = caret.at;
    if (c === cap.start) return null;
    const t = placeMemberAt(cap, c);
    const p = t && cap.parts[capPartAt(cap, c)];
    if (p && (p.raw || p.spot.tile === null)) return null;
    return [v.slice(0, cap.end) + " " + data + v.slice(cap.end), cap.end + 1 + data.length];
  }

  /* An arrow over a capsule: to its next stop (`PlaceCap::step`), resting.
     False when the key leaves the capsule and moves by a letter. */
  function placeArrow(forward) {
    if (q.selectionStart !== q.selectionEnd) return false;
    const now = placeCapNow();
    const to = now && capStep(now.cap, q.value, now.caret.at, forward);
    if (to === null || to === undefined) return false;
    qEdit = null;
    q.setSelectionRange(to, to);
    lastQueryCaret = to;
    return true;
  }

  /* The pill's `!` over a capsule: a group into its exclusions, and back. */
  function placePill(a, b) {
    const v = q.value;
    const el = termEl(a);
    if (!el || !el.classList.contains("place")) return false;
    const [out] = el.classList.contains("neg") ? unnegatePlaces(v, a) : negatePlaces(v, a);
    editTerm([a, b], out.slice(a, out.length - (v.length - b)));
    return true;
  }

