  /* ---------------- the type panel ----------------
   * `kind:` and `ext:` in one panel: a tile a kind with its count and share,
   * and under them the extensions within the kinds chosen — the query's own
   * when none is. A press toggles one value; Alt leaves it out. Counted
   * exactly (`exact=1`): over the query less its kinds for the tiles, less
   * its extensions for the row; one walk when those are the same. */

  function typePanelFill() {
    const a = typeAsk(q.value);
    const facets = (query, by) => SERVICE.get("facets", { q: query, by, top: EXT_TOP, exact: 1 });
    if (a.oneWalk) {
      if (!READS.test(a.kinds) && !typeHeard(a).kinds) {
        panelAsk("panel type " + a.kinds, () => facets(a.kinds, "kind,ext").then((r) => ({
          kinds: r.groups[0], exts: r.groups[1], total: r.total,
        })));
      }
      return;
    }
    if (!READS.test(a.kinds) && !typeHeard(a).kinds) {
      panelAsk("panel kinds " + a.kinds, () => facets(a.kinds, "kind").then((r) => ({ kinds: r.groups[0], total: r.total })));
    }
    if (!READS.test(a.exts) && !typeHeard(a).exts) {
      panelAsk("panel exts " + a.exts, () => facets(a.exts, "ext").then((r) => ({ exts: r.groups[0], total: r.total })));
    }
  }

  /* What is in hand for the tiles and the row: a walk of either kind serves. */
  function typeHeard(a) {
    const one = (query) => panelHeard("panel type " + query);
    const kinds = one(a.kinds) || panelHeard("panel kinds " + a.kinds);
    const exts = one(a.exts) || panelHeard("panel exts " + a.exts);
    return { kinds: kinds && { pairs: facetPairs(kinds.kinds), total: kinds.total }, exts: exts && facetPairs(exts.exts) };
  }

  /* The extensions the row offers: the counted ones, then any the query
     holds that were not among them. */
  function typeExts(heard, pe) {
    const out = (heard || []).filter(([k]) => k).map(([k, n]) => ({ value: k, n }));
    for (const k of [...pe.on, ...pe.off]) if (!out.some((c) => c.value === k)) out.push({ value: k, n: undefined });
    return out;
  }

  function typePanelHtml() {
    const a = typeAsk(q.value);
    const heard = typeHeard(a);
    const pk = picked(q.value, "kind"), pe = picked(q.value, "ext");
    const counts = new Map(heard.kinds ? heard.kinds.pairs : []);
    const total = heard.kinds ? heard.kinds.total : 0;
    const reads = READS.test(a.kinds);
    const kkb = panelZone("kinds", KIND_LABELS.length, 1);
    const tiles = KIND_LABELS.map(([tok, msgid], i) => {
      const n = heard.kinds ? counts.get(tok) || 0 : null;
      const share = n && total ? Math.max(2, Math.round(100 * n / total)) : 0;
      const cls = "kd" + (pk.on.includes(tok) ? " on" : pk.off.includes(tok) ? " no" : "") + (n === 0 ? " zero" : "") + (kkb(i) ? " kb" : "");
      return '<button type="button" tabindex="-1" class="' + cls + '" data-i="' + i + '" title="kind:' + tok + '">' +
        '<span class="ico k-' + tok + '"></span><b>' + escape_(KIND_KEY[tok] || T(msgid)) + "</b>" +
        "<small>" + (reads ? "" : escape_(panelNum(n))) + '</small><span class="sh"><i style="width:' + share + '%"></i></span></button>';
    }).join("");
    const exts = typeExts(heard.exts, pe);
    PANEL.exts = exts.map((c) => c.value);
    const ekb = panelZone("exts", exts.length);
    const chips = exts.map((c) => ({ label: "." + c.value, n: reads ? undefined : c.n, on: pe.on.includes(c.value), no: pe.off.includes(c.value) }));
    const within = pk.on.map((k) => KIND_KEY[k] || T(kindMsgid(k) || k)).join(", ");
    return '<div class="pp pp-type">' +
      '<div class="lhead"><b>' + escape_(LENS.kind === "ext" ? T("Extension") : T("Type")) + "</b><span>" + escape_(reads ? T("reads files") : "") + "</span></div>" +
      '<div class="kinds" data-zone="kinds">' + tiles + "</div>" +
      '<div class="lgroup">' + escape_(T("Extension") + (within ? " · " + within : "")) + "</div>" +
      (heard.exts || exts.length ? panelChipsHtml(chips, "exts", ekb, "exts") : '<div class="pp-chips exts" data-zone="exts"><span class="pc wait">…</span></div>') +
      '<div class="lfoot pp-keys"><span>' + escape_(T("Space or Enter adds or takes one away; Alt+Enter leaves it out")) +
      "</span><span>" + escape_(T("Alt+click: leave it out")) + "</span></div></div>";
  }

  /* A tile or a chip pressed: its value toggled, and the lens kept on a term
     of its own field near where it hung, so the panel does not move. */
  function typePress(zone, i, exclude) {
    const field = zone === "kinds" ? "kind" : "ext";
    const value = zone === "kinds" ? (KIND_LABELS[i] || [])[0] : (PANEL.exts || [])[i];
    if (!value) return;
    const own = LENS.kind === "ext" ? "ext" : "kind";
    const near = LENS.a;
    const [text] = valueToggle(typeUntyped(field, value), field, value, exclude);
    const [out, caret] = typeAnchor(text, own, near);
    panelWrite(out, caret);
  }

  /* The query less the letters being typed at the end of the lens's term
     when they start the value pressed: `kind:do` and Document is `kind:doc`. */
  function typeUntyped(field, value) {
    const t = panelTerm(), v = q.value;
    if (t.field !== field || q.selectionStart !== t.end) return v;
    const vals = listValues(t);
    const last = vals[vals.length - 1];
    const known = field === "kind" ? KIND_VALUES.includes(valueKey("kind", last || "")) : last === value;
    if (!last || known || !value.startsWith(valueKey(field, last))) return v;
    const by = vals.length > 1 ? valueWritten(v, t, vals.slice(0, -1)) : neg(t) + v.slice(t.start, t.end).replace(/^!?([^:]*:).*$/s, "$1");
    return v.slice(0, t.start) + by + v.slice(t.end);
  }

  /* Where the lens rests after a press: the term of `field` nearest `near`;
     with none left, an empty one where it stood. */
  function typeAnchor(text, field, near) {
    const own = fieldTerms(text, field);
    if (own.length) {
      const best = own.reduce((x, y) => (Math.abs(y.start - near) < Math.abs(x.start - near) ? y : x));
      return [text, best.end];
    }
    const p = Math.min(near, text.length);
    const lead = p > 0 && !/\s/.test(text[p - 1]) ? " " : "", tail = p < text.length && !/\s/.test(text[p]) ? " " : "";
    return [text.slice(0, p) + lead + field + ":" + tail + text.slice(p), p + lead.length + field.length + 1];
  }

