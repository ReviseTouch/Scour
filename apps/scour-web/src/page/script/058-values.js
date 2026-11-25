  /* ---------------- the type and size model ----------------
   * `scour_ui::lens` `rest.rs`, `values.rs` and `size.rs` in the page's
   * words, kept alike by hand: the page test runs the Rust tests' cases on
   * these. Negation covers a whole term, so "documents but not build
   * output" is two terms, `kind:doc !kind:build`. Pure. */

  /* `lens::is_empty_term`: `empty:` or `bos:`, `!` or not. */
  function isEmptyTerm(text, t) {
    const raw = text.slice(t.start, t.end).replace(/^!+/, "");
    return /^(empty|bos):$/i.test(raw);
  }
  /* Of the panel `field`; the size panel's holds `empty:`. */
  const ofField = (text, t, field) => t.field === field || (field === "size" && isEmptyTerm(text, t));
  /* The query less every run holding a term of `field`, the runs left joined by a space. */
  const withoutField = (text, field) => placeRuns(text)
    .filter((r) => !r.some((t) => ofField(text, t, field)))
    .map((r) => text.slice(r[0].start, r[r.length - 1].end)).join(" ");
  const fieldTerms = (text, field) => placeTerms(text).filter((t) => ofField(text, t, field));
  /* `rest` and one more term: a count's query. */
  function withTerm(rest, term) {
    const r = rest.trim(), t = term.trim();
    return !r ? t : !t ? r : r + " " + t;
  }

  /* The kinds as tiles, in `KIND_VALUES` order, with the core's msgids. */
  const KIND_LABELS = [
    ["folder", "Folder"], ["code", "Code"], ["doc", "Document"], ["image", "Image"], ["data", "Data"],
    ["config", "Configuration"], ["archive", "Archive"], ["exec", "Executable"], ["audio", "Audio"],
    ["video", "Video"], ["font", "Font"], ["build", "Build output"], ["file", "File"],
  ];
  /* The other spellings of one kind the service reads (`Kind::from_name`). */
  const KIND_SPELLINGS = {
    folder: ["dir", "klasor", "klas\u00f6r"], code: ["source", "kod", "kaynak"],
    image: ["img", "pic", "gorsel", "g\u00f6rsel", "resim"], archive: ["arsiv", "ar\u015fiv", "zip"],
    doc: ["document", "belge"],
    exec: ["exe", "bin", "executable", "calistirilabilir", "\u00e7al\u0131\u015ft\u0131r\u0131labilir"],
    audio: ["ses", "muzik", "m\u00fczik"], video: ["film"], build: ["derleme", "cikti", "\u00e7\u0131kt\u0131"],
    data: ["veri"], config: ["conf", "settings", "ayar", "yapilandirma", "yap\u0131land\u0131rma"],
    font: ["yazitipi", "yaz\u0131tipi"], file: ["dosya"],
  };
  /* How many extensions the row under the kinds shows. */
  const EXT_TOP = 8;
  const kindMsgid = (token) => (KIND_LABELS.find(([k]) => k === token) || [])[1] || null;

  /* A value as the facet keys it: lower case, an extension without its dot,
     a kind by its token (`belge` is `doc`). */
  function valueKey(field, value) {
    const v = value.replace(/[\u0130I]/g, "i").toLowerCase();
    if (field === "ext") return v.replace(/^\.+/, "");
    if (field === "kind" && !KIND_VALUES.includes(v)) {
      const hit = Object.entries(KIND_SPELLINGS).find(([, all]) => all.includes(v));
      return hit ? hit[0] : v;
    }
    return v;
  }

  const listValues = (t) => t.value.split(";").filter(Boolean);

  /* `lens::picked`: what the query's terms of `field` ask for and leave out. */
  function picked(text, field) {
    const out = { on: [], off: [] };
    for (const t of fieldTerms(text, field)) {
      const side = t.not ? out.off : out.on;
      for (const v of listValues(t)) {
        const k = valueKey(field, v);
        if (!side.includes(k)) side.push(k);
      }
    }
    return out;
  }

  /* `lens::value_toggle`: one value of `field` in or out — `exclude` in a `!`
     term standing alone — and the caret after what changed. Values the panel
     did not offer stay; a term keeps its field's spelling. */
  function valueToggle(text, field, value, exclude) {
    const key = valueKey(field, value);
    const has = (t) => listValues(t).some((v) => valueKey(field, v) === key);
    const holder = (s, not) => fieldTerms(s, field).find((t) => t.not === not && has(t));
    const mine = holder(text, exclude);
    if (mine) return valueOut(text, mine, field, key);
    const other = holder(text, !exclude);
    const s = other ? valueOut(text, other, field, key)[0] : text;
    const alone = (t) => placeRuns(s).some((r) => r.length === 1 && sameTerm(r[0], t));
    const into = fieldTerms(s, field).find((t) => t.not === exclude && (!exclude || alone(t)));
    if (into) return placeReplace(s, into, valueWritten(s, into, [...listValues(into), value]));
    return appended(s, (exclude ? "!" : "") + field + ":" + value);
  }

  /* `lens::appended`: `term` at the end, one space before it whatever the
     text ends with, and the caret after it. */
  function appended(text, term) {
    const base = text.trimEnd();
    const out = base ? base + " " + term : term;
    return [out, out.length];
  }

  /* A term less the values keyed `key`; gone when none is left. */
  function valueOut(text, t, field, key) {
    const left = listValues(t).filter((v) => valueKey(field, v) !== key);
    return left.length ? placeReplace(text, t, valueWritten(text, t, left)) : placeRemove(text, t);
  }

  /* A list term rewritten with `values`, its `!` and field's spelling kept. */
  function valueWritten(text, t, values) {
    const raw = text.slice(t.start, t.end).replace(/^!+/, "");
    const at = raw.indexOf(":");
    return neg(t) + (at < 0 ? "kind" : raw.slice(0, at)) + ":" + values.join(";");
  }

  /* `lens::list_kept`: a list's choices, each leaving the other values be. */
  function listKept(t, values) {
    const field = t.field || "kind";
    const have = listValues(t);
    const key = (v) => valueKey(field, v);
    return values.map(([value, label]) => {
      const on = have.some((h) => key(h) === key(value));
      const next = have.filter((h) => key(h) !== key(value));
      if (!on) next.push(value);
      return { label, hint: on ? "✓" : "", term: neg(t) + field + ":" + next.join(";"), stays: true };
    });
  }

  /* `lens::type_ask`: the tiles count over the query less its kinds, the
     extensions over the query less its extensions; one walk when alike. */
  function typeAsk(text) {
    const kinds = withoutField(text, "kind"), exts = withoutField(text, "ext");
    return { kinds, exts, oneWalk: kinds === exts };
  }

  /* The size presets, smallest first: Empty, then `LENS_SIZES`. */
  const SIZE_PRESETS = [{ label: "Empty", term: "empty:" }, ...LENS_SIZES.map(([v, l]) => ({ label: l, term: "size:" + v }))];
  /* `lens::size_said`: what a size capsule says — its preset's msgid, "Empty"
     for `empty:` — or null. */
  function sizeSaid(text, t) {
    const i = sizePresetOf(text, t);
    return i === null ? null : SIZE_PRESETS[i].label;
  }
  /* Empty left out: every folder and every file with bytes in it. */
  const NOT_EMPTY = "!empty:";
  const sizePresetTerm = (t, i) => neg(t) + SIZE_PRESETS[i].term;
  /* Which preset a term is, `!` set aside; null for anything else. */
  function sizePresetOf(text, t) {
    if (isEmptyTerm(text, t)) return 0;
    if (t.field !== "size") return null;
    const v = t.value.toLowerCase();
    const i = SIZE_PRESETS.findIndex((p) => p.term === "size:" + v);
    return i < 0 ? null : i;
  }
  /* The query each preset is counted with: the query less its size terms, and the preset. */
  const sizeCounts = (text) => {
    const rest = withoutField(text, "size");
    return SIZE_PRESETS.map((p) => withTerm(rest, p.term));
  };

  /* ---- `lens::settle`: where the caret rests after a panel writes, so the
     same lens stays open ---- */

  /* `lens::choice_settle`: `by` in place of `t`, the caret at its end; a term
     taken out leaves an empty `field:` where it stood. */
  function choiceSettle(text, t, by, field) {
    const [out, at] = placeReplace(text, t, by);
    return by ? [out, at] : restEmpty(out, at, field);
  }

  /* `lens::value_settle`: `valueToggle`, resting on the term of the side just
     changed nearest the change, else the other side's, else an empty one. */
  function valueSettle(text, field, value, exclude) {
    const [out, at] = valueToggle(text, field, value, exclude);
    const all = fieldTerms(out, field);
    const gap = (t) => (t.start <= at && at <= t.end ? 0 : Math.min(Math.abs(t.start - at), Math.abs(t.end - at)));
    const near = (not) => all.filter((t) => t.not === not).reduce((x, y) => (!x || gap(y) < gap(x) ? y : x), null);
    const t = near(exclude) || near(!exclude);
    return t ? [out, t.end] : restEmpty(out, at, field);
  }

  /* An empty `field:` written at `at` — after the run it falls inside, so no
     `|` is broken — a space either side where text touches it. */
  function restEmpty(text, at, field) {
    let p = Math.min(at, text.length);
    for (const r of placeRuns(text)) {
      const s = r[0].start, e = r[r.length - 1].end;
      if (s < p && p < e) p = e;
    }
    const lead = p > 0 && !/\s/.test(text[p - 1]) ? " " : "";
    const tail = p < text.length && !/\s/.test(text[p]) ? " " : "";
    return [text.slice(0, p) + lead + field + ":" + tail + text.slice(p), p + lead.length + field.length + 1];
  }

