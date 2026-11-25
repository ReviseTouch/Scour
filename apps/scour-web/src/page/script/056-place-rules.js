  /* ---------------- the place rules ----------------
   * `scour_ui::lens::folders` in the page's words: what the folder list's
   * Add and Exclude write, where the caret rests after a press, and what the
   * list is weighed by. The page test runs the Rust tests' cases on these. */

  /* `lens::term_at`: the term the caret is in or just after. */
  function placeTermAt(text, caret) {
    return placeTerms(text).find((t) => caret > t.start && caret <= t.end) || null;
  }

  /* `add` as a term of its own at the end, and the caret after it. */
  function placeAtEnd(text, add) {
    const out = text + (!text || /\s$/.test(text) ? "" : " ") + add;
    return [out, out.length];
  }

  /* The standalone `!under:` of `want` taken out, the caret kept on its term. */
  function placeUnexclude([text, caret], home, want) {
    const h = placeScope(text, home).outside.find((x) => x.path === want);
    if (!h) return [text, caret];
    const [out, cut] = placeRemove(text, h.term);
    const gone = text.length - out.length;
    return [out, cut < caret ? caret - gone : caret];
  }

  /* `lens::place_add_at`: "Add". A member the folder lies in or holds, an
     empty `under:` and the member being edited at `caret` give way to it in
     place; else it joins. Never takes a folder out. */
  function placeAddAt(text, home, path, caret) {
    const typing = editedTerm(text, caret);
    return placeAddWith(text, home, path, typing && isPlace(typing) ? typing : null);
  }

  function placeAddWith(text, home, path, typed) {
    const want = placeNorm(home, path);
    const add = "under:" + quotedPath(homeIn(home, want));
    const run = placeRuns(text).find((r) => r.some(isPlace));
    if (!run) return placeUnexclude(placeAtEnd(text, add), home, want);
    const givesWay = (t) => {
      const held = placeNorm(home, t.value);
      return t.value === "" || (typed && sameTerm(typed, t)) || liesUnder(want, held) || liesUnder(held, want);
    };
    const places = run.filter(isPlace);
    const give = places.filter(givesWay);
    if (!give.length) {
      const end = places[places.length - 1].end;
      return placeUnexclude([text.slice(0, end) + "|" + add + text.slice(end), end + 1 + add.length], home, want);
    }
    // Later ones first, so the first one's offsets stay true.
    let out = text;
    for (const t of give.slice(1).reverse()) out = placeRemove(out, t)[0];
    return placeUnexclude(placeReplace(out, give[0], add), home, want);
  }

  /* `lens::place_exclude`: "Exclude" writes the folder's `!under:` at the
     end, or takes it away; a member naming the folder leaves the group. */
  function placeExclude(text, home, path) {
    const want = placeNorm(home, path);
    const s = placeScope(text, home);
    const h = s.outside.find((x) => x.path === want);
    if (h) return placeRemove(text, h.term);
    const rest = scopeHolds(s, want) ? placeToggle(text, home, want, Pick.Also)[0] : text;
    return placeAtEnd(rest, "!under:" + quotedPath(homeIn(home, want)));
  }

  /* `lens::place_settle`: where the caret rests after a press of the place
     lens wrote `text`. `not`: on the `!under:` nearest `at`. Else at the end
     of the group's last `under:`; with none, an empty `under:`, found or
     written at `at`. */
  function placeSettle(text, at, not) {
    const all = placeTerms(text);
    const excluded = all.filter((t) => t.not && t.field === "under");
    if (not && excluded.length) {
      const near = excluded.reduce((b, t) => (Math.abs(t.start - at) < Math.abs(b.start - at) ? t : b));
      return [text, near.end];
    }
    const run = placeRuns(text).find((r) => r.some(isPlace));
    if (run) return [text, run.filter(isPlace).pop().end];
    const empty = all.find((t) => isPlace(t) && t.value === "");
    if (empty) return [text, empty.end];
    let p = Math.min(at, text.length);
    const inside = all.find((t) => t.start < p && p < t.end);
    if (inside) p = inside.end;
    const pad = (c) => (c && !/\s/.test(c) ? " " : "");
    const lead = pad(text[p - 1]), tail = pad(text[p]);
    return [text.slice(0, p) + lead + "under:" + tail + text.slice(p), p + lead.length + 6];
  }

  /* `lens::place_at`: the folder a place value's list shows, in full, and
     what is typed after its last `/`. Nothing yet shows home, or `/`. */
  function placeAt(value, home) {
    const [base, part] = placeParts(value);
    return [base ? fullIn(home, base) : (home || "/"), part];
  }

  /* `lens::without_places`: the query less every run holding a place not
     written with `!` — what the folder list and the tiles are weighed by. */
  function withoutPlaces(text) {
    const place = (t) => !t.not && (t.field === "under" || t.field === "parent");
    return placeRuns(text).filter((r) => !r.some(place))
      .map((r) => text.slice(r[0].start, r[r.length - 1].end)).join(" ");
  }

