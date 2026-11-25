  /* ---------------- the scope chip ----------------
   * At the left of the query: a dot for each disk the search can look on, in
   * its kind's colour, a ringed one for a disk waiting for an answer, and
   * where the query looks. Drawn from `volumes`, `sources` and the text; it
   * holds nothing. A click opens the place lens as the Location button does. */
  const scopeChip = document.getElementById("qscope");
  let scopeDrawn = "";

  /* The chip's dots and every name it could say, for `text`. */
  function scopeSays(text, home, disks, sources) {
    const tiles = diskTiles(disks.map(diskIn), sources, () => null);
    return { dots: chipDots(tiles, placeScope(text, home)), spots: spots(text, home, tiles) };
  }

  /* As many names as the chip's width holds, the rest as "+k". */
  function scopeLabel(list) {
    const nm = scopeChip.querySelector(".nm");
    const room = nm ? nm.clientWidth : 0;
    const n = list.filter((s) => !s.not).length;
    if (!room || n <= 1) return chipLabel(list, n, T("Everywhere"));
    const ctx = (scopeLabel.ctx ||= document.createElement("canvas").getContext("2d"));
    ctx.font = getComputedStyle(nm).font;
    for (let max = n; max > 1; max--) {
      if (ctx.measureText(chipLabel(list, max, "")).width <= room) return chipLabel(list, max, "");
    }
    return chipLabel(list, 1, "");
  }

  function drawScopeChip(text) {
    if (!scopeChip) return;
    const { dots, spots: list } = scopeSays(text, HOME, PLACE_DISKS, PLACE_SOURCES);
    const dotsHtml = '<span class="dots">' + dots.map((d) =>
      '<i class="dot' + (d.asking ? " ask" : d.lit ? "" : " dim") + '" style="--c:var(--disk-' + d.kind + ')"></i>').join("") + "</span>";
    if (!scopeChip.querySelector(".nm")) scopeChip.innerHTML = dotsHtml + '<span class="nm"></span>';
    const label = scopeLabel(list);
    // A fixed width, so the field does not move as the names change: all of them here.
    const title = T("Where it searches") + ": " + chipLabel(list, list.length, T("Everywhere"));
    const html = dotsHtml + '<span class="nm">' + escape_(label) + "</span>";
    if (html + title === scopeDrawn) return;
    scopeDrawn = html + title;
    scopeChip.innerHTML = html;
    scopeChip.title = title;
  }

  if (scopeChip) {
    scopeChip.addEventListener("mousedown", (e) => {
      e.preventDefault();
      if (LENS && LENS.kind === "place") { lensClose(); return; }
      openOrFocusLensField("under");
    });
  }
  SERVICE.sources().then((r) => placeHeard(null, r.sources || [])).catch(() => {});

