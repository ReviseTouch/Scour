  /* ---------------- the content pattern ----------------
   * `scour_ui::pattern` in the page's words: the pieces a content pattern is
   * taken apart into, what the pill rewrites, and what `:` offers. Columns
   * are characters, as the model's; each case in tests/page.test.cjs is one of
   * the Rust tests, with the same inputs and answers. Kept alike by hand. */

  /* The msgid heading a capsule of each kind (`PieceKind::label`). */
  const PIECE_HEAD = { word: "", phrase: "exact phrase", or: "either", not: "without", similar: "similar" };

  /* A `~` that makes a term forgiving: at a term's start, after `!`, `;`,
     `|`, `(` or a space, and followed by a letter of the word or nothing yet. */
  function patternTildeAt(chars, i, start) {
    const before = i === start || "!;|(".includes(chars[i - 1]) || /\s/u.test(chars[i - 1]);
    const next = chars[i + 1];
    return before && (next === undefined || (!/\s/u.test(next) && !'";|)~'.includes(next)));
  }

  /* The alternatives of the piece at [start, end), each without its
     operators, and a hole (start === end) where the last one, or the only
     one, is not written yet: after `!`, `~`, a trailing `;`, between quotes. */
  function patternAlts(chars, start, end) {
    const out = [];
    const close = (a, b, last, seps) => {
      let similar = false;
      while (a < b && (/\s/u.test(chars[a]) || "(!~".includes(chars[a]))) {
        similar = similar || (chars[a] === "~" && patternTildeAt(chars, a, start));
        a++;
      }
      while (b > a && (/\s/u.test(chars[b - 1]) || chars[b - 1] === ")")) b--;
      // A phrase closed, or one still being written: no quote after it.
      const quoted = a < b && chars[a] === '"';
      const closed = quoted && b > a + 1 && chars[b - 1] === '"';
      const open = quoted && !closed && !chars.slice(a + 1, b).includes('"');
      if (closed) { a++; b--; } else if (open) a++;
      if (a < b || (last && (seps > 0 || out.length === 0))) out.push({ start: a, end: Math.max(a, b), phrase: closed || open, similar });
    };
    let quoted = false, seps = 0, from = start;
    for (let i = start; i < end; i++) {
      if (chars[i] === '"') quoted = !quoted;
      else if ((chars[i] === ";" || chars[i] === "|") && !quoted) { close(from, i, false, seps); from = i + 1; seps++; }
    }
    close(from, end, true, seps);
    return out;
  }

  /* The pieces of a text pattern as the search takes it apart: each with its
     columns, kind, operators' columns and alternatives. */
  function patternPieces(input) {
    const chars = Array.from(input);
    const tokens = [];
    let start = null, quoted = false, depth = 0;
    chars.forEach((c, i) => {
      if (c === '"') { quoted = !quoted; if (start === null) start = i; }
      else if (c === "(" && !quoted && start === null) { depth++; start = i; }
      else if (c === ")" && !quoted && depth > 0) depth--;
      else if (/\s/u.test(c) && !quoted && depth === 0) { if (start !== null) { tokens.push([start, i]); start = null; } }
      else if (start === null) start = i;
    });
    if (start !== null) tokens.push([start, chars.length]);
    // Glued as the search glues them: a lone `;`/`|` joins its neighbours, a
    // lone `!` the next, a leading or trailing one the neighbour on its side.
    const groups = [];
    let glue = false, bang = null;
    for (const [a, b] of tokens) {
      const t = chars.slice(a, b).join("");
      const q = t.startsWith('"');
      if (!q && (t === "|" || t === ";")) {
        glue = true;
        // Nothing to be the other of yet: an empty "either".
        if (groups.length) groups[groups.length - 1][1] = b; else groups.push([a, b]);
        continue;
      }
      if (!q && t === "!") { bang = a; continue; }
      const lead = !q && (t.startsWith("|") || t.startsWith(";"));
      const trail = !q && t.length > 1 && (t.endsWith("|") || t.endsWith(";"));
      const from = bang !== null ? bang : a;
      bang = null;
      if (groups.length && (glue || lead)) groups[groups.length - 1][1] = b;
      else groups.push([from, b]);
      glue = trail;
    }
    // A `!` with no word after it yet: an empty "without".
    if (bang !== null && (!groups.length || groups[groups.length - 1][1] <= bang)) groups.push([bang, bang + 1]);
    return groups.map(([a, b]) => {
      const ops = [];
      let q = false, or = false, not = false, phrase = false, similar = false;
      for (let i = a; i < b; i++) {
        const c = chars[i];
        if (c === '"') { q = !q; phrase = true; ops.push(i); }
        else if ((c === ";" || c === "|") && !q) { or = true; ops.push(i); }
        else if ((c === "(" || c === ")") && !q) ops.push(i);
        else if (c === "!" && !q && (i === a || /\s/u.test(chars[i - 1]))) { not = true; ops.push(i); }
        else if (c === "~" && !q && patternTildeAt(chars, i, a)) { similar = true; ops.push(i); }
      }
      const kind = not ? "not" : or ? "or" : similar ? "similar" : phrase ? "phrase" : "word";
      return { start: a, end: b, kind, ops, alts: patternAlts(chars, a, b) };
    });
  }

  /* The piece the caret stands in or at the edge of, or null. */
  function patternPieceAt(pieces, caret) {
    const k = pieces.findIndex((p) => p.start <= caret && caret <= p.end);
    return k < 0 ? null : k;
  }

  /* Written but holding no word yet: every alternative a hole. */
  function patternEmpty(p) {
    return p.alts.every((a) => a.start === a.end);
  }

  /* The hole the caret is in, as [piece, alternative], or null. */
  function patternHoleAt(pieces, caret) {
    for (let k = 0; k < pieces.length; k++) {
      const j = pieces[k].alts.findIndex((a) => a.start === a.end && a.start === caret);
      if (j >= 0) return [k, j];
    }
    return null;
  }

  /* ---- what the pill does (`pattern::edit`) ---- */

  function patternCut(input, from, to, put) {
    const c = Array.from(input);
    return c.slice(0, from).join("") + put + c.slice(to).join("");
  }

  /* The pattern without the piece at `at`, and one space less. */
  function patternRemove(input, at) {
    const piece = patternPieces(input)[at];
    if (!piece) return input;
    const c = Array.from(input);
    let to = piece.end;
    while (to < c.length && /\s/u.test(c[to])) to++;
    let from = piece.start;
    if (to === c.length) while (from > 0 && /\s/u.test(c[from - 1])) from--;
    return patternCut(input, from, to, "");
  }

  /* The piece at `at` excluded, or no longer: a `!` before it, or its `!` gone. */
  function patternToggleNot(input, at) {
    const piece = patternPieces(input)[at];
    if (!piece) return input;
    if (piece.kind !== "not") return patternCut(input, piece.start, piece.start, "!");
    const c = Array.from(input);
    const bang = piece.ops.find((i) => c[i] === "!") ?? piece.start;
    let to = bang + 1;
    while (to < piece.end && /\s/u.test(c[to])) to++;
    return patternCut(input, bang, to, "");
  }

  /* The alternative `alt` of the piece at `at` forgiving, or exact again; a
     phrase never is. */
  function patternToggleSimilar(input, at, alt) {
    const p = patternPieces(input)[at];
    const a = p && p.alts[alt];
    if (!a || a.phrase) return input;
    return a.similar ? patternCut(input, a.start - 1, a.start, "") : patternCut(input, a.start, a.start, "~");
  }

  /* ---- the keys that open and empty a capsule (`pattern::edit`) ---- */

  /* Backspace in a hole takes the operator that opened it: an empty capsule
     whole, an empty last alternative its `;`; just inside a phrase's opening
     quote, both quotes go and its words stay. [text, caret], or null: an
     ordinary backspace. */
  function patternBackspace(input, caret) {
    const c = Array.from(input);
    const p = patternPieces(input);
    const hole = patternHoleAt(p, caret);
    if (hole) {
      const piece = p[hole[0]];
      if (patternEmpty(piece)) return [patternCut(input, piece.start, piece.end, ""), piece.start];
      let sep = caret - 1;
      while (sep >= piece.start && c[sep] !== ";" && c[sep] !== "|") sep--;
      return sep < piece.start ? null : [patternCut(input, sep, caret, ""), sep];
    }
    const a = p.flatMap((x) => x.alts).find((x) => x.phrase && x.start === caret && caret > 0 && c[caret - 1] === '"');
    if (!a) return null;
    const close = c[a.end] === '"' ? 1 : 0;
    return [patternCut(input, caret - 1, a.end + close, c.slice(a.start, a.end).join("")), caret - 1];
  }

  /* A space stays in its capsule: after a letter of a word the word becomes a
     phrase holding it (`osmanlı` → `"osmanlı "`); in a hole it is not written;
     between quotes, before a word or between capsules it is typed. */
  function patternSpace(input, caret) {
    const p = patternPieces(input);
    const hole = patternHoleAt(p, caret);
    if (hole) return p[hole[0]].alts[hole[1]].phrase ? null : [input, caret];
    const a = p.flatMap((x) => x.alts).find((x) => !x.phrase && x.start < caret && caret <= x.end);
    if (!a) return null;
    const c = Array.from(input);
    const part = (from, to) => c.slice(from, to).join("");
    return [part(0, a.start) + '"' + part(a.start, caret) + " " + part(caret, a.end) + '"' + part(a.end, c.length), caret + 2];
  }

  /* Tab leaves the capsule for a new one after it: an empty capsule goes, a
     phrase loses the spaces at its ends and its quotes when one plain word is
     left, and the caret lands past a space after it. Null: not in a capsule. */
  function patternLeave(input, caret) {
    const p = patternPieces(input);
    const k = patternPieceAt(p, caret);
    if (k === null) return null;
    const piece = p[k];
    if (patternEmpty(piece)) return [patternCut(input, piece.start, piece.end, ""), piece.start];
    const c = Array.from(input);
    let end = piece.end;
    for (const a of piece.alts.filter((x) => x.phrase && x.start !== x.end).reverse()) {
      const closed = c[a.end] === '"' ? 1 : 0;
      const words = c.slice(a.start, a.end).join("").trim();
      const plain = words !== "" && !/[\s";|!~()\\]/u.test(words);
      const put = Array.from(plain || !words ? words : '"' + words + '"');
      const from = a.start - 1, to = a.end + closed;
      end += put.length - (to - from);
      c.splice(from, to - from, ...put);
    }
    const spaced = end < c.length && /\s/u.test(c[end]) && (end + 1 === c.length || /\s/u.test(c[end + 1]));
    if (!spaced) c.splice(end, 0, " ");
    return [c.join(""), end + 1];
  }

  /* A quote at a word's start writes both, the caret between; at a phrase's
     closing quote it steps over it. */
  function patternQuote(input, caret) {
    const c = Array.from(input);
    const inside = c.slice(0, caret).filter((x) => x === '"').length % 2 === 1;
    if (inside) return c[caret] === '"' ? [input, caret + 1] : null;
    const start = caret === 0 || /\s/u.test(c[caret - 1]) || "!;|(~".includes(c[caret - 1]);
    const end = c[caret] === undefined || /\s/u.test(c[caret]) || c[caret] === ")";
    return start && end ? [patternCut(input, caret, caret, '""'), caret + 1] : null;
  }

  /* ---- what `:` offers (`pattern::launch`) ---- */

  /* `write` null: the case flips and the field loses the `:`. */
  const CONTENT_ITEMS = [
    { token: "phrase", label: "Exact phrase", about: "these words, in this order", sign: "\"…\"", icon: "quote", write: '""', into: 1 },
    { token: "or", label: "Either", about: "this word or the next", sign: "|", icon: "or", write: "|", into: 1 },
    { token: "not", label: "Without", about: "files that do not hold the word", sign: "!", icon: "not", write: "!", into: 1 },
    { token: "similar", label: "Similar spelling", about: "one letter off still matches", sign: "~", icon: "approx", write: "~", into: 1 },
    { token: "case", label: "Match case", about: "capitals as typed", sign: "Aa", icon: "case", write: null, into: 0 },
  ];

  /* Case aside, dotted and dotless i one (U+0130, I, U+0131). */
  const contentFold = (s) => s.replace(/[\u0130I\u0131]/g, "i").toLowerCase();

  /* The `:` the caret asks with, as [column, letters typed after it]; null
     inside quotes or with anything but letters between it and the caret. */
  function contentAsking(input, caret) {
    const chars = Array.from(input);
    const end = Math.min(caret, chars.length);
    let at = end;
    while (at > 0 && /\p{Alphabetic}/u.test(chars[at - 1])) at--;
    if (at === 0 || chars[at - 1] !== ":") return null;
    const colon = at - 1;
    if (chars.slice(0, colon).filter((c) => c === '"').length % 2 === 1) return null;
    return [colon, chars.slice(at, end).join("")];
  }

  /* The items `typed` keeps, by token or by the name a person reads. */
  function contentItems(typed, name) {
    const want = contentFold(typed);
    return CONTENT_ITEMS.filter((i) => !want || i.token.startsWith(want) || contentFold(name(i.label)).startsWith(want));
  }

  /* `input` with `item` chosen for the `:` at `colon` and the letters up to
     `caret`: [text, caret column]. Either glues to the word before it. */
  function contentChoose(input, colon, caret, item) {
    const chars = Array.from(input);
    const end = Math.max(Math.min(caret, chars.length), colon + 1);
    let head = chars.slice(0, colon).join("");
    const tail = chars.slice(end).join("");
    if (item.token === "or") head = head.replace(/\s+$/u, "");
    return [head + (item.write ?? "") + tail, Array.from(head).length + item.into];
  }

