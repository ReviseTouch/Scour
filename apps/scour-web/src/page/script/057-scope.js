  /* ---------------- the scope chip ----------------
   * At the left of the query: a dot for each disk the search can look on, in
   * its kind's colour, a ringed one for a disk waiting for an answer, and
   * where the query looks. Drawn from `volumes`, `sources` and the text; it
   * holds nothing. A click opens the place lens as the Location button does. */
  const scopeChip = document.getElementById("qscope");
  let scopeDrawn = "";

  /* The dots and the names the chip says, for `text`. No names: everywhere. */
  function scopeSays(text, home, disks, sources) {
    const ins = disks.map(diskIn);
    const tiles = diskTiles(ins, sources, () => null);
    const scope = placeScope(text, home);
    const whole = tiles.map((tl) => tl.count !== null && tl.roots.every((r) => scopeHolds(scope, r)));
    const on = tiles.map((tl) => scope.within.some((h) => diskOf(h.path, ins) === tl.id));
    const dots = tiles
      .map((tl, i) => ({ kind: tl.kind, asking: tl.shown === "asking", lit: tl.count !== null && (!scope.within.length || on[i]), on: tl.count !== null }))
      .filter((d) => d.on || d.asking)
      .map(({ kind, asking, lit }) => ({ kind, asking, lit }));
    const covered = new Set(tiles.flatMap((tl, i) => (whole[i] ? tl.roots.map(untrailed) : [])));
    const leaf = (p) => (homeIn(home, p) === "~" ? "~" : p === "/" ? "/" : p.slice(p.lastIndexOf("/") + 1));
    const names = [
      ...tiles.filter((_, i) => whole[i]).map((tl) => tl.name),
      ...scope.within.filter((h) => !covered.has(h.path)).map((h) => leaf(h.path)),
    ];
    return { dots, names: [...new Set(names)] };
  }

  function drawScopeChip(text) {
    if (!scopeChip) return;
    const { dots, names } = scopeSays(text, HOME, PLACE_DISKS, PLACE_SOURCES);
    const html = '<span class="dots">' + dots.map((d) =>
      '<i class="dot' + (d.asking ? " ask" : d.lit ? "" : " dim") + '" style="--c:var(--disk-' + d.kind + ')"></i>').join("") +
      '</span><span class="nm">' + escape_(names.length ? names.join(" + ") : T("Everywhere")) + "</span>";
    // A fixed width, so the field does not move as the names change: all of them here.
    const title = T("Where it searches") + ": " + (names.length ? names.join(" + ") : T("Everywhere"));
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

