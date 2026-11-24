  /* The rows the last search returned; nothing else keeps a copy. */
  let FILES = [];

  /* Every kind label comes from `/api/kinds`, the rail's own source. */
  const labelOfToken = (token) => KIND_KEY[token] || T("Other");
  const kindLabel = (row) => labelOfToken(row.is_dir ? "folder" : row.kind);

  /* Spreads everything sent and lays the two renamed fields over it, or a
     column switched on later arrives blank. */
  const fromService = (row) => ({
    ...row,
    path: row.dir,
    /* The engine's own word beside the translated one: the icon asks by token. */
    ktoken: row.is_dir ? "folder" : row.kind,
    kind: kindLabel(row),
  });


  let KINDS = [];
  /* The scope shortcuts and `~`, from the desktop's own record of places. No
     counts: `/api/facets` answers for the query and has no equivalent here. */
  let SCOPES = [];
  /* Mounts where `st_atime` is written once at creation. See `noatime_mounts`. */
  let MOUNTS = [];
  let HOME = "";
  const SIZES = [[">10 MB", ">10mb", null], [">1 MB", ">1mb", null], ["= 0", "=0", null]];

  const BANDS = [
    [0,        3600,       "--t0"],
    [3600,     DAY,        "--t0"],
    [DAY,      7 * DAY,    "--t1"],
    [7 * DAY,  30 * DAY,   "--t2"],
    [30 * DAY, 180 * DAY,  "--t3"],
    [180 * DAY, 365 * DAY, "--t4"],
    [365 * DAY, Infinity,  "--t5"]
  ];
  const bandOf = (mtime) => {
    const ago = now - mtime;
    for (const b of BANDS) if (ago < b[1]) return b[2];
    return "--t5";
  };

  const fmt = (n) => NUMBERS.format(n);
  const bytes = (n) => {
    const u = ["B", "KB", "MB", "GB", "TB"];
    let i = 0, v = n;
    while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
    return (i === 0 ? v : v.toFixed(v < 10 ? 1 : 0)) + " " + u[i];
  };
  /* Seconds, but only for the first minute: above that a screenful of counting
   * rows would be a second loud voice. The fine range needs `tickClock`. */
  const ago = (mtime) => {
    const s = now - mtime;
    /* One second is the floor, so clock skew reads as a second. Six rungs
       rather than a unit substituted into one string, and the singular is a
       second msgid standing in for plural forms. */
    if (s < 60) return T("{n} sec ago", { n: Math.max(1, s) });
    if (s < 3600) return T("{n} min ago", { n: Math.max(1, Math.round(s / 60)) });
    if (s < DAY) return T("{n} hr ago", { n: Math.round(s / 3600) });
    if (s < 30 * DAY) {
      const n = Math.round(s / DAY);
      return n === 1 ? T("1 day ago") : T("{n} days ago", { n });
    }
    if (s < 365 * DAY) {
      const n = Math.round(s / (30 * DAY));
      return n === 1 ? T("1 month ago") : T("{n} months ago", { n });
    }
    const years = Math.round(s / (365 * DAY));
    return years === 1 ? T("1 year ago") : T("{n} years ago", { n: years });
  };

  /* The exact time behind "4 days ago". `STAMP` is built once: this runs
     once per row per paint, and a formatter per call is not free. */
  const exact = (at) => (!at ? "" : STAMP.format(new Date(at * 1000)));

  /* The deepest mount point containing it is the one it is on. Out here
   * because the clock reads it too: a `—` must not gain an age on a tick. */
  function frozenAtime(f) {
    let owner = null;
    for (const m of MOUNTS) {
      const under = m.at === "/" || (f.path + "/").startsWith(m.at + "/");
      if (under && (!owner || m.at.length > owner.at.length)) owner = m;
    }
    return !!owner && !owner.reads;
  }

  const q      = document.getElementById("q");
  const shadow = document.getElementById("shadow");
  const body   = document.getElementById("rows-body");
  const empty  = document.getElementById("empty");
  const bars   = document.getElementById("bars");

  /* The protocol's word for the order, which every face reads. The column
     stays `mtime`: that is a column, not an order. */
  let sortKey = "modified", sortDesc = true;

  // The page's own reading of the query, copied from `scour-query`.
  function parse(text) {
    const terms = text.trim().split(/\s+/).filter(Boolean);
    const out = { text: [], not: [], letters: [], fuzzy: [], kind: null, under: null, parent: null,
                  size: null, band: null, ext: null, path: null, isDir: null };
    for (const raw of terms) {
      const neg = raw.startsWith("!");
      const t = neg ? raw.slice(1) : raw;
      if (!t) continue;
      const sp = fieldSplit(t);
      const f = sp && field(sp[0]);
      // A field that cannot be read is plain text — the parser's own rule.
      if (!f || (f.takes !== "none" && !accepts(f, sp[1]))) {
        (neg ? out.not : out.text).push(fold(t).replace(/"/g, ""));
        continue;
      }
      const v = sp[1];
      switch (f.name) {
        case "kind":    out.kind    = fold(v); break;
        case "under":   out.under   = v;       break;
        case "parent":  out.parent  = v.replace(/\/$/, ""); break;
        case "size":    out.size    = fold(v); break;
        case "ext":     out.ext     = fold(v); break;
        case "path":    out.path    = fold(v); break;
        case "dm": case "dc": case "da": out.band = fold(v); break;
        case "file":    out.isDir   = false;   break;
        case "folder":  out.isDir   = true;    break;
        case "content": out.text.push("\u0000"); break;
        case "letters": if (!neg && v) out.letters.push(fold(v).replace(/"/g, "")); break;
        case "fuzzy":   if (!neg && v) out.fuzzy.push(fold(v).replace(/"/g, "")); break;
      }
    }
    return out;
  }

  /* Kind token to label, from `/api/kinds`. Empty until it lands. */
  let KIND_KEY = {};

  const escape_ = (s) => s.replace(/[&<>"']/g, (c) => (
    { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));

  /* A copy of `scour-query`'s `fields.rs` for the completions, the help and
     the rails; the colours come over the wire, so drift shows in the rails.
     `name` and `alias` are grammar; only `about` is a msgid. */
  const FIELDS = [
    { name: "ext",     alias: [],                          takes: "ext",   about: "the extension is this, or one of these separated by ;", eg: "ext:pdf" },
    { name: "path",    alias: [],                          takes: "text",  about: "the whole path contains this",                   eg: "path:src/api" },
    { name: "under",   alias: ["in", "altinda"],           takes: "path",  about: "under this folder, at any depth",                eg: "under:/home" },
    { name: "parent",  alias: ["child", "children"],       takes: "path",  about: "directly inside this folder, one level",         eg: "parent:/etc" },
    { name: "file",    alias: ["files"],                   takes: "none",  about: "files only",                                     eg: "" },
    { name: "folder",  alias: ["folders", "dir"],          takes: "none",  about: "folders only",                                   eg: "" },
    { name: "size",    alias: [],                          takes: "size",  about: "size, in binary units, comparison optional",      eg: "size:>1mb" },
    { name: "kind",    alias: ["type", "tur", "tür"],      takes: "kind",  about: "the rough kind of file",                         eg: "kind:image" },
    { name: "dm",      alias: ["modified"],                takes: "time",  about: "when it last changed",                           eg: "dm:7d" },
    { name: "dc",      alias: ["created"],                 takes: "time",  about: "when it was created",                            eg: "dc:2026-01-31" },
    { name: "da",      alias: ["accessed"],                takes: "time",  about: "when it was last read",                          eg: "da:>2026-01-01" },
    { name: "content", alias: ["text", "icerik", "içerik"],takes: "text",  about: "the file's contents hold this text; the files are read to find out", eg: "content:invoice" },
    { name: "fuzzy",   alias: ["typo", "benzer"],          takes: "text",  about: "the name contains this, give or take a typing mistake", eg: "fuzzy:raporr" },
    { name: "letters", alias: ["harfler"],                 takes: "text",  about: "the name has these letters in this order, with anything between", eg: "letters:scrgui" },
  ];
  /* `Kind::OFFERED`, which is what `/api/kinds` sends. `parse` and `without`
   * both go through `accepts`, so a missing kind leaves the whole rail at
   * zero; `the_page_takes_the_engines_kind_vocabulary` fails on drift. */
  const KIND_VALUES = ["folder", "code", "doc", "image", "data", "config",
                       "archive", "exec", "audio", "video", "font", "build", "file"];
  /* Every other spelling `Kind::from_name` takes: accepted, not offered. */
  const KIND_ALIASES = [
    "dir", "klasor", "klasör",
    "source", "kod", "kaynak",
    "img", "pic", "gorsel", "görsel", "resim",
    "arsiv", "arşiv", "zip",
    "document", "belge",
    "exe", "bin", "executable", "calistirilabilir", "çalıştırılabilir",
    "ses", "muzik", "müzik",
    "film",
    "media", "medya",
    "derleme", "cikti", "çıktı",
    "veri",
    "conf", "settings", "ayar", "yapilandirma", "yapılandırma",
    "yazitipi", "yazıtipi",
    "dosya",
    "text", "metin",
  ];
  const TIME_VALUES = ["today", "yesterday", "7d", "30d", "1y"];

  /* The engine's `DefaultFolder` rule: dotted and dotless i are one letter,
     which is why the table lists `altinda`. The `"tr"` is not the interface's
     language and must not follow it. The catalogue folds nothing. */
  const fold = (t) => t.replace(/[İIı]/g, "i").toLocaleLowerCase("tr");
  const field = (name) => {
    const f = fold(name);
    return FIELDS.find((x) => x.name === f || x.alias.includes(f)) || null;
  };

  const SIZE_RE = /^(?:[<>]=?|=)?\s*\d+(?:[.,]\d+)?\s*(?:b|kb|mb|gb|tb)?$/;
  const TIME_RE = /^(?:[<>]=?|=)?\s*(?:\d+(?:h|s|saat|d|g|gun|gün|w|hafta|m|ay|y|yil|yıl)|today|yesterday|week|month|year|bugun|bugün|dun|dün|hafta|ay|yil|yıl|\d{4}-\d{2}-\d{2})$/;

  function accepts(f, value) {
    const v = fold(value);
    switch (f.takes) {
      case "none": return true;
      case "text": case "path": return v.length > 0;
      case "ext":  return v.split(";").some((e) => e.replace(/^\./, "").length > 0);
      case "size": return SIZE_RE.test(v);
      case "time": return TIME_RE.test(v);
      case "kind": return v.split(";").some(Boolean) && v.split(";").every((k) => !k || KIND_VALUES.includes(k) || KIND_ALIASES.includes(k));
    }
    return false;
  }

  const isListField = (f) => !!(f && (f.takes === "ext" || f.takes === "kind"));

  function isListPiece(piece) {
    if (!piece) return true;
    if (piece.startsWith('"')) return false;
    const head = piece.split("|")[0] || "";
    return head.length > 0 && !/^[!"(<]/.test(head) && !/[:/*?!]/.test(head);
  }

  function canJoinList(tok) {
    const trimmed = tok.replace(/^;+/, "");
    if (!trimmed) return true;
    return isListPiece(trimmed.split(";")[0]);
  }

  function cutSemicolons(s) {
    const out = [];
    let cur = s;
    for (;;) {
      if (isListField(fieldOf(cur))) {
        let quoted = false, from = 0, skipFirst = true, splitAt = -1;
        for (let i = 0; i < cur.length; i++) {
          const c = cur[i];
          if (c === '"') quoted = !quoted;
          else if (c === "|" && !quoted) break;
          else if (c === ";" && !quoted) {
            if (!skipFirst && !isListPiece(cur.slice(from, i))) { splitAt = from - 1; break; }
            skipFirst = false;
            from = i + 1;
          }
        }
        if (splitAt < 0 && !skipFirst && !isListPiece(cur.slice(from))) splitAt = from - 1;
        if (splitAt >= 0) { out.push(cur.slice(0, splitAt)); cur = cur.slice(splitAt + 1); continue; }
        out.push(cur);
        break;
      }
      let quoted = false, cut = -1;
      for (let i = 0; i < cur.length; i++) {
        if (cur[i] === '"') quoted = !quoted;
        else if (cur[i] === ";" && !quoted) { cut = i; break; }
      }
      if (cut >= 0) { out.push(cur.slice(0, cut)); cur = cur.slice(cut + 1); }
      else { out.push(cur); break; }
    }
    return out;
  }

