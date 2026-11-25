  /* ---------------- what a capsule does to the list ----------------
   * Three things drawn from answers the page has, over the capsules and never
   * in their line: the floating pill's effect ("18.420 → 212"), the red mark
   * on a capsule whose removal would bring an empty list back, and a dashed
   * frame round terms joined by `|`. The text stays the only model. */

  /* The query a capsule's effect is counted without: every term drawn in
     [a, b] — a group capsule's whole run — out, each with its `|`
     (`lens::remove_term`), later ones first so the earlier offsets hold. */
  function effectWithout(text, a, b) {
    let out = text;
    const inside = placeTerms(text).filter((t) => t.start >= a && t.end <= b).sort((x, y) => y.start - x.start);
    for (const t of inside) out = placeRemove(out, t)[0];
    return out.replace(/\s+/g, " ").trim();
  }

  /* The empty list's "each filter left out on its own" answer: the text it
     was for, and by term text what leaving that term out would find. */
  let RESCUE = null;

  /* The rescue a capsule at [a, b] stands for: one of the terms drawn in it
     that the answer for this very text names; null otherwise. */
  function rescueOf(text, a, b, rescue) {
    if (!rescue || rescue.text !== text) return null;
    for (const [s, e] of terms(text)) {
      if (s >= a && e <= b && rescue.by.has(text.slice(s, e))) return rescue.by.get(text.slice(s, e));
    }
    return null;
  }

  /* The runs of terms joined by `|` that are framed: two or more members,
     not drawn as one capsule already (`caps`, the place capsules). */
  function orRuns(text, caps) {
    return placeRuns(text)
      .filter((r) => r.length > 1 && !caps.some((c) => r[0].start >= c.start && r[r.length - 1].end <= c.end))
      .map((r) => ({ start: r[0].start, end: r[r.length - 1].end, members: r.map((t) => t.start) }));
  }

  /* ---- drawn ---- */

  const EFFECT_WAIT = 250;
  /* Counts without a capsule, by query text and its offsets: the same
     question is asked once. */
  const effectSeen = new Map();
  let effectTimer = 0, typedAt = 0;
  q.addEventListener("input", () => { typedAt = Date.now(); clearTimeout(effectTimer); }, true);

  const countSaid = (n, capped) => fmt(n) + (capped ? "+" : "");

  /* The pill's slot for the capsule at [a, b]: what leaving it out would do.
     Filled from the rescue answer when the list is empty; otherwise one
     `count`, asked once the pointer has rested a moment and nobody types. The
     slot grows to the right of the buttons, which never move. */
  function pillEffect(a, b, slot) {
    clearTimeout(effectTimer);
    const text = q.value;
    const rescue = rescueOf(text, a, b, RESCUE);
    if (rescue) {
      slot.textContent = T("without it: {n} results", { n: countSaid(rescue.n, rescue.capped) });
      return;
    }
    const el = termEl(a);
    if (!el || !el.classList.contains("cap") || el.classList.contains("idle") || READS.test(text)) return;
    const key = text + "\n" + a + "\n" + b;
    const say = (r) => {
      if (LIST.query !== text) return;
      slot.textContent = countSaid(r.n, r.capped) + " → " + countSaid(LIST.total, !LIST.exact);
    };
    if (effectSeen.has(key)) { say(effectSeen.get(key)); return; }
    effectTimer = setTimeout(() => {
      const still = () => hoverTerm && hoverTerm[0] === a && hoverTerm[1] === b && q.value === text;
      if (!still() || Date.now() - typedAt < 600) return;
      SERVICE.get("count", { q: effectWithout(text, a, b), cap: COUNT_CAP })
        .then((r) => {
          effectSeen.set(key, { n: r.total, capped: r.capped });
          const now = acts.querySelector(".fx");
          if (still() && now) say(effectSeen.get(key));
        })
        .catch(() => {});
    }, EFFECT_WAIT);
  }

  /* The red mark on each capsule whose removal would find something. */
  function markRescues() {
    if (!shadow.querySelectorAll) return;
    for (const el of shadow.querySelectorAll(".term.cap")) {
      el.classList.toggle("rescues", !!rescueOf(q.value, +el.dataset.a, +el.dataset.b, RESCUE));
    }
  }

  /* The boxes the layer draws a run's letters and capsules in, its `|`s with them. */
  function runRects(run) {
    const from = SEGS.find((s) => run.start >= s.start && run.start < s.end);
    const to = SEGS.find((s) => run.end > s.start && run.end <= s.end);
    if (!from || !to) return [];
    const range = document.createRange();
    range.setStart(from.node, run.start - from.start);
    range.setEnd(to.node, run.end - to.start);
    // A capsule the range starts or ends inside is not in its boxes: its own are.
    const own = run.members.flatMap((at) => [...(termEl(at)?.getClientRects() || [])]);
    return [...range.getClientRects(), ...own].filter((r) => r.width > 0.5 && r.height > 4);
  }

  /* A dashed frame round each `|` run, one box a line, over the layer: it
     takes no room, so no line and no capsule moves. */
  let framesEl = null;
  function drawFrames(text) {
    if (!framesEl) {
      framesEl = document.createElement("div");
      framesEl.className = "qframes";
      framesEl.setAttribute("aria-hidden", "true");
      qfield.insertBefore(framesEl, decor);
    }
    const box = qfield.getBoundingClientRect();
    let html = "";
    for (const run of orRuns(text, PLACE_DRAWN)) {
      const lines = [];
      for (const r of runRects(run)) {
        const mid = r.top + r.height / 2;
        const line = lines.find((l) => Math.abs(l.mid - mid) < 12);
        if (line) Object.assign(line, { l: Math.min(line.l, r.left), r: Math.max(line.r, r.right), t: Math.min(line.t, r.top), b: Math.max(line.b, r.bottom) });
        else lines.push({ mid, l: r.left, r: r.right, t: r.top, b: r.bottom });
      }
      for (const l of lines) {
        html += '<div class="qor" style="left:' + (l.l - box.left - 4) + "px;top:" + (l.t - box.top - 2) +
          "px;width:" + (l.r - l.l + 8) + "px;height:" + (l.b - l.t + 4) + 'px"></div>';
      }
    }
    framesEl.innerHTML = html;
  }

