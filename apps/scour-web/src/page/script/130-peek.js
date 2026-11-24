  /* Toggled with Space, as every quick-look is. What can be shown is asked of
   * the bridge, and the bytes are reached by pointing an element at the URL:
   * a blob would hold the whole file to show one frame. */
  const peek = document.getElementById("peek");
  const peekBody = document.getElementById("peek-body");
  const peekName = document.getElementById("peek-name");
  const peekFacts = document.getElementById("peek-facts");
  const peekFound = document.getElementById("peek-found");
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
  const peekNative = document.getElementById("peek-native");
  const viewSearch = document.getElementById("view-search");

  const PEEK = {
    on: false,
    /* What is drawn now, so arrowing back does not restart a video. */
    path: null,
    ctrl: null,
    timer: 0,
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
      showNothing("");
      PEEK.path = null;
      if (PEEK.ctrl) PEEK.ctrl.abort();
    } else {
      const row = focusedRow();
      if (row) drawPeek(row.__row);
    }
  }

  /* From the row the list already has, and no request. */
  function peekFactsOf(f) {
    const full = f.path + "/" + f.name;
    const shown = HOME && f.path.startsWith(HOME + "/")
      ? "~" + f.path.slice(HOME.length) : f.path;
    /* The same msgids the column headings use, so one field has one name. */
    const rows = [
      [T("Location"), shown],
      [T("Kind"), f.kind || ""],
      [f.is_dir ? T("Contents") : T("Size"),
       f.is_dir ? (f.items ? T("{n} items", { n: fmt(f.items) }) : "") : bytes(f.size)],
      [T("Modified"), f.mtime ? ago(f.mtime) : ""],
      [T("Created"), f.ctime ? ago(f.ctime) : ""],
      [T("Accessed"), f.atime ? ago(f.atime) : ""],
      [T("Mode"), f.perm || ""],
      [T("Owner"), [f.user, f.group].filter(Boolean).join(" · ")],
    ].filter(([, v]) => v);
    peekFacts.textContent = "";
    for (const [k, v] of rows) {
      const dt = document.createElement("dt");
      dt.textContent = k;
      const dd = document.createElement("dd");
      dd.textContent = v;
      peekFacts.append(dt, dd);
    }
    peekName.textContent = f.name;
    peekName.title = full;
  }

  function showNothing(message, thumbPath) {
    peekBody.className = "peek-body";
    peekBody.textContent = "";
    peekCut.hidden = true;
    const box = document.createElement("div");
    box.className = "none";
    /* The file manager's own picture, all there is for a format a browser
       cannot open. */
    if (thumbPath) {
      const img = document.createElement("img");
      img.src = `/api/icon?${new URLSearchParams({ p: thumbPath, t: TOKEN })}`;
      img.alt = "";
      box.appendChild(img);
    }
    box.appendChild(document.createTextNode(message));
    peekBody.appendChild(box);
  }

  /* The head of a text file, marked from the same folded terms the name column
   * uses. Built with nodes: this is somebody else's file and is never parsed. */
  function writePeekText(text, terms) {
    peekBody.className = "peek-body text";
    peekBody.textContent = "";
    const pre = document.createElement("pre");
    const hay = fold(text);
    const wanted = terms.filter((t) => t.length > 1);
    let at = 0;
    /* Bounded: a one-letter term in a quarter of a megabyte is tens of
       thousands of elements. */
    let left = wanted.length ? 2000 : 0;
    while (left > 0) {
      let best = -1, len = 0;
      for (const t of wanted) {
        const i = hay.indexOf(t, at);
        if (i >= 0 && (best < 0 || i < best)) { best = i; len = t.length; }
      }
      if (best < 0) break;
      pre.append(text.slice(at, best));
      const m = document.createElement("mark");
      m.textContent = text.slice(best, best + len);
      pre.appendChild(m);
      at = best + len;
      left--;
    }
    pre.append(text.slice(at));
    peekBody.appendChild(pre);
  }

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

  const clock = (s) => {
    if (!isFinite(s) || s <= 0) return "";
    const m = Math.floor(s / 60), sec = Math.round(s % 60);
    return m >= 60
      ? `${Math.floor(m / 60)}:${String(m % 60).padStart(2, "0")}:${String(sec).padStart(2, "0")}`
      : `${m}:${String(sec).padStart(2, "0")}`;
  };

  function media(tag, url, type) {
    const el = document.createElement(tag);
    /* Caught now: by the time a large video has metadata, the row may have
       changed. */
    const mine = PEEK.path;
    if (tag === "img") {
      el.alt = "";
      el.addEventListener("load", () => {
        if (PEEK.path === mine) addFact(T("Dimensions"), `${el.naturalWidth} × ${el.naturalHeight}`);
      });
    } else {
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
    }
    el.src = url;
    return el;
  }

  /* Debounced: arrowing down a list previews where the hand stops. */
  function drawPeek(f) {
    if (!PEEK.on || !f) return;
    const full = f.path + "/" + f.name;
    if (full === PEEK.path) return;
    PEEK.path = full;
    peekFactsOf(f);
    peekFoundOf(f);
    clearTimeout(PEEK.timer);
    if (PEEK.ctrl) PEEK.ctrl.abort();
    peekBody.className = "peek-body";
    peekBody.textContent = "";
    peekCut.hidden = true;
    /* Hidden until the bridge says there is a previewer. */
    peekNative.hidden = true;
    if (f.is_dir) {
      showNothing(T("A folder — what is in it is in the list."));
      return;
    }
    PEEK.timer = setTimeout(() => {
      const ctrl = new AbortController();
      PEEK.ctrl = ctrl;
      SERVICE.peek(full, ctrl.signal)
        .then((meta) => {
          if (ctrl.signal.aborted) return;
          const url = SERVICE.peekUrl(full);
          /* Several desktops have no previewer; the bridge looks for it. */
          peekNative.hidden = !meta.native;
          if (meta.shape === "image") {
            peekBody.appendChild(media("img", url));
          } else if (meta.shape === "video") {
            peekBody.appendChild(media("video", url, meta.type));
          } else if (meta.shape === "audio") {
            peekBody.appendChild(media("audio", url, meta.type));
          } else if (meta.shape === "pdf") {
            /* `<embed>`: the PDF viewer is a plugin, and an iframe would be a
               document this origin answers for. */
            const e = document.createElement("embed");
            e.src = url;
            e.type = "application/pdf";
            peekBody.appendChild(e);
          } else if (meta.shape === "text") {
            fetch(url, { cache: "no-store", signal: ctrl.signal })
              .then((r) => r.text().then((t) => [r, t]))
              .then(([r, t]) => {
                if (ctrl.signal.aborted) return;
                /* Read at draw time: the query moves while a panel is open. */
                writePeekText(t, parse(q.value).text);
                if (r.headers.get("X-Scour-Complete") === "0") {
                  peekCut.textContent = T("the first {shown} — the file is {total}", {
                    shown: bytes(new Blob([t]).size), total: bytes(meta.size),
                  });
                  peekCut.hidden = false;
                }
              })
              .catch(() => {});
          } else {
            showNothing(T("This kind of file cannot be shown in a browser."),
                        meta.thumb ? full : null);
          }
        })
        .catch((e) => {
          if (ctrl.signal.aborted) return;
          /* 403 is a decision rather than a failure: `--no-preview`. */
          showNothing(String(e).includes("403")
            ? T("Previewing is off (--no-preview).")
            : T("It could not be previewed."));
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
  peekNative.addEventListener("click", () => {
    if (PEEK.path) launch(PEEK.path, "preview");
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && PEEK.on && !peek.hidden) setPeek(false);
  });

