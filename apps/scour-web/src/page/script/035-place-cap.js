  /* ---------------- the place capsule ----------------
   * A run of `under:` terms drawn as one Location capsule (`lens::place_caps`):
   * a disk held whole is its name, a folder its crumbs, the part the caret is
   * in its raw letters. Every letter of the run stays in the layer — the
   * collapsed ones at no width — so each offset is still a text offset; the
   * caret of a collapsed part is drawn at its edge, and a press on a part
   * goes into its first member. */

  /* The capsules of the last paint, folded as drawn. */
  let PLACE_DRAWN = [];

  function placeCapTiles() {
    return diskTiles(PLACE_DISKS.map(diskIn), PLACE_SOURCES, () => null);
  }

  /* The capsules `text` is drawn with; `caret` null when there is none. */
  function placeCapsDrawn(text, caret) {
    PLACE_DRAWN = placeCaps(text, HOME, placeCapTiles(), caret);
    return PLACE_DRAWN;
  }

  /* Where a press on a part puts the caret: the end of its first member,
     inside a closing quote. */
  function placePressAt(text, part) {
    const t = part.spot.terms[0];
    return text[t.end - 1] === '"' && t.end - 1 > capHeadOf(text, t.start, t.end) ? t.end - 1 : t.end;
  }

  const placeZero = (s) => (s ? '<span class="z">' + escape_(s) + "</span>" : "");

  /* A collapsed part: a disk's dot and name, or a folder's crumbs, the last one bold. */
  function placePartArt(text, part, k) {
    const at = ' data-k="' + k + '" data-at="' + placePressAt(text, part) + '"';
    const title = ' title="' + escape_(part.spot.paths.map(homeForm).join("  ")) + '"';
    if (part.spot.tile !== null) {
      return '<span class="art pv"' + at + title + '><i class="pdot" style="background:var(--disk-' +
        part.spot.kind + ')"></i>' + escape_(part.spot.name) + "</span>";
    }
    const n = part.crumbs.length;
    return '<span class="art pv"' + at + title + ">" + part.crumbs.map((c, i) =>
      (i ? '<i class="cs">›</i>' : "") + (i + 1 === n ? "<b>" : '<span class="cr">') + escape_(c.label) +
      (i + 1 === n ? "</b>" : "</span>")).join("") + "</span>";
  }

  /* A raw part: its value as typed, in a well, quotes in the capsule's ink. */
  function placeRawHtml(value, k) {
    const m = /^(")?(.*?)(")?$/s.exec(value);
    const inner = (m[1] ? '<span class="r-quote">"</span>' : "") + (m[2] ? '<span class="r-value">' + escape_(m[2]) + "</span>" : "") +
      (m[3] ? '<span class="r-quote">"</span>' : "");
    return '<span class="pw" data-k="' + k + '" data-hint="' + escape_(capHint("under:")) + '">' + inner + "</span>";
  }

  /* The whole capsule: the head over the first member's field name, then each
     member in the order of the text, a shown part drawn at its first one. */
  function placeCapHtml(text, cap, caret) {
    const head = capHeadOf(text, cap.start, cap.end);
    const edit = caret > cap.start && caret <= cap.end;
    const partOf = new Map();
    cap.parts.forEach((p, k) => p.spot.terms.forEach((t) => partOf.set(t.start, k)));
    const members = cap.parts.flatMap((p) => p.spot.terms).sort((x, y) => x.start - y.start);
    let html = '<span class="term cap place' + (edit ? " edit" : "") + '" data-a="' + cap.start + '" data-b="' + cap.end +
      '" data-h="' + head + '"><span class="cap-head"><span class="art head-art">' + FOLDER_ART +
      '<span class="nm">' + escape_(fieldLabel("under")) + "</span></span>" + placeZero(text.slice(cap.start, head)) +
      '</span><span class="pbody">';
    let at = head, first = true;
    for (const t of members) {
      const k = partOf.get(t.start), p = cap.parts[k];
      const lead = p.spot.terms[0] === t;
      const own = t.start === cap.start ? head : capHeadOf(text, t.start, t.end);
      html += placeZero(text.slice(at, t.start));
      if (lead && p.shown && !first) html += '<span class="art psep" data-at="' + t.start + '">·</span>';
      if (lead && p.shown) first = false;
      html += placeZero(text.slice(Math.max(at, t.start), own));
      const value = text.slice(own, t.end);
      html += p.raw && p.shown ? placeRawHtml(value, k) : placeZero(value) + (lead && p.shown ? placePartArt(text, p, k) : "");
      at = t.end;
    }
    const hid = cap.parts.findIndex((p) => !p.shown);
    if (hid >= 0) {
      html += '<span class="art pv more" data-at="' + placePressAt(text, cap.parts[hid]) + '">+' + cap.hidden + "</span>";
    }
    return html + "</span></span>";
  }

  /* After the layer is drawn: a capsule wider than its line shows fewer
     parts, the rest behind "+N" — measured, then folded, at most once a part. */
  function placeFit(text, caret) {
    for (const cap of PLACE_DRAWN) {
      for (let guard = cap.parts.length; guard > 0; guard--) {
        const el = shadow.querySelector('.term.place[data-a="' + cap.start + '"]');
        const body = el && el.querySelector(".pbody");
        const over = body ? body.scrollWidth - body.clientWidth : 0;
        const shown = cap.parts.map((p, k) => [p, k]).filter(([p]) => p.shown);
        if (over < 1 || shown.length <= 1) break;
        let free = 0, max = shown.length;
        const need = over + (cap.hidden ? 0 : 30);
        for (const [p, k] of [...shown].reverse()) {
          if (free >= need || max <= 1) break;
          if (p.caret) continue;
          const part = body.querySelector('[data-k="' + k + '"]');
          free += (part ? part.offsetWidth : 0) + 12;
          max--;
        }
        capFold(cap, Math.min(max, shown.length - 1));
        el.outerHTML = placeCapHtml(text, cap, caret);
      }
    }
  }

  /* The capsule an offset is inside, its first letter left out. */
  const placeCapOf = (off) => PLACE_DRAWN.find((c) => off > c.start && off <= c.end) || null;
  /* The member an offset is in, if any: after its first letter, up to its end. */
  const placeMemberAt = (cap, off) => cap.parts.flatMap((p) => p.spot.terms).find((t) => t.start < off && off <= t.end) || null;

  function placeRect(el) {
    const r = el && el.offsetParent !== null ? el.getBoundingClientRect() : null;
    return r && r.width ? r : null;
  }

  /* Where the caret is drawn for an offset of a capsule: in a raw part among
     its letters; at a collapsed part's trailing edge; on the "+N" for a hidden one. */
  function placeCaret(off) {
    const cap = placeCapOf(off);
    if (!cap) return null;
    const el = shadow.querySelector('.term.place[data-a="' + cap.start + '"]');
    if (!el) return null;
    const k = capPartAt(cap, off), p = cap.parts[k];
    const well = p.shown && p.raw && el.querySelector('.pw[data-k="' + k + '"]');
    const t = placeMemberAt(cap, off);
    if (well && t) {
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
    const r = placeRect(p.shown ? el.querySelector('[data-k="' + k + '"]') : el.querySelector(".pv.more"));
    return r ? { x: r.right + 1, y: r.top + r.height / 2 } : null;
  }

  /* The boxes a selection from `lo` to `hi` lights inside the capsules: each
     collapsed part one of whose members it touches. */
  function placeSelRects(lo, hi) {
    const out = [];
    for (const cap of PLACE_DRAWN) {
      const el = shadow.querySelector('.term.place[data-a="' + cap.start + '"]');
      if (!el) continue;
      cap.parts.forEach((p, k) => {
        if (!p.shown || p.raw || !p.spot.terms.some((t) => lo < t.end && hi > t.start)) return;
        const r = placeRect(el.querySelector('[data-k="' + k + '"]'));
        if (r) out.push(r);
      });
      if (cap.hidden && cap.parts.some((p) => !p.shown && p.spot.terms.some((t) => lo < t.end && hi > t.start))) {
        const r = placeRect(el.querySelector(".pv.more"));
        if (r) out.push(r);
      }
    }
    return out;
  }

  /* A press that landed among a collapsed part's hidden letters goes where a
     press on the part goes. */
  function placeSnap(off) {
    const cap = placeCapOf(off);
    const t = cap && placeMemberAt(cap, off);
    if (!t) return off;
    const p = cap.parts[capPartAt(cap, off)];
    return p.raw ? off : placePressAt(q.value, p);
  }

  /* The capsule for an edit at caret `c` of `v`, read afresh. */
  function placeCapFor(v, c) {
    return placeCaps(v, HOME, placeCapTiles(), c).find((cap) => c >= cap.start && c <= cap.end) || null;
  }

  /* A part taken out whole: as `Spot::remove` in the place group, term by
     term in a capsule that is not the group. */
  function placeDropPart(v, cap, p) {
    const group = placeRuns(v).find((r) => r.some(isPlace));
    if (group && group[0].start === cap.start) return spotRemove(p.spot, v, HOME);
    let out = [v, p.spot.terms[0].start];
    for (const t of [...p.spot.terms].sort((x, y) => y.start - x.start)) out = placeRemove(out[0], t);
    return out;
  }

  /* Backspace (`back`) or Delete at a collapsed part's edge takes the part
     away, never half a term; an empty member goes with its `|`. Null when
     the key is the text's own. */
  function placeCapKey(back) {
    const v = q.value, c = q.selectionStart;
    const cap = placeCapFor(v, c);
    if (!cap || (back ? c === cap.start : c === cap.end)) return null;
    const members = cap.parts.flatMap((p) => p.spot.terms).sort((x, y) => x.start - y.start);
    const t = members.find((m) => m.start < c && c <= m.end);
    if (back && t && t.value === "" && c === t.end) return placeRemove(v, t);
    if (back && t) return null;
    if (!back && t && c < t.end) return null;
    const k = back ? capPartAt(cap, c) : cap.parts.findIndex((p) => p.spot.terms.includes(members.find((m) => m.start >= c)));
    return k === null || k < 0 ? null : placeDropPart(v, cap, cap.parts[k]);
  }

  /* Letters typed at a collapsed part's edge start a word after the capsule. */
  function placeCapType(data) {
    const v = q.value, c = q.selectionStart;
    const cap = placeCapFor(v, c);
    if (!cap || c === cap.start || cap.parts.some((p) => p.spot.terms.some((t) => t.start < c && c <= t.end))) return null;
    return [v.slice(0, cap.end) + " " + data + v.slice(cap.end), cap.end + 1 + data.length];
  }

  /* The pill's `!` on a capsule: every member its own exclusion — "under a or
     b" excluded is "not under a and not under b". */
  function placeNegated(v, a, b) {
    const ts = placeTerms(v).filter((t) => t.start >= a && t.end <= b);
    return ts.map((t) => "!" + v.slice(t.start, t.end)).join(" ");
  }

