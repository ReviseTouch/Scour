  /* ---------------- the content field's launcher ----------------
   * `:` in a text pattern offers a piece to start at the caret, or the case
   * to flip (`pattern::launch`), in the query launcher's rows, under the `:`.
   * The letters after it narrow it; none left and the `:` is text. Escape
   * leaves the `:` as typed, and that `:` does not ask again. */

  const CONTENT_ART = {
    quote: SVG('<path d="M3.5 5.5h3v2.8c0 1.6-1 2.6-2.6 3.1M9.5 5.5h3v2.8c0 1.6-1 2.6-2.6 3.1"/>'),
    or: SVG('<path d="M2.5 8h4l3-3.5h4M6.5 8l3 3.5h4"/>'),
    not: SVG('<circle cx="8" cy="8" r="5.5"/><path d="M4.1 11.9l7.8-7.8"/>'),
    approx: FIELD_ART.fuzzy,
    case: '<span class="aa">Aa</span>',
  };

  const grepLaunchEl = document.getElementById("grep-launch");
  /* What the open launcher answers: its `:`, rows and the one picked; `shut`
     the `:` Escape left as text. */
  const GLAUNCH = { colon: -1, items: [], sel: 0, shut: -1 };

  function grepLaunchClose() {
    grepLaunchEl.hidden = true;
    GLAUNCH.colon = -1;
    GLAUNCH.items = [];
  }

  function drawGrepLaunch() {
    grepLaunchEl.innerHTML = GLAUNCH.items.map((it, i) =>
      '<div class="lopt lrow read' + (i === GLAUNCH.sel ? " on" : "") + '" role="option" data-i="' + i +
      '" aria-selected="' + (i === GLAUNCH.sel) + '"><span class="ic">' + CONTENT_ART[it.icon] + "</span>" +
      '<span class="l"><span class="nm">' + escape_(T(it.label)) + '</span><span class="ab">' + escape_(T(it.about)) +
      '</span></span><span class="in"></span><span class="k">' + escape_(it.sign) + "</span></div>").join("");
  }

  /* Opened, narrowed, moved or closed by where the caret is now. */
  function grepLaunchSync() {
    const v = grepInput.value, s = grepInput.selectionStart ?? 0;
    const ask = GREP.mode === "text" && document.activeElement === grepInput && s === grepInput.selectionEnd
      ? contentAsking(v, grepCol(v, s)) : null;
    if (!ask) GLAUNCH.shut = -1;
    const items = ask && ask[0] !== GLAUNCH.shut ? contentItems(ask[1], T) : [];
    if (!items.length) { grepLaunchClose(); return; }
    if (ask[0] !== GLAUNCH.colon || GLAUNCH.sel >= items.length) GLAUNCH.sel = 0;
    GLAUNCH.colon = ask[0];
    GLAUNCH.items = items;
    drawGrepLaunch();
    grepLaunchEl.hidden = false;
    const x = grepCaretBox(grepUnit(v, ask[0]));
    const bar = grepBar.getBoundingClientRect();
    const w = grepLaunchEl.offsetWidth;
    grepLaunchEl.style.left = Math.max(8, Math.min(x - 16, innerWidth - w - 8)) + "px";
    grepLaunchEl.style.top = (bar.bottom + 4) + "px";
  }

  function grepLaunchChoose(i) {
    const item = GLAUNCH.items[i];
    if (!item) return;
    const v = grepInput.value;
    const [out, at] = contentChoose(v, GLAUNCH.colon, grepCol(v, grepInput.selectionStart ?? 0), item);
    if (item.write === null) {
      GREP.exact = !GREP.exact;
      grepCaseBtn.setAttribute("aria-pressed", String(GREP.exact));
    }
    grepLaunchClose();
    grepSetText(out, grepUnit(out, at));
  }

  /* A key the open launcher takes; false when it is the field's. */
  function grepLaunchKey(e) {
    if (grepLaunchEl.hidden) return false;
    const n = GLAUNCH.items.length;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      GLAUNCH.sel = (GLAUNCH.sel + (e.key === "ArrowDown" ? 1 : n - 1)) % n;
      drawGrepLaunch();
    } else if (e.key === "Enter" || e.key === "Tab") {
      grepLaunchChoose(GLAUNCH.sel);
    } else if (e.key === "Escape") {
      GLAUNCH.shut = GLAUNCH.colon;
      grepLaunchClose();
    } else return false;
    e.preventDefault();
    return true;
  }

  grepLaunchEl.addEventListener("mousedown", (e) => {
    const row = e.target.closest(".lopt");
    e.preventDefault();
    if (row) grepLaunchChoose(+row.dataset.i);
  });

