  /* ---------------- the place capsule ----------------
   * A run of `under:` terms drawn as one Location capsule, and a block of
   * `!under:` ones as one red capsule (`lens::place_caps_at`,
   * `lens::exclusion_caps`): a disk held whole is its name, a folder its
   * crumbs, a folder being edited its raw letters. Every letter stays in the
   * layer — the collapsed ones at no width — so each offset is still a text
   * offset; a collapsed part's caret is drawn at its edge. */

  /* The capsules of the last paint, folded as drawn; `not` on the red ones. */
  let PLACE_DRAWN = [];

  function placeCapTiles() {
    return diskTiles(PLACE_DISKS.map(diskIn), PLACE_SOURCES, () => null);
  }

  /* Every capsule of `text` for `caret` (a `Caret`, or null), in text order. */
  function placeCapsFor(text, caret) {
    const tiles = placeCapTiles();
    const pos = placeCapsAt(text, HOME, tiles, caret).map((c) => ({ ...c, not: false }));
    const neg = exclusionCaps(text, HOME, tiles, caret).map((c) => ({ ...c, not: true }));
    return [...pos, ...neg].sort((x, y) => x.start - y.start);
  }

  /* The capsules `text` is drawn with. */
  function placeCapsDrawn(text, caret) {
    PLACE_DRAWN = placeCapsFor(text, caret);
    return PLACE_DRAWN;
  }

  /* Where a press on a part puts the caret: a disk's trailing edge, resting;
     the end of a folder, inside a closing quote. */
  function placePressAt(text, part) {
    if (part.spot.tile !== null) return capPartEnd(part);
    const t = part.spot.terms[0];
    return text[t.end - 1] === '"' && t.end - 1 > capHeadOf(text, t.start, t.end) ? t.end - 1 : t.end;
  }

  const placeZero = (s) => (s ? '<span class="z">' + escape_(s) + "</span>" : "");

  /* A collapsed part: a disk's dot and name, or a folder's crumbs, the last one bold. */
  function placePartArt(text, part, k) {
    const folder = part.spot.tile === null;
    const at = ' data-k="' + k + '" data-at="' + placePressAt(text, part) + '"' + (folder ? ' data-edit="1"' : "");
    const title = ' title="' + escape_(part.spot.paths.map(homeForm).join("  ")) + '"';
    if (!folder) {
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
     member in the order of the text — a shown part drawn at its first one, a
     block of hidden parts as one "+N" where the block stands. */
  function placeCapHtml(text, cap, caret) {
    const head = capHeadOf(text, cap.start, cap.end);
    const edit = caret > cap.start && caret <= cap.end;
    const partOf = new Map();
    cap.parts.forEach((p, k) => p.spot.terms.forEach((t) => partOf.set(t.start, k)));
    const blocks = new Map(capBlocks(cap).map(([first, n]) => [first, n]));
    const members = cap.parts.flatMap((p) => p.spot.terms).sort((x, y) => x.start - y.start);
    let html = '<span class="term cap place' + (cap.not ? " neg" : "") + (edit ? " edit" : "") + '" data-a="' + cap.start +
      '" data-b="' + cap.end + '" data-h="' + head + '"><span class="cap-head"><span class="art head-art">' +
      (cap.not ? '<span class="bang">!</span>' : "") + FOLDER_ART + '<span class="nm">' + escape_(fieldLabel("under")) +
      "</span></span>" + placeZero(text.slice(cap.start, head)) + '</span><span class="pbody">';
    let at = head, first = true;
    const sep = (to) => (first ? "" : '<span class="art psep" data-at="' + to + '">·</span>');
    for (const t of members) {
      const k = partOf.get(t.start), p = cap.parts[k];
      const lead = p.spot.terms[0] === t;
      const own = t.start === cap.start ? head : capHeadOf(text, t.start, t.end);
      html += placeZero(text.slice(at, t.start));
      if (lead && p.shown) { html += sep(t.start); first = false; }
      if (lead && blocks.has(k)) {
        html += sep(t.start) + '<span class="art pv more" data-blk="' + k + '" data-at="' + placePressAt(text, p) + '">+' +
          blocks.get(k) + "</span>";
        first = false;
      }
      html += placeZero(text.slice(Math.max(at, t.start), own));
      const value = text.slice(own, t.end);
      html += p.raw && p.shown ? placeRawHtml(value, k) : placeZero(value) + (lead && p.shown ? placePartArt(text, p, k) : "");
      at = t.end;
    }
    return html + "</span></span>";
  }

  /* After the layer is drawn: a capsule wider than its line shows fewer
     parts, the rest behind "+N" — measured, then folded, at most once a part. */
  function placeFit(text, caret) {
    for (const cap of PLACE_DRAWN) {
      for (let guard = cap.parts.length; guard > 0; guard--) {
        const el = placeCapEl(cap);
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

  const placeCapEl = (cap) => shadow.querySelector('.term.place[data-a="' + cap.start + '"]');

