  /* ---------------- the terms ----------------
   * `scour_ui::lens::terms` (`lens/split.rs`) in the page's words: where one
   * term ends and the next begins as the service reads it. Words split on
   * whitespace and `|` outside quotes; an `ext:` or `kind:` list goes on
   * across a `;` with spaces round it. Offsets are the text's own. */

  /* `lens::term`: one term by its offsets, its field folded to the lens's name. */
  function placeTerm(text, start, end) {
    const raw = text.slice(start, end);
    const not = raw.startsWith("!");
    const bare = not ? raw.slice(1) : raw;
    const sp = bare.startsWith('"') ? null : fieldSplit(bare);
    const name = sp ? fold(sp[0]) : "";
    const field = sp && Object.hasOwn(LENS_FIELDS, name) ? LENS_FIELDS[name] : null;
    let value = (sp ? sp[1] : bare).replace(/^"+|"+$/g, "");
    if (field === "ext" || field === "kind") value = value.replace(/\s+/g, "");
    return { start, end, not, field, value };
  }

  /* Every term with its offsets. */
  function placeTerms(text) {
    const out = [];
    for (const [s, e] of splitJoined(text, splitWords(text))) {
      const raw = text.slice(s, e);
      if (/^;+$/.test(raw)) continue;
      const pieces = splitIsList(splitCondensed(raw)) ? splitCut(text, s, e) : [[s, e]];
      for (const [a, b] of pieces) splitAlternatives(text, a, b, out);
    }
    return out;
  }

  /* Ranges of the words: whitespace outside quotes cuts. */
  function splitWords(text) {
    const out = [];
    let start = null, quoted = false;
    for (let i = 0; i < text.length; i++) {
      const c = text[i];
      const cut = !quoted && /\s/.test(c);
      if (c === '"') quoted = !quoted;
      if (cut) {
        if (start !== null) out.push([start, i]);
        start = null;
      } else if (start === null) start = i;
    }
    if (start !== null) out.push([start, text.length]);
    return out;
  }

  /* The words with an open list joined to what continues it. */
  function splitJoined(text, words) {
    const out = [];
    let open = false;
    for (const [s, e] of words) {
      const t = text.slice(s, e);
      const last = out[out.length - 1];
      const afterList = !!last && splitOpenable(splitCondensed(text.slice(last[0], last[1])));
      if (last && afterList && splitCanJoin(t) && (open || t.startsWith(";"))) last[1] = e;
      else out.push([s, e]);
      const [a, b] = out[out.length - 1];
      const now = splitCondensed(text.slice(a, b));
      open = now.endsWith(";") && splitOpenable(now);
    }
    return out;
  }

  /* A list range cut where a `;` starts a term of its own; each piece trimmed. */
  function splitCut(text, s, e) {
    const out = [];
    let from = s;
    for (;;) {
      const cur = text.slice(from, e);
      const at = splitIsList(splitCondensed(cur)) ? splitList(cur) : splitUnquoted(cur, ";");
      if (at === null) {
        out.push(splitTrimmed(text, from, e));
        return out;
      }
      out.push(splitTrimmed(text, from, from + at));
      from += at + 1;
    }
  }

  /* The terms of one piece: cut on `|` outside quotes. */
  function splitAlternatives(text, s, e, out) {
    let from = s, quoted = false;
    const push = (a, b) => {
      const [x, y] = splitTrimmed(text, a, b);
      if (x < y) out.push(placeTerm(text, x, y));
    };
    for (let i = s; i < e; i++) {
      if (text[i] === '"') quoted = !quoted;
      else if (text[i] === "|" && !quoted) {
        push(from, i);
        from = i + 1;
      }
    }
    push(from, e);
  }

  function splitTrimmed(text, s, e) {
    const raw = text.slice(s, e);
    const a = s + raw.length - raw.trimStart().length;
    return [a, a + raw.trim().length];
  }

  /* The text without the whitespace outside quotes. */
  function splitCondensed(raw) {
    let quoted = false, out = "";
    for (const c of raw) {
      if (c === '"') quoted = !quoted;
      if (quoted || !/\s/.test(c)) out += c;
    }
    return out;
  }

  function splitUnquoted(s, mark) {
    let quoted = false;
    for (let i = 0; i < s.length; i++) {
      if (s[i] === '"') quoted = !quoted;
      if (s[i] === mark && !quoted) return i;
    }
    return null;
  }

  /* Whether a word opens an `ext:` or `kind:` list, one `!` set aside. */
  function splitIsList(word) {
    if (word.startsWith('"')) return false;
    const sp = fieldSplit(word.startsWith("!") ? word.slice(1) : word);
    const f = sp ? LENS_FIELDS[fold(sp[0])] : null;
    return f === "ext" || f === "kind";
  }

  /* A list no `|` has closed and no stranger has ended: more can follow. */
  const splitOpenable = (w) => splitIsList(w) && splitUnquoted(w, "|") === null && splitList(w) === null;

  /* One `;`-piece that can stay a value of the list. */
  function splitPiece(piece) {
    const p = piece.trim();
    if (!p) return true;
    if (p.startsWith('"')) return false;
    const head = p.split("|")[0];
    return head !== "" && !/^[!"(<]/.test(head) && !/[:/*?!]/.test(head);
  }

  /* Whether a word continues an open list across a space. */
  function splitCanJoin(word) {
    const rest = word.replace(/^;+/, "");
    if (!rest) return true;
    const at = splitUnquoted(rest, ";");
    return splitPiece(at === null ? rest : rest.slice(0, at));
  }

  /* Where in a list a `;` starts a new term: before the first piece after the
     first that cannot be a value. Not past a `|`. */
  function splitList(raw) {
    let quoted = false, from = 0, first = true;
    for (let i = 0; i < raw.length; i++) {
      const c = raw[i];
      if (c === '"') quoted = !quoted;
      else if (c === "|" && !quoted) break;
      else if (c === ";" && !quoted) {
        if (!first && !splitPiece(raw.slice(from, i))) return from - 1;
        first = false;
        from = i + 1;
      }
    }
    return !first && !splitPiece(raw.slice(from)) ? from - 1 : null;
  }

