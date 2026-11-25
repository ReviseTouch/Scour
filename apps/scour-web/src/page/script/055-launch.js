  /* ---------------- the launcher ----------------
   * The rows `:` opens, as `scour_ui::lens::launch` gives them: in two groups,
   * each with its capsule's picture, its shortcut, and whether the query
   * already holds one. Kept alike with the Rust by hand. */
  const LAUNCH_HEADINGS = { narrow: "Narrow", how: "How it searches" };
  /* A picture's name, and the field whose capsule draws it. */
  const LAUNCH_ART = { folder: "under", clock: "dm", shapes: "kind", ext: "ext", size: "size", approx: "fuzzy", read: "content" };

  /* The launcher field a term is of, `parent:` read as the place it is. */
  function launchFieldOf(raw) {
    const body = raw.startsWith("!") ? raw.slice(1) : raw;
    const sp = body.startsWith('"') ? null : fieldSplit(body);
    const f = sp ? LENS_FIELDS[fold(sp[0])] || null : null;
    return f === "parent" ? "under" : f;
  }

  /* The first term of `text` of this field, as [start, end], or null. */
  function heldTerm(text, field) {
    if (field === "fuzzy") return null; // one for every word a query has
    return terms(text).find(([a, b]) => launchFieldOf(text.slice(a, b)) === field) || null;
  }

  const launchKeeps = (field, name, want) => !want || field.startsWith(want) || name.startsWith(want);

  /* The fewest letters that leave the row at `at` alone; its name's on a tie. */
  function launchKey(at, names) {
    const unique = (p) => {
      const hits = LENS_LAUNCH.map(([f], i) => i).filter((i) => launchKeeps(LENS_LAUNCH[i][0], names[i], p));
      return hits.length === 1 && hits[0] === at;
    };
    const prefixes = (s) => [...s].map((_, i) => [...s].slice(0, i + 1).join(""));
    const name = prefixes(names[at]), field = prefixes(LENS_LAUNCH[at][0]);
    for (let n = 0; n < Math.max(name.length, field.length); n++) {
      for (const p of [name[n], field[n]]) if (p && unique(p)) return ":" + p;
    }
    return "";
  }

  /* What `:` and the letters after it offer in `text`. */
  function launchRows(text, typed) {
    const want = fold(typed.replace(/^:+/, ""));
    const names = LENS_LAUNCH.map(([, l]) => fold(T(l)));
    let group = null;
    return LENS_LAUNCH
      .map(([field, label, about, grp, icon], i) => ({ field, label, about, group: grp, icon, i }))
      .filter((r) => launchKeeps(r.field, names[r.i], want))
      .map((r) => {
        const heading = r.group !== group ? LAUNCH_HEADINGS[r.group] : "";
        group = r.group;
        return { ...r, heading, key: launchKey(r.i, names), held: !!heldTerm(text, r.field) };
      });
  }

  /* Choosing a held row: the launcher at [a, b] goes with a space beside it,
     and the caret ends the term already there. */
  function intoHeld(text, a, b, field) {
    if (text[b] === " ") b++; else if (text[a - 1] === " ") a--;
    const rest = text.slice(0, a) + text.slice(b);
    const t = heldTerm(rest, field);
    return t ? { text: rest, caret: t[1] } : null;
  }

  function launchHtml(its, sel) {
    return its.map((it, i) =>
      (it.group ? '<div class="lgroup">' + escape_(it.group) + "</div>" : "") +
      '<div class="lopt lrow' + (i === sel ? " on" : "") + (it.icon === "read" ? " read" : "") +
      '" role="option" data-i="' + i + '" aria-selected="' + (i === sel) + '">' +
      '<span class="ic">' + (FIELD_ART[LAUNCH_ART[it.icon]] || "") + "</span>" +
      '<span class="l"><span class="nm">' + escape_(it.label) + '</span><span class="ab">' + escape_(it.hint) + "</span></span>" +
      '<span class="in">' + (it.held ? escape_(T("in the query")) : "") + "</span>" +
      '<span class="k">' + escape_(it.key) + "</span></div>").join("");
  }
