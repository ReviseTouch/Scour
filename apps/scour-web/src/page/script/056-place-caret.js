  /* ---------------- the caret over a place capsule ----------------
   * `scour_ui::lens::caret` and `lens::negate` in the page's words: whether
   * the person is editing the term the caret is in or it only rests there,
   * where the arrows stop over a capsule, and a group turned round into its
   * exclusions and back. Pure; kept alike by the page test. */

  /* `Caret`: where the caret is, and whether the person is editing there.
     Resting after a press, a click on a part or an arrow; editing while typing. */
  const caretResting = (at) => ({ at, editing: false });
  const caretEditing = (at) => ({ at, editing: true });

  /* `caret::is_edited`: a field with nothing after its `:`, or the caret
     strictly inside the term, or at its end while editing. */
  function isEdited(t, caret) {
    if (t.field !== null && t.value === "") return true;
    return !!caret && t.start < caret.at && (caret.at < t.end || (caret.at === t.end && caret.editing));
  }

  /* `lens::edited_term`: the term the person is editing at `caret`, if any. */
  function editedTerm(text, caret) {
    const t = placeTermAt(text, caret.at);
    return t && isEdited(t, caret) ? t : null;
  }

  /* `lens::lens_of`, as far as `opens_itself` needs it: which lens a term opens. */
  function placeLensOf(text, t) {
    const raw = text.slice(t.start, t.end);
    if (/^:[^:]*$/.test(raw)) return "launch";
    const byField = { under: "place", parent: "place", dm: "time", size: "size", empty: "size", kind: "kind", ext: "ext",
      content: "content", fuzzy: "word", letters: "word" };
    if (t.field) return byField[t.field] || null;
    if (raw.includes(":")) return null;
    return [...t.value].length >= 2 ? "word" : null;
  }

  /* A place term drawn in a capsule — a run of `under:`, or a lone `!under:` —
     that the caret only rests on: its lens does not open by itself. */
  function capsuleRests(text, t, caret) {
    const capsuled = t.field === "under" && placeRuns(text).some((r) =>
      r.some((m) => sameTerm(m, t)) && (r.every((m) => m.field === "under" && !m.not) || r.length === 1));
    return capsuled && !isEdited(t, caret);
  }

  /* `lens::opens_by_itself`: whether the lens of the term at the caret opens
     by itself — never for a caret resting on a capsule's part. */
  function opensByItself(text, caret) {
    const t = placeTermAt(text, caret.at);
    if (!t) return false;
    const lens = placeLensOf(text, t);
    return !!lens && lens !== "word" && !capsuleRests(text, t, caret);
  }

  const capPartEnd = (p) => Math.max(...p.spot.terms.map((t) => t.end));
  /* An offset that does not split a letter of two code units. */
  const charBoundary = (text, o) => !(o > 0 && o < text.length && /[\udc00-\udfff]/.test(text[o]));

  /* `PlaceCap::stops`: where the arrows stop — the start, each collapsed
     part's trailing edge, every letter of a raw part, one stop a hidden block. */
  function capStops(cap, text) {
    const out = [cap.start];
    const n = cap.parts.length;
    cap.parts.forEach((p, k) => {
      if (!p.shown) {
        if (k + 1 === n || cap.parts[k + 1].shown) out.push(capPartEnd(p));
      } else if (p.raw) {
        for (const t of p.spot.terms) {
          for (let o = t.start; o <= t.end; o++) if (charBoundary(text, o)) out.push(o);
        }
      } else out.push(capPartEnd(p));
    });
    return [...new Set(out)].sort((a, b) => a - b);
  }

  /* `PlaceCap::step`: the caret after one arrow from `at`, or null when the
     key leaves the capsule and the text moves by a letter. */
  function capStep(cap, text, at, forward) {
    if (at < cap.start || at > cap.end) return null;
    const stops = capStops(cap, text);
    const next = forward ? stops.find((s) => s > at) : [...stops].reverse().find((s) => s < at);
    return next === undefined ? null : next;
  }

  /* `PlaceCap::blocks`: each block of hidden parts as [first, how many]. */
  function capBlocks(cap) {
    const out = [];
    cap.parts.forEach((p, k) => {
      if (p.shown) return;
      const last = out[out.length - 1];
      if (last && last[0] + last[1] === k) last[1]++;
      else out.push([k, 1]);
    });
    return out;
  }

  /* `negate::exclusion_blocks`: standalone `!under:` terms side by side. */
  function exclusionBlocks(text) {
    const out = [];
    let open = false;
    for (const run of placeRuns(text)) {
      const one = run.length === 1 && run[0].not && run[0].field === "under";
      if (one && open) out[out.length - 1].push(run[0]);
      else if (one) out.push([run[0]]);
      open = one;
    }
    return out;
  }

  const placeSpans = (ts, at) => ts[0].start <= at && at <= ts[ts.length - 1].end;
  function placeRewrite(text, ts, by) {
    const a = ts[0].start, b = ts[ts.length - 1].end;
    return [text.slice(0, a) + by + text.slice(b), a + by.length];
  }

  /* `lens::negate_places`: the group at `at` as one `!under:` per member, side
     by side ("in none of them"), empty ones dropped; else left as it is. */
  function negatePlaces(text, at) {
    const run = placeRuns(text).find((r) => r.every(isPlace) && placeSpans(r, at));
    if (!run) return [text, at];
    const each = run.filter((t) => t.value !== "").map((t) => "!" + text.slice(t.start, t.end));
    return placeRewrite(text, run, each.join(" "));
  }

  /* `lens::unnegate_places`: the block of `!under:` at `at` as one `|` group. */
  function unnegatePlaces(text, at) {
    const block = exclusionBlocks(text).find((b) => placeSpans(b, at));
    if (!block) return [text, at];
    return placeRewrite(text, block, block.map((t) => text.slice(t.start + 1, t.end)).join("|"));
  }

