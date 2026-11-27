  /* Toggled with Space, as every quick-look is. What can be shown is asked of
   * the bridge, and the bytes are reached by pointing an element at the URL:
   * a blob would hold the whole file to show one frame.
   *
   * Four layers, top to bottom, as `scour_ui::preview` orders them: what it is
   * and where, what can be done, what is in it, the facts. The first two and
   * the last change the moment the row does — the row already holds them —
   * and what is in it is asked once the hand has stopped. Nothing moves. */
  const peek = document.getElementById("peek");
  const peekBody = document.getElementById("peek-body");
  const peekName = document.getElementById("peek-name");
  const peekCrumbs = document.getElementById("peek-crumbs");
  const peekActs = document.getElementById("peek-acts");
  const peekInfo = document.getElementById("peek-info");
  const peekSum = document.getElementById("peek-sum");
  const peekMore = document.getElementById("peek-more");
  const peekFacts = document.getElementById("peek-facts");
  const peekFound = document.getElementById("peek-found");
  /* A selection begun in the preview stays in it, and Ctrl+A there takes its
     text alone: the rest of the page is out of its reach until a press
     elsewhere. A drag that left the text ran up through the whole page. */
  document.addEventListener("mousedown", (e) => {
    document.body.classList.toggle("peek-selecting", peekBody.contains(e.target));
  }, true);
  document.addEventListener("keydown", (e) => {
    if (!(e.ctrlKey || e.metaKey) || (e.key !== "a" && e.key !== "A")) return;
    if (peek.hidden || !document.body.classList.contains("peek-selecting")) return;
    if (e.target.closest && e.target.closest("input, textarea, [contenteditable]")) return;
    e.preventDefault();
    const all = document.createRange();
    all.selectNodeContents(peekBody);
    const s = getSelection();
    s.removeAllRanges();
    s.addRange(all);
  });

  /* Where the file holds the text a `content:` query asked for. */
  function peekFoundOf(f) {
    const found = (f && f.found) || [];
    peekFound.hidden = found.length === 0;
    if (!found.length) { peekFound.replaceChildren(); return; }
    const head = document.createElement("h4");
    head.textContent = T("Found in the file");
    peekFound.replaceChildren(head, ...found.map((x) => {
      const line = document.createElement("div");
      line.append(...foundNodes(x));
      return line;
    }));
  }
  const peekToggle = document.getElementById("peek-toggle");
  const peekCut = document.getElementById("peek-cut");
  const viewSearch = document.getElementById("view-search");

  const PEEK = {
    on: false,
    /* What is drawn now, so arrowing back does not restart a video. */
    path: null,
    /* The row it is about, for the buttons; several rows are `SEL`. */
    row: null,
    many: false,
    ctrl: null,
    timer: 0,
    /* All the facts or the one line, as it was last left: the service keeps it. */
    details: false,
    /* A folder's summary, by path and the revision it was asked at. */
    dirs: new Map(),
  };

  /* The row with the keyboard; the list restores focus across a repaint. */
  const focusedRow = () => {
    const el = document.activeElement;
    return el && el.closest ? el.closest("tr[data-path]") : null;
  };

  /* `keeping` is false when the settings asked for this: no write back. */
  function setPeek(on, keeping = true) {
    PEEK.on = on;
    peek.hidden = !on;
    peekToggle.setAttribute("aria-pressed", on ? "true" : "false");
    if (keeping) keep({ preview: on });
    viewSearch.classList.toggle("peeking", on);
    if (!on) {
      /* Emptied on the way out: a hidden `<video>` keeps decoding. */
      peekBody.className = "peek-body";
      peekBody.textContent = "";
      PEEK.path = null;
      if (PEEK.ctrl) PEEK.ctrl.abort();
    } else if (SEL.size > 1) {
      peekMany();
    } else {
      const row = focusedRow();
      if (row) drawPeek(row.__row);
    }
  }

  /* ---- what it is and where ---- */

  /* Each part of the folder a press that searches it, `~` for home: the same
     term "Search this folder" writes. */
  function peekCrumbsOf(dir) {
    const out = [];
    const part = (label, path) => {
      const b = document.createElement("button");
      b.type = "button";
      b.className = "cr";
      b.dataset.dir = path;
      b.textContent = label;
      b.title = T("Search this folder") + ": " + homeForm(path);
      return b;
    };
    const sep = () => {
      const s = document.createElement("span");
      s.textContent = "/";
      return s;
    };
    let at = "";
    let rest = dir;
    /* After `~` a part is set off by a slash; after the root's own `/` not. */
    let apart = false;
    if (HOME && (dir === HOME || dir.startsWith(HOME + "/"))) {
      out.push(part("~", HOME));
      at = HOME;
      rest = dir.slice(HOME.length);
      apart = true;
    } else {
      out.push(part("/", "/"));
    }
    rest.split("/").filter(Boolean).forEach((p) => {
      at += "/" + p;
      if (apart) out.push(sep());
      out.push(part(p, at));
      apart = true;
    });
    return out;
  }
  peekCrumbs.addEventListener("click", (e) => {
    const b = e.target.closest("button[data-dir]");
    if (b) toggleTerm("under:" + quoteIf(homeForm(b.dataset.dir)));
  });

  /* ---- what can be done: three of the menu's own lines, and the menu ---- */

  /* Beside the name, as the menu's own drawings, named on hover: a row of
     their own cost the content a line. */
  function peekActsOf(shape, n) {
    const fits = (w) => (shape === "many" ? w === "many" : w === "one" || w === shape);
    const buttons = (PEEK_BUTTONS[shape] || []).map((id) => {
      const it = MENU.find((m) => m.id === id && fits(m.when));
      const b = document.createElement("button");
      b.type = "button";
      b.className = "pi";
      b.dataset.act = id;
      const said = it ? T(it.msgid, { n: fmt(n) }) + (it.key === "Enter" ? "  ↵" : "") : id;
      b.title = said;
      b.setAttribute("aria-label", said);
      /* The drawing is the build's own, never a file's. */
      b.innerHTML = it && MENU_ICONS[it.icon] ? MENU_ICONS[it.icon] : "";
      return b;
    });
    const more = document.createElement("button");
    more.type = "button";
    more.className = "pi more";
    more.dataset.act = "menu";
    more.textContent = "⋯";
    more.title = T("The whole menu");
    more.setAttribute("aria-label", T("The whole menu"));
    peekActs.replaceChildren(...buttons, more);
  }
  peekActs.addEventListener("click", (e) => {
    const b = e.target.closest("button[data-act]");
    const f = PEEK.row;
    if (!b || !f) return;
    const p = f.path + "/" + f.name;
    if (b.dataset.act === "menu") {
      /* Not on to the page, whose every click shuts the menu. */
      e.stopPropagation();
      const r = b.getBoundingClientRect();
      openRowMenu({ preventDefault() {}, clientX: r.right, clientY: r.bottom + 4 }, p, f, null);
      return;
    }
    runAct(b.dataset.act, p, f, PEEK.many ? [...SEL.keys()] : [p], null);
  });

  /* ---- the facts: one line, the rest behind a press that is kept ---- */

  function peekInfoOf(f) {
    peekInfo.hidden = false;
    /* A folder nobody counted has -1 items, which is not a count. */
    const size = f.is_dir ? (f.items >= 0 ? T("{n} items", { n: fmt(f.items) }) : "") : bytes(f.size || 0);
    const rest = [size, f.mtime ? T("modified {when}", { when: ago(f.mtime) }) : ""].filter(Boolean);
    peekSum.replaceChildren();
    if (f.kind) {
      const b = document.createElement("b");
      b.textContent = f.kind;
      peekSum.append(b);
      if (rest.length) peekSum.append(" · ");
    }
    peekSum.append(rest.join(" · "));
    peekSum.title = peekSum.textContent;
    /* `DETAILS`' order, and the same msgids the column headings use. */
    const rows = [
      [T("Location"), homeForm(f.path)],
      [T("Modified"), exact(f.mtime)],
      [T("Created"), exact(f.ctime)],
      [T("Accessed"), exact(f.atime)],
      [T("Mode"), f.perm || ""],
      [T("Owner"), [f.user, f.group].filter(Boolean).join(" · ")],
    ].filter(([, v]) => v);
    peekFacts.textContent = "";
    for (const [k, v] of rows) addFact(k, v);
    showDetails();
  }
  function showDetails() {
    peekFacts.hidden = !PEEK.details;
    peekMore.textContent = T("Details") + (PEEK.details ? " ▾" : " ▸");
    peekMore.setAttribute("aria-expanded", PEEK.details ? "true" : "false");
  }
  peekMore.addEventListener("click", () => {
    PEEK.details = !PEEK.details;
    showDetails();
    keep({ preview_details: PEEK.details });
  });

  /* Dimensions and length are known only to a decoder, so they arrive from
   * the element rather than costing every row a decode at scan time. */
  function addFact(key, value) {
    if (!value) return;
    const dt = document.createElement("dt");
    dt.textContent = key;
    const dd = document.createElement("dd");
    dd.textContent = value;
    peekFacts.append(dt, dd);
  }

  /* ---- what is in it ---- */

  /* The kind's own picture, large: what stands while the file is asked for,
     so the old file's page is never under the new name. */
  function kindPicture(f) {
    const k = f && f.is_dir ? "folder" : (f && f.ktoken) || "file";
    const ico = document.createElement("span");
    ico.className = "pk-ico k-" + (GLYPHS.has(k) ? k : "file");
    ico.setAttribute("aria-hidden", "true");
    return ico;
  }
  function showKind(f) {
    peekBody.className = "peek-body";
    peekBody.textContent = "";
    peekCut.hidden = true;
    const box = document.createElement("div");
    box.className = "none";
    box.appendChild(kindPicture(f));
    peekBody.appendChild(box);
  }

  /* What cannot be drawn: the file manager's own picture where it made one,
     the kind's otherwise, a sentence, and the desktop's previewer if any. */
  function showNothing(message, f, thumbPath, nativePath) {
    peekBody.className = "peek-body";
    peekBody.textContent = "";
    peekCut.hidden = true;
    const box = document.createElement("div");
    box.className = "none";
    if (thumbPath) {
      const img = document.createElement("img");
      img.src = `/api/icon?${new URLSearchParams({ p: thumbPath, t: TOKEN })}`;
      img.alt = "";
      box.appendChild(img);
    } else {
      box.appendChild(kindPicture(f));
    }
    const said = document.createElement("p");
    said.textContent = message;
    box.appendChild(said);
    if (nativePath) {
      const b = document.createElement("button");
      b.type = "button";
      b.className = "act";
      b.textContent = T("Open in the system previewer");
      b.addEventListener("click", () => launch(nativePath, "preview"));
      box.appendChild(b);
    }
    peekBody.appendChild(box);
  }

  /* A text file, or the stretch of it around a find, a line to a row with its
   * number beside it — counted from the stretch's first line — marked from the
   * same folded terms the name column uses. The lines a content search found
   * are marked, the first brought into sight. Built with nodes: this is
   * somebody else's file and is never parsed. Beside it, the rail. */
  function writePeekText(text, terms, found, view) {
    peekBody.className = "peek-body text";
    peekBody.textContent = "";
    const scroller = document.createElement("div");
    scroller.className = "ptext";
    const pre = document.createElement("pre");
    pre.style.counterReset = `ln ${view.firstLine - 1}`;
    const wanted = terms.filter((t) => t.length > 1);
    const hitLines = new Set(found.map((x) => x.line).filter((n) => n != null));
    const atLine = view.atLine;
    /* Bounded: a one-letter term in a quarter of a megabyte is tens of
       thousands of elements. */
    let left = wanted.length ? 2000 : 0;
    const lines = text.split("\n");
    if (lines.length > 1 && lines[lines.length - 1] === "") lines.pop();
    let marked = null;
    lines.forEach((line, i) => {
      const row = document.createElement("span");
      const hay = fold(line);
      let at = 0;
      while (left > 0) {
        let best = -1, len = 0;
        for (const t of wanted) {
          const k = hay.indexOf(t, at);
          if (k >= 0 && (best < 0 || k < best)) { best = k; len = t.length; }
        }
        if (best < 0) break;
        row.append(line.slice(at, best));
        const m = document.createElement("mark");
        m.textContent = line.slice(best, best + len);
        row.appendChild(m);
        at = best + len;
        left--;
      }
      row.append(line.slice(at));
      const n = view.firstLine + i;
      if (hitLines.has(n)) row.className = "hit";
      if (atLine === n) { row.className = "hit at"; marked = row; }
      pre.appendChild(row);
    });
    scroller.appendChild(pre);
    const rail = document.createElement("div");
    rail.className = "prail";
    rail.setAttribute("role", "presentation");
    rail.title = T("Where the finds are: press to go there");
    peekBody.append(scroller, rail);
    if (marked) scroller.scrollTop = Math.max(0, marked.offsetTop - scroller.clientHeight / 3);
    peekRail(scroller, pre, rail, found, view);
  }

  /* The rail beside a text preview, after Colpan's minimap: where the finds
   * are, where the eye is, and a press that goes there. When only a stretch of
   * a large file is shown the rail is the whole file: the stretch a band on it,
   * finds outside the band marked by their byte, and a press outside it reads
   * the stretch around that place instead. Nothing on it moves by itself. */
  function peekRail(scroller, pre, rail, found, view) {
    const whole = view.from === 0 && !view.cut;
    const len = Math.max(1, view.len);
    const b0 = whole ? 0 : view.from / len;
    const b1 = whole ? 1 : Math.min(1, (view.from + view.bytes) / len);
    const outside = whole ? [] : found
      .filter((x) => x.offset != null && (x.offset < view.from || x.offset >= view.from + view.bytes))
      .map((x) => x.offset);
    const at = (r) => `${(Math.min(1, Math.max(0, r)) * 100).toFixed(3)}%`;
    function draw() {
      const tall = Math.max(1, pre.scrollHeight);
      let html = whole ? "" : `<b class="band" style="top:${at(b0)};height:${at(b1 - b0)}"></b>`;
      for (const row of pre.querySelectorAll("span.hit")) {
        html += `<i style="top:${at(b0 + (row.offsetTop / tall) * (b1 - b0))}"></i>`;
      }
      for (const o of outside) html += `<i class="far" style="top:${at(o / len)}"></i>`;
      html += '<u class="view"></u>';
      rail.innerHTML = html;
      seen();
    }
    function seen() {
      const tall = Math.max(1, scroller.scrollHeight);
      const v = rail.querySelector(".view");
      if (!v) return;
      const top = b0 + (scroller.scrollTop / tall) * (b1 - b0);
      const h = Math.max(0.025, (scroller.clientHeight / tall) * (b1 - b0));
      v.style.top = at(top);
      v.style.height = at(h);
    }
    /* A press inside the band scrolls there, the place in the middle; outside
       it, the nearest find within a few pixels, or the place itself, is read. */
    function go(clientY) {
      const box = rail.getBoundingClientRect();
      const r = Math.min(1, Math.max(0, (clientY - box.top) / box.height));
      if (r >= b0 && r <= b1) {
        const inner = (r - b0) / Math.max(1e-9, b1 - b0);
        scroller.scrollTop = inner * scroller.scrollHeight - scroller.clientHeight / 2;
        return;
      }
      const near = outside.reduce((best, o) => {
        const d = Math.abs(o / len - r) * box.height;
        return d < 8 && (!best || d < best[0]) ? [d, o] : best;
      }, null);
      view.reread(near ? near[1] : Math.floor(r * len));
    }
    let held = false;
    rail.addEventListener("pointerdown", (e) => {
      held = true;
      rail.setPointerCapture(e.pointerId);
      go(e.clientY);
    });
    rail.addEventListener("pointermove", (e) => {
      if (!held) return;
      const box = rail.getBoundingClientRect();
      const r = (e.clientY - box.top) / box.height;
      if (r >= b0 && r <= b1) go(e.clientY);
    });
    rail.addEventListener("pointerup", (e) => { held = false; rail.releasePointerCapture(e.pointerId); });
    rail.addEventListener("wheel", (e) => { e.preventDefault(); scroller.scrollTop += e.deltaY; }, { passive: false });
    scroller.addEventListener("scroll", seen, { passive: true });
    /* The rows move when the panel's width does. */
    new ResizeObserver(draw).observe(scroller);
    draw();
  }

  /* `picture_said`: its pixels and its extension, under it. */
  function pictureSaid(w, h, name) {
    const dot = name.lastIndexOf(".");
    const ext = dot > 0 ? name.slice(dot + 1).toUpperCase() : "";
    return `${w} × ${h}` + (ext ? " · " + ext : "");
  }

  const clock = (s) => {
    if (!isFinite(s) || s <= 0) return "";
    const m = Math.floor(s / 60), sec = Math.round(s % 60);
    return m >= 60
      ? `${Math.floor(m / 60)}:${String(m % 60).padStart(2, "0")}:${String(sec).padStart(2, "0")}`
      : `${m}:${String(sec).padStart(2, "0")}`;
  };

  function media(tag, url, type, name) {
    const el = document.createElement(tag);
    /* Caught now: by the time a large video has metadata, the row may have
       changed. */
    const mine = PEEK.path;
    if (tag === "img") {
      el.alt = "";
      const box = document.createElement("div");
      box.className = "pic";
      const said = document.createElement("div");
      said.className = "cap1";
      el.addEventListener("load", () => {
        if (PEEK.path !== mine) return;
        said.textContent = pictureSaid(el.naturalWidth, el.naturalHeight, name);
        addFact(T("Dimensions"), `${el.naturalWidth} × ${el.naturalHeight}`);
      });
      el.src = url;
      box.append(el, said);
      return box;
    }
    el.controls = true;
    /* No autoplay, and `metadata` gives the length without the whole file. */
    el.preload = "metadata";
    if (type) el.type = type;
    el.addEventListener("loadedmetadata", () => {
      if (PEEK.path !== mine) return;
      addFact(T("Length"), clock(el.duration));
      if (tag === "video" && el.videoWidth) {
        addFact(T("Dimensions"), `${el.videoWidth} × ${el.videoHeight}`);
      }
    });
    el.src = url;
    return el;
  }

  /* A folder: what the index already knows of it, and no file read. Its own
     weight and count, the heaviest of what is in it — the folders by `usage`,
     the files beside them by size — and the newest files anywhere under it. */
  async function peekFolder(full, ctrl) {
    const key = full + "@" + REV;
    let got = PEEK.dirs.get(key);
    if (!got) {
      const here = quoteIf(full);
      const [u, kids, fresh] = await Promise.all([
        SERVICE.get("usage", { path: full, top: 6, q: "" }, ctrl.signal),
        /* `file:`: a folder sorted by size is ordered by what is under it. */
        SERVICE.search("parent:" + here + " file:", "size", true, 6, 0, ctrl.signal),
        SERVICE.search("under:" + here + " file:", "modified", true, 5, 0, ctrl.signal),
      ]);
      got = { u, kids: kids.rows.map(fromService), fresh: fresh.rows.map(fromService) };
      if (PEEK.dirs.size > 32) PEEK.dirs.clear();
      PEEK.dirs.set(key, got);
    }
    if (ctrl.signal.aborted) return;
    writeFolder(got);
  }
  function writeFolder({ u, kids, fresh }) {
    peekBody.className = "peek-body text";
    peekBody.textContent = "";
    const box = document.createElement("div");
    box.className = "dir";
    const head = (said) => {
      const h = document.createElement("h6");
      h.textContent = said;
      return h;
    };
    const heavy = [];
    for (const c of u.children || []) {
      heavy.push({ name: c.path.slice(c.path.lastIndexOf("/") + 1), dir: c.path, bytes: c.bytes });
    }
    for (const f of kids) if (!f.is_dir) heavy.push({ name: f.name, dir: null, bytes: f.size || 0 });
    heavy.sort((a, b) => b.bytes - a.bytes);
    const top = heavy.slice(0, 6);
    const shares = resBars(top.map((x) => x.bytes));
    const root = u.root || { files: 0, bytes: 0 };
    box.appendChild(head(T("Heaviest") + " · " +
      T("{n} items · {size}", { n: fmt(root.files), size: bytes(root.bytes) })));
    top.forEach((x, i) => {
      const row = document.createElement(x.dir ? "button" : "div");
      row.className = "row";
      if (x.dir) {
        row.type = "button";
        row.dataset.dir = x.dir;
        row.title = T("Search this folder") + ": " + homeForm(x.dir);
      }
      const nm = document.createElement("span");
      nm.textContent = x.name;
      const sz = document.createElement("b");
      sz.textContent = bytes(x.bytes);
      const bar = document.createElement("i");
      bar.style.setProperty("--w", (shares[i] * 100).toFixed(1) + "%");
      row.append(nm, sz, bar);
      box.appendChild(row);
    });
    const newest = fresh.filter((f) => !f.is_dir).slice(0, 5);
    if (newest.length) {
      box.appendChild(document.createElement("hr"));
      box.appendChild(head(T("Newest")));
      for (const f of newest) {
        const row = document.createElement("div");
        row.className = "new";
        const nm = document.createElement("span");
        nm.textContent = f.name;
        nm.title = f.path + "/" + f.name;
        const when = document.createElement("b");
        when.textContent = f.mtime ? ago(f.mtime) : "";
        row.append(nm, when);
        box.appendChild(row);
      }
    }
    peekBody.appendChild(box);
  }
  peekBody.addEventListener("click", (e) => {
    const b = e.target.closest(".dir button[data-dir]");
    if (b) toggleTerm("under:" + quoteIf(homeForm(b.dataset.dir)));
  });

  /* `tally`: each kind once with how many of it, the commonest first. */
  function peekTally(kinds) {
    const out = [];
    for (const k of kinds) {
      const seen = out.find((x) => x[0] === k);
      if (seen) seen[1]++;
      else out.push([k, 1]);
    }
    return out.sort((a, b) => b[1] - a[1]);
  }

  /* Several rows: how many, what kinds, how much, across how many folders.
     Nothing is read; the buttons are the menu's lines for several. */
  function peekMany() {
    const rows = [...SEL.values()].filter(Boolean);
    const key = "many:" + SEL.size + ":" + [...SEL.keys()].join("\n").length;
    if (PEEK.path === key) return;
    PEEK.path = key;
    PEEK.many = true;
    clearTimeout(PEEK.timer);
    if (PEEK.ctrl) PEEK.ctrl.abort();
    const focused = focusedRow();
    PEEK.row = (focused && focused.__row) || rows[0] || null;
    const n = SEL.size;
    const folders = new Set(rows.map((f) => f.path)).size;
    peekName.textContent = T("{n} selected", { n: fmt(n) });
    peekName.title = "";
    const where = document.createElement("span");
    where.textContent = folders === 1 ? T("in one folder") : T("in {n} folders", { n: fmt(folders) });
    peekCrumbs.replaceChildren(where);
    peekActsOf("many", n);
    peekInfo.hidden = true;
    peekFound.hidden = true;
    peekCut.hidden = true;
    peekBody.className = "peek-body";
    peekBody.textContent = "";
    const box = document.createElement("div");
    box.className = "many";
    const stack = document.createElement("div");
    stack.className = "stack";
    stack.append(document.createElement("i"), document.createElement("i"), document.createElement("i"));
    const big = document.createElement("b");
    big.textContent = T("{n} selected", { n: fmt(n) });
    const small = document.createElement("small");
    const files = rows.filter((f) => !f.is_dir);
    const parts = peekTally(rows.map((f) => f.kind || "")).map(([k, c]) => `${k} ${fmt(c)}`);
    if (files.length) parts.push(T("{size} in all", { size: bytes(files.reduce((a, f) => a + (f.size || 0), 0)) }));
    small.textContent = parts.join(" · ");
    box.append(stack, big, small);
    peekBody.appendChild(box);
  }

  /* The selection moved: several rows are their own preview, and back to one
     is that row's. Called by the selection bar, which every change redraws. */
  function peekFollowSelection() {
    if (!PEEK.on) return;
    if (SEL.size > 1) { peekMany(); return; }
    if (!PEEK.many) return;
    PEEK.many = false;
    PEEK.path = null;
    const focused = focusedRow();
    const one = (focused && focused.__row) || [...SEL.values()][0];
    if (one) drawPeek(one);
  }

  /* Debounced: arrowing down a list previews where the hand stops. */
  function drawPeek(f) {
    if (!PEEK.on || !f) return;
    if (SEL.size > 1) { peekMany(); return; }
    const full = f.path + "/" + f.name;
    if (full === PEEK.path) return;
    PEEK.path = full;
    PEEK.row = f;
    PEEK.many = false;
    peekName.textContent = f.name;
    peekName.title = full;
    peekCrumbs.replaceChildren(...peekCrumbsOf(f.path));
    peekActsOf(f.is_dir ? "folder" : "file", 1);
    peekInfoOf(f);
    peekFoundOf(f);
    clearTimeout(PEEK.timer);
    if (PEEK.ctrl) PEEK.ctrl.abort();
    showKind(f);
    PEEK.timer = setTimeout(() => {
      const ctrl = new AbortController();
      PEEK.ctrl = ctrl;
      if (f.is_dir) {
        peekFolder(full, ctrl).catch(() => {
          if (!ctrl.signal.aborted) showNothing(T("It could not be previewed."), f);
        });
        return;
      }
      SERVICE.peek(full, ctrl.signal)
        .then((meta) => {
          if (ctrl.signal.aborted) return;
          const url = SERVICE.peekUrl(full);
          const shown = (el) => {
            peekBody.className = "peek-body";
            peekBody.textContent = "";
            peekBody.appendChild(el);
          };
          if (meta.shape === "image") {
            shown(media("img", url, null, f.name));
          } else if (meta.shape === "video") {
            shown(media("video", url, meta.type));
          } else if (meta.shape === "audio") {
            shown(media("audio", url, meta.type));
          } else if (meta.shape === "pdf") {
            /* `<embed>`: the PDF viewer is a plugin, and an iframe would be a
               document this origin answers for. */
            const e = document.createElement("embed");
            e.src = url;
            e.type = "application/pdf";
            shown(e);
          } else if (meta.shape === "text") {
            /* Around the first find, not from the head: in a large file it can
               be megabytes in, past anything the head shows. */
            const found = f.found || [];
            const first = found.find((x) => x.offset != null || x.line != null);
            const read = (place) => fetch(SERVICE.peekUrl(full, place), { cache: "no-store", signal: ctrl.signal })
              .then((r) => r.text().then((t) => [r, t]))
              .then(([r, t]) => {
                if (ctrl.signal.aborted) return;
                const firstLine = Number(r.headers.get("X-Scour-First-Line")) || 1;
                const from = Number(r.headers.get("X-Scour-From")) || 0;
                const size = new Blob([t]).size;
                const cut = r.headers.get("X-Scour-Complete") === "0";
                /* The find the stretch was read for, or the first one in it. */
                const asked = place && place.at != null
                  ? found.find((x) => x.offset === place.at) : first;
                /* Read at draw time: the query moves while a panel is open. */
                writePeekText(t, parse(q.value).text, found, {
                  firstLine, from, cut, bytes: size, len: meta.size,
                  atLine: asked && asked.line != null ? asked.line : 0,
                  reread: (offset) => read({ at: offset }),
                });
                peekCut.hidden = !cut;
                if (cut) {
                  peekCut.textContent = firstLine > 1
                    ? T("from line {n} — the file is {total}", { n: fmt(firstLine), total: bytes(meta.size) })
                    : T("the first {shown} — the file is {total}", { shown: bytes(size), total: bytes(meta.size) });
                }
              })
              .catch(() => {});
            read(first ? (first.offset != null ? { at: first.offset } : { line: first.line }) : null);
          } else {
            showNothing(T("This kind of file cannot be shown here."), f,
                        meta.thumb ? full : null, meta.native ? full : null);
          }
        })
        .catch((e) => {
          if (ctrl.signal.aborted) return;
          /* 403 is a decision rather than a failure: `--no-preview`. */
          showNothing(String(e).includes("403")
            ? T("Previewing is off (--no-preview).")
            : T("It could not be previewed."), f);
        });
    }, 120);
  }

  /* Space toggles only inside the list; elsewhere it still scrolls. */
  body.addEventListener("keydown", (e) => {
    if (e.key !== " " || e.ctrlKey || e.altKey || e.metaKey) return;
    const row = e.target.closest("tr[data-path]");
    if (!row) return;
    e.preventDefault();
    if (PEEK.on && row.__row && row.__row.path + "/" + row.__row.name === PEEK.path) {
      setPeek(false);
    } else {
      PEEK.path = null;
      setPeek(true);
      drawPeek(row.__row);
    }
  });
  peekToggle.addEventListener("click", () => setPeek(!PEEK.on));

  /* Whatever takes the focus is what is previewed: one rule for all three. */
  body.addEventListener("focusin", (e) => {
    const row = e.target.closest && e.target.closest("tr[data-path]");
    if (!row) return;
    if (row.dataset.path) focusPath = row.dataset.path;
    if (PEEK.on) drawPeek(row.__row);
  });
  document.getElementById("peek-close").addEventListener("click", () => {
    setPeek(false);
    const row = focusedRow();
    if (row) row.focus();
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && PEEK.on && !peek.hidden) setPeek(false);
  });

