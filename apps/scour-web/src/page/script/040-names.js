  /* Whether a row has a picture arrives with it from one `stat`. Never
   * `loading="lazy"`: a browser will not start one for a page it thinks
   * nobody is looking at. */
  const iconUrl = (f) =>
    `/api/icon?t=${TOKEN}&p=${encodeURIComponent(f.path + "/" + f.name)}`;

  const SYSICON_MISS = new Set();

  function sysIconKey(f) {
    if (f.is_dir || f.ktoken === "folder") {
      const full = (f.path + "/" + f.name).replace(/\/+$/, "");
      if (HOME && full === HOME) return "place:home";
      if (HOME && f.path === HOME && f.name) return "home:" + f.name;
      return "folder";
    }
    const lower = (f.name || "").toLowerCase();
    for (const c of ["tar.gz", "tar.xz", "tar.bz2", "tar.zst"]) {
      if (lower.endsWith("." + c) && lower.length > c.length + 1) return "ext:" + c;
    }
    const dot = lower.lastIndexOf(".");
    if (dot > 0) {
      const ext = lower.slice(dot + 1);
      if (ext.length <= 32 && /^[a-z0-9._+-]+$/.test(ext)) return "ext:" + ext;
    }
    return "kind:" + (GLYPHS.has(f.ktoken) ? f.ktoken : "file");
  }

  const sysIconUrl = (key) => key.startsWith("home:")
    ? `/api/sysicon?t=${TOKEN}&home_dir=${encodeURIComponent(key.slice(5))}`
    : `/api/sysicon?t=${TOKEN}&k=${encodeURIComponent(key)}`;

  /* The type tokens something is drawn for; anything else falls back to the
     plain page rather than to no picture. */
  const GLYPHS = new Set([
    "folder", "file", "doc", "code", "image", "video", "media",
    "audio", "archive", "exec", "data", "config", "font", "build",
  ]);

  /* The freshness rule and the type glyph belong to the row, not a column.
   * Off removes the element: hidden nodes are still laid out on every scroll. */
  function writeMark(td, f) {
    /* The rule is first and stays first: `writeRow` reaches for it as
       `firstElementChild`, and a ghost row keeps it. */
    let rule = td.firstElementChild;
    if (!rule || rule.tagName !== "I") {
      rule = document.createElement("i");
      td.prepend(rule);
    }
    /* Only the picture: a tile holds its name in this cell too. Asked by
       class, since the glyph is a `<span>` and a thumbnail an `<img>`. */
    let el = rule.nextElementSibling;
    if (el && !el.classList.contains("ico")) el = null;
    if (!f || !ICONS) {
      if (el) el.remove();
      return rule;
    }
    const showThumb = !!(f.thumb && grid());
    const sysKey = !showThumb && KEPT && KEPT.system_icons ? sysIconKey(f) : null;
    const useSys = !!(sysKey && !SYSICON_MISS.has(sysKey));
    const want = showThumb || useSys ? "IMG" : "SPAN";
    if (el && el.tagName !== want) {
      el.remove();
      el = null;
    }
    if (!el) {
      el = want === "IMG" ? new Image() : document.createElement("span");
      el.className = "ico";
      if (want === "IMG") el.alt = "";
      rule.after(el);
    }
    if (showThumb) {
      el.onerror = () => {
        f.thumb = false;
        writeMark(td, f);
      };
      const url = iconUrl(f);
      if (el.getAttribute("src") !== url) {
        el.style.visibility = "";
        el.setAttribute("src", url);
      }
    } else if (useSys) {
      el.onerror = () => {
        SYSICON_MISS.add(sysKey);
        writeMark(td, f);
      };
      const url = sysIconUrl(sysKey);
      if (el.getAttribute("src") !== url) {
        el.style.visibility = "";
        el.setAttribute("src", url);
      }
    } else {
      const cls = "ico k-" + (GLYPHS.has(f.ktoken) ? f.ktoken : "file");
      if (el.className !== cls) el.className = cls;
    }
    return rule;
  }

  /* The name, appended as nodes rather than through `innerHTML`: a parse per
   * row measured 15 ms a paint over two hundred rows. */
  function writeName(td, f, terms, parsed) {
    // Rewritten whole: the marked run moves with every keystroke.
    td.replaceChildren();
    // A drive's root has no name of its own: `C:/` is called by its path.
    const name = f.name || f.path.replace(/\/+$/, "");
    // Every place each word is, not the first only.
    const all = spans(name, terms);
    if (all.length) {
      let pos = 0;
      for (const [from, len] of all) {
        if (from > pos) td.append(name.slice(pos, from));
        const m = document.createElement("mark");
        m.textContent = name.slice(from, from + len);
        td.append(m);
        pos = from + len;
      }
      if (pos < name.length) td.append(name.slice(pos));
      return;
    }
    let at = null;
    if (parsed && parsed.fuzzy && parsed.fuzzy.length) {
      at = fuzzySpan(name, parsed.fuzzy);
    }
    if (!at && KEPT && KEPT.typos === "always" && terms.length) {
      at = fuzzySpan(name, terms);
    }
    if (at) {
      const mark = document.createElement("mark");
      mark.textContent = name.slice(at[0], at[0] + at[1]);
      td.append(name.slice(0, at[0]), mark, name.slice(at[0] + at[1]));
      return;
    }
    if (parsed && parsed.letters && parsed.letters.length) {
      const runs = subseqSpans(name, parsed.letters[0]);
      if (runs) {
        let pos = 0;
        for (const [s, e] of runs) {
          if (s > pos) td.append(name.slice(pos, s));
          const m = document.createElement("mark");
          m.textContent = name.slice(s, e);
          td.append(m);
          pos = e;
        }
        if (pos < name.length) td.append(name.slice(pos));
        return;
      }
    }
    td.append(name);
  }

  function subseqSpans(name, needle) {
    const [text, at] = bare(name);
    const want = bare(needle)[0];
    if (!want) return null;
    let wi = 0;
    const out = [];
    for (let i = 0; i < text.length; i++) {
      if (text[i] === want[wi]) {
        const s = at ? at[i] : i;
        const e = at ? at[i + 1] : i + 1;
        const prev = out[out.length - 1];
        if (prev && prev[1] === s) prev[1] = e;
        else if (!prev || s >= prev[1]) out.push([s, e]);
        wi++;
        if (wi === want.length) return out;
      }
    }
    return null;
  }

  function oneEdit(a, b) {
    if (a === b) return true;
    if (Math.abs(a.length - b.length) > 1) return false;
    let i = 0;
    while (i < a.length && i < b.length && a[i] === b[i]) i++;
    if (a.length === b.length) {
      if (a.slice(i + 1) === b.slice(i + 1)) return true;
      return i + 1 < a.length && a[i] === b[i + 1] && a[i + 1] === b[i] && a.slice(i + 2) === b.slice(i + 2);
    }
    return a.length > b.length ? a.slice(i + 1) === b.slice(i) : a.slice(i) === b.slice(i + 1);
  }

  function fuzzySpan(name, terms) {
    const [text, at] = bare(name);
    for (const raw of terms) {
      const want = bare(raw)[0];
      if (!want || want.length < 3) continue;
      let fallback = null;
      for (const len of [want.length - 1, want.length, want.length + 1]) {
        if (len < 2 || len > text.length) continue;
        for (let i = 0; i + len <= text.length; i++) {
          const sub = text.slice(i, i + len);
          if (oneEdit(sub, want)) {
            const from = at ? at[i] : i;
            const to = at ? at[i + len] : i + len;
            const res = [from, to - from];
            if (!/[._\-\s]$/.test(sub)) return res;
            fallback = fallback || res;
          }
        }
      }
      if (fallback) return fallback;
    }
    return null;
  }

  /* Text as a name is searched — case and marks set aside — and where each
     character of it stood. Plain ASCII, nearly every name, is only lowered. */
  function bare(s) {
    if (/^[\x00-\x7f]*$/.test(s)) return [s.toLowerCase(), null];
    let out = "";
    const at = [];
    for (let i = 0; i < s.length; i++) {
      const c = s[i].normalize("NFD").replace(/[\u0300-\u036f]/g, "").toLowerCase().replace("\u0131", "i");
      for (let k = 0; k < c.length; k++) at.push(i);
      out += c;
    }
    at.push(s.length);
    return [out, at];
  }

  /* Every place each term is, as `Unaccented::marks` finds them in Rust:
     in order, overlapping and touching ones joined, as [from, length]. */
  function spans(name, terms) {
    if (!terms.length) return [];
    const [text, at] = bare(name);
    const found = [];
    for (const t of terms) {
      const want = bare(t)[0];
      if (!want) continue;
      for (let i = text.indexOf(want); i >= 0; i = text.indexOf(want, i + 1)) {
        const from = at ? at[i] : i;
        const to = at ? at[i + want.length] : i + want.length;
        if (to > from) found.push([from, to]);
      }
    }
    found.sort((a, b) => a[0] - b[0] || a[1] - b[1]);
    const out = [];
    for (const [a, b] of found) {
      const last = out[out.length - 1];
      if (last && a <= last[1]) last[1] = Math.max(last[1], b);
      else out.push([a, b]);
    }
    return out.map(([a, b]) => [a, b - a]);
  }

  /* Completions come from the field table, so one the parser would read as
     plain text cannot be offered. */
  function completions(text, cursor) {
    let a = cursor, b = cursor;
    while (a > 0 && !/\s/.test(text[a - 1])) a--;
    while (b < text.length && !/\s/.test(text[b])) b++;
    const neg = text[a] === "!" ? "!" : "";
    const typed = text.slice(a, cursor).replace(/^!/, "");
    const sp = fieldSplit(typed);
    if (sp) {
      const f = field(sp[0]);
      if (!f) return [];
      const list = f.takes === "kind" ? KIND_VALUES : f.takes === "time" ? TIME_VALUES : [];
      const semi = f.takes === "kind" ? sp[1].lastIndexOf(";") : -1;
      const head = semi >= 0 ? sp[1].slice(0, semi + 1) : "";
      const pre = fold(semi >= 0 ? sp[1].slice(semi + 1) : sp[1]);
      return list.filter((v) => v.startsWith(pre)).map((v) => ({
        insert: neg + sp[0] + ":" + head + v, label: f.name + ":" + v, about: T(f.about), at: [a, b],
      }));
    }
    const pre = fold(typed);
    return FIELDS
      .filter((f) => f.name.startsWith(pre) || f.alias.some((x) => x.startsWith(pre)))
      .map((f) => ({ insert: f.name + ":", label: f.eg || f.name + ":", about: T(f.about), at: [a, b] }));
  }

  const qread   = document.getElementById("qread");
  let hints = [], hintSel = 0;
  let focusPath = null, wantFocusPath = null;

  function showHints() {
    // Nothing while the box is unfocused (`render()` also runs on load), and
    // nothing while the word under the caret is empty.
    if (document.activeElement !== q) { hideHints(); return; }
    const cur = q.selectionStart ?? q.value.length;
    let a = cur;
    while (a > 0 && !/\s/.test(q.value[a - 1])) a--;
    if (cur === a) { hideHints(); return; }
    hints = completions(q.value, cur);
    if (hints.length === 0) { hideHints(); return; }
    hintSel = Math.min(hintSel, hints.length - 1);
    hintBox.innerHTML = hints.map((h, i) =>
      '<button role="option" data-i="' + i + '" aria-selected="' + (i === hintSel) + '">' +
      "<code>" + escape_(h.label) + "</code><span>" + escape_(h.about) + "</span></button>").join("");
    hintBox.classList.remove("past");
    hintBox.hidden = false;
    q.setAttribute("aria-expanded", "true");
  }
  function hideHints() {
    hintBox.hidden = true;
    hintBox.classList.remove("past");
    hints = [];
    hintSel = 0;
    q.setAttribute("aria-expanded", "false");
    const arrow = document.getElementById("qpast");
    if (arrow) arrow.setAttribute("aria-expanded", "false");
  }

  /* The past, in the completions' own box: a remembered query is a completion
   * whose insert is the whole line, so every key already works. */
  function showHistory() {
    const typed = q.value.trim().toLowerCase();
    // What was typed narrows it; an empty box offers everything, newest first.
    const past = HISTORY.filter((h) => !typed || h.toLowerCase().includes(typed));
    if (!past.length) { hideHints(); return; }
    hints = past.map((h) => ({ insert: h, label: h, about: "", at: [0, q.value.length] }));
    hintSel = 0;
    hintBox.innerHTML = hints.map((h, i) =>
      '<button role="option" data-i="' + i + '" aria-selected="' + (i === hintSel) + '">' +
      "<code>" + escape_(h.label) + "</code><span></span></button>").join("");
    hintBox.classList.add("past");
    hintBox.hidden = false;
    q.setAttribute("aria-expanded", "true");
  }

  /* The box searches per keystroke, so the history holds what was committed
   * to — Enter, or opening a result — rather than `r`, `ra`, `rap`. */
  const qpast = document.getElementById("qpast");
  /* Shown only when there is a past. */
  function showPastAffordance() {
    qpast.hidden = HISTORY.length === 0;
  }
  qpast.addEventListener("mousedown", (e) => {
    // `mousedown`: by `click` the input has lost focus and the list is closed.
    e.preventDefault();
    if (hintBox.hidden) { q.focus(); showHistory(); } else { hideHints(); }
    qpast.setAttribute("aria-expanded", String(!hintBox.hidden));
  });

  function remember(text) {
    text = (text || "").trim();
    if (!text || HISTORY[0] === text) return;
    HISTORY = [text, ...HISTORY.filter((h) => h !== text)].slice(0, 100);
    showPastAffordance();
    /* The query, not the list: the service holds the history and applies the
       cap, so no frontend has to know the number. */
    keep({ remember: text });
  }
  function acceptHint(i) {
    const h = hints[i];
    if (!h) return;
    const before = q.value.slice(0, h.at[0]);
    const after = q.value.slice(h.at[1]);
    /* A space, unless there is one: otherwise a completion taken against the
       end of another word welds to it, as `raporkind:exec`. */
    const lead = before && !/\s$/.test(before) ? " " : "";
    const tail = after && !/^\s/.test(after) ? " " : "";
    q.value = before + lead + h.insert + tail + after;
    const caret = before.length + lead.length + h.insert.length + tail.length;
    q.setSelectionRange(caret, caret);
    hideHints();
    render();
    q.focus();
  }

  hintBox.addEventListener("mousedown", (e) => {
    // `mousedown`, not `click`: by `click` the input has lost focus and the
    // list has closed itself, so the click never lands.
    const b = e.target.closest("button");
    if (!b) return;
    e.preventDefault();
    acceptHint(+b.dataset.i);
  });

  q.addEventListener("keydown", (e) => {
    /* `Alt+↓` opens what was searched before; plain `↓` walks into the
       results. */
    if (e.altKey && e.key === "ArrowDown") {
      e.preventDefault();
      if (hintBox.hidden) showHistory(); else hideHints();
      return;
    }
    /* Enter commits the query to the history and keeps the text. With the
       list open it takes the selection instead — handled below. */
    if (e.key === "Enter" && hintBox.hidden) {
      // A text area would break the line; a query is one line however it wraps.
      e.preventDefault();
      if (!LIST.total && SIMILAR && !similarBox.hidden) {
        q.value = SIMILAR;
        render();
        return;
      }
      remember(q.value);
      return;
    }
    /* Down goes to the list, but only when no completion is showing: there
       the arrows belong to the completions until Escape closes them. */
    if (hintBox.hidden && e.key === "ArrowDown") {
      const rows = [...body.querySelectorAll("tr[data-path]")];
      const first = (focusPath && rows.find((tr) => tr.dataset.path === focusPath)) || rows[0];
      if (first) {
        e.preventDefault();
        first.focus();
        return;
      }
    }
    if (hintBox.hidden) return;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      hintSel = (hintSel + (e.key === "ArrowDown" ? 1 : hints.length - 1)) % hints.length;
      [...hintBox.children].forEach((b, i) => b.setAttribute("aria-selected", i === hintSel));
      return;
    }
    if (e.key === "Enter" || e.key === "Tab") { e.preventDefault(); acceptHint(hintSel); return; }
    if (e.key === "Escape") { e.preventDefault(); hideHints(); }
  });
  q.addEventListener("blur", hideHints);

