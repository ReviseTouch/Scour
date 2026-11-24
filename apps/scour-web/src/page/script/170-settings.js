  /* ---------------- the settings page ----------------
   * The panels that were four buttons are moved in once and shown with their
   * section; their own code fills them as before. What the service holds is
   * read back from `KEPT`, which `applySettings` keeps. */
  const settingsBox = document.getElementById("settings");
  const settingsBtn = document.getElementById("settings-btn");
  document.getElementById("set-faces").appendChild(facesPanel);
  document.getElementById("set-disks").appendChild(disksPanel);
  document.getElementById("set-rules").appendChild(rulesPanel);
  document.getElementById("set-keys").appendChild(helpBox);
  document.getElementById("set-hotkey").appendChild(document.getElementById("hotkey-body"));
  const SECTIONS = {
    general: ["General", "The same for every face: the window, the browser and the terminal read these."],
    view: ["List", "How the list is drawn, and what it shows."],
    search: ["Searching", "How the words you type are read. The same in every face."],
    disks: ["Disks and folders", "Scour knows every file on these. A disk taken out leaves the index; putting it back takes a scan."],
    rules: ["Skipped", "Places inside the scanned disks that are not looked at. A rule switched off is kept, and can be switched on again."],
    keys: ["Keyboard", "The key that opens Scour, what the keys do, and what the query understands."],
    index: ["Index", "Where the list of files is kept. It is built from the disks and kept current by itself; building it again is for when something looks wrong."],
    performance: ["Performance", "How Scour spends memory and the processor. Less of either costs a little speed."],
    about: ["About", "What is running, and what it holds."],
  };
  let SECTION = "general";

  /* A few buttons, side by side, the one in force raised. */
  function seg(id, items, current, pick) {
    const el = document.getElementById(id);
    el.innerHTML = items.map(([value, label]) =>
      `<button type="button" data-v="${esc(value)}" class="${value === current ? "on" : ""}">` +
      `${esc(label)}</button>`).join("");
    el.onclick = (e) => {
      const b = e.target.closest("button[data-v]");
      if (b) pick(b.dataset.v);
    };
  }

  function settingsBtnAsking(on) {
    const b = document.getElementById("settings-btn");
    if (b) b.classList.toggle("asking", on);
  }

  function applyTheme(theme) {
    if (theme === "light" || theme === "dark") document.documentElement.dataset.theme = theme;
    else delete document.documentElement.dataset.theme;
  }

  function drawSettings() {
    const k = KEPT || {};
    seg("seg-lang", [["", T("The desktop's")], ...LANGUAGES.map(({ tag, name }) => [tag, name])],
      k.language || "", (v) => { KEPT.language = v; chooseLanguage(v); drawSettings(); });
    seg("seg-face", [["window", T("Window")], ["browser", T("Browser")], ["tui", T("Terminal")]],
      k.face || "window", (v) => {
        KEPT.face = v; keep({ face: v });
        /* Another face chosen: the foot offers to start it now. */
        ARMED = v === "browser" ? "" : v;
        drawFaces();
        drawSettings();
      });
    seg("seg-theme", [["", T("The desktop's")], ["light", T("Light")], ["dark", T("Dark")]],
      k.theme || "", (v) => { KEPT.theme = v; applyTheme(v); keep({ theme: v }); drawSettings(); });
    const mode = (document.querySelector("#views button[aria-pressed=true]") || {}).dataset;
    seg("seg-layout", [["detail", T("Details")], ["icons", T("Icons")], ["large", T("Large icons")]],
      (mode && mode.view) || "detail", (v) => {
        const b = document.querySelector(`#views button[data-view="${v}"]`);
        if (b) b.click();
        drawSettings();
      });
    const peekOn = peekToggle.getAttribute("aria-pressed") === "true";
    seg("seg-peek", [["on", T("On")], ["off", T("Off")]], peekOn ? "on" : "off", (v) => {
      if ((v === "on") !== (peekToggle.getAttribute("aria-pressed") === "true")) peekToggle.click();
      drawSettings();
    });
    seg("seg-refresh",
      [["1", T("1 s")], ["2", T("2 s")], ["5", T("5 s")], ["15", T("15 s")], ["60", T("1 min")]],
      String(k.refresh_every || 2),
      (v) => { KEPT.refresh_every = Number(v); keep({ refresh_every: Number(v) }); drawSettings(); });
    /* The performance section: the service takes each as it is written. */
    seg("seg-memory",
      [["", T("Automatic")], ["now", T("At once")], ["1m", T("1 min")], ["5m", T("5 min")]],
      k.memory || "",
      (v) => { KEPT.memory = v; keep({ memory: v }); drawSettings(); });
    seg("seg-narrow", [["on", T("On")], ["off", T("Off")]], k.narrow_off ? "off" : "on", (v) => {
      KEPT.narrow_off = v === "off";
      keep({ narrow_off: KEPT.narrow_off });
      if (KEPT.narrow_off) { CHIPS = []; drawNarrow(); }
      drawSettings();
    });
    seg("seg-texts", [["on", T("On")], ["off", T("Off")]], k.texts_off ? "off" : "on", (v) => {
      KEPT.texts_off = v === "off";
      keep({ texts_off: KEPT.texts_off });
      setTimeout(loadTexts, 300);
      drawSettings();
    });
    seg("seg-threads",
      [["0", T("Automatic")], ["1", "1"], ["2", "2"], ["4", "4"], ["8", "8"]],
      String(k.search_threads || 0),
      (v) => { KEPT.search_threads = Number(v); keep({ search_threads: Number(v) }); drawSettings(); });
    seg("seg-fresh",
      [["0", T("As configured")], ["1", T("1 s")], ["5", T("5 s")], ["15", T("15 s")], ["60", T("1 min")]],
      String(k.fresh_within || 0),
      (v) => { KEPT.fresh_within = Number(v); keep({ fresh_within: Number(v) }); drawSettings(); });
    seg("seg-tabs", [["on", T("On")], ["off", T("Off")]], k.tabs_off ? "off" : "on", (v) => {
      KEPT.tabs_off = v === "off";
      drawTabs();
      keep({ tabs_off: KEPT.tabs_off });
      drawSettings();
    });
    seg("seg-sysicons", [["on", T("On")], ["off", T("Off")]], k.system_icons ? "on" : "off", (v) => {
      KEPT.system_icons = v === "on";
      keep({ system_icons: KEPT.system_icons });
      repaint(true);
      drawSettings();
    });
    seg("seg-rail", [["on", T("On")], ["off", T("Off")]], k.rail_shown ? "on" : "off", (v) => {
      setPanes({ rail_shown: v === "on" });
      drawSettings();
    });
    seg("seg-ribbon", [["on", T("On")], ["off", T("Off")]], k.ribbon_shown ? "on" : "off", (v) => {
      setPanes({ ribbon_shown: v === "on" });
      drawSettings();
    });
    seg("seg-grid", [["on", T("On")], ["off", T("Off")]], k.grid_lines ? "on" : "off", (v) => {
      KEPT.grid_lines = v === "on";
      applyGrid();
      keep({ grid_lines: KEPT.grid_lines });
      drawSettings();
    });
    seg("seg-vgrid", [["on", T("On")], ["off", T("Off")]], k.grid_columns ? "on" : "off", (v) => {
      KEPT.grid_columns = v === "on";
      applyGrid();
      keep({ grid_columns: KEPT.grid_columns });
      drawSettings();
    });
    seg("seg-accents", [["on", T("On")], ["off", T("Off")]], k.accents_exact ? "off" : "on", (v) => {
      KEPT.accents_exact = v === "off";
      keep({ accents_exact: KEPT.accents_exact });
      drawSettings();
    });
    seg("seg-typos", [["off", T("Off")], ["", T("When nothing is found")], ["always", T("Always")]],
      k.typos === "off" || k.typos === "always" ? k.typos : "", (v) => {
        KEPT.typos = v;
        keep({ typos: v });
        drawSettings();
      });
    seg("seg-removable", [["", T("Ask")], ["always", T("Always scan")], ["never", T("Never")]],
      k.removable || "", (v) => { KEPT.removable = v; keep({ removable: v }); loadDisks(); drawSettings(); });
    seg("seg-network", [["", T("Ask")], ["never", T("Never")]],
      k.network || "", (v) => { KEPT.network = v; keep({ network: v }); loadDisks(); drawSettings(); });
    seg("seg-every",
      [["1", T("1 min")], ["5", T("5 min")], ["10", T("10 min")], ["30", T("30 min")], ["60", T("1 hour")]],
      String(k.folders_every || 10),
      (v) => { KEPT.folders_every = Number(v); keep({ folders_every: Number(v) }); loadDisks(); drawSettings(); });
    drawFolders();
  }

  /* Where the index is and what it holds; asked again while the section is
     open, so a reset is seen filling up. */
  let INDEX_SURE = false;
  function loadIndex() {
    SERVICE.get("index", {}).then((st) => {
      let facts = `${fmt(st.entries)} ${T("entries")} · ${bytes(st.bytes_on_disk)}`;
      if (st.written_at) facts += ` · ${T("written")} ${exact(st.written_at)}`;
      const where = document.getElementById("index-where");
      where.textContent = "";
      where.append(st.dir || "", document.createElement("br"), facts);
      document.getElementById("index-open").disabled = !st.dir;
    }).catch(() => {});
    loadTexts();
    drawIndexReset();
  }
  /* Documents' kept text, beside the index it serves. */
  function loadTexts() {
    SERVICE.get("texts", {}).then((x) => {
      const facts = document.getElementById("texts-facts");
      facts.textContent = !x.reader
        ? T("No document reader is installed.")
        : !x.on
          ? T("Not kept: every search reads them again.")
          : `${fmt(x.documents)} ${T("with text")} · ${fmt(x.no_text)} ${T("without text")} · ` +
            `${fmt(x.failed)} ${T("would not open")} · ${bytes(x.disk)} / ${bytes(x.max_mb * 1048576)}`;
      document.getElementById("texts-clear").disabled = !x.on;
    }).catch(() => {});
  }
  document.getElementById("texts-clear").addEventListener("click", () => {
    fetch(`/api/texts?${new URLSearchParams({ forget: "1", t: TOKEN })}`, {
      method: "POST",
      cache: "no-store",
    }).then(loadTexts).catch((err) => console.warn("texts:", err));
  });
  function drawIndexReset() {
    document.getElementById("index-reset-desc").textContent = INDEX_SURE
      ? T("Searches will find little until the scan is done, a minute or a few on most machines.")
      : T("Empties the index and scans every disk again, from nothing.");
    const box = document.getElementById("index-reset-btns");
    box.innerHTML = INDEX_SURE
      ? `<button type="button" class="set-push" data-index="cancel">${esc(T("Cancel"))}</button> ` +
        `<button type="button" class="set-push asking" data-index="reset">${esc(T("Reset now"))}</button>`
      : `<button type="button" class="set-push" data-index="sure">${esc(T("Reset…"))}</button>`;
  }
  function indexSaid(text) { document.getElementById("index-said").textContent = text; }
  document.getElementById("index-open").addEventListener("click", () => {
    SERVICE.indexDo("open").catch((err) => console.warn("index:", err));
  });
  document.getElementById("index-tidy").addEventListener("click", () => {
    SERVICE.indexDo("reorganize")
      .then(() => indexSaid(T("Reorganizing. Searching goes on meanwhile.")))
      .catch((err) => console.warn("index:", err));
  });
  document.getElementById("index-reset-btns").addEventListener("click", (e) => {
    const b = e.target.closest("button[data-index]");
    if (!b) return;
    if (b.dataset.index === "reset") {
      SERVICE.indexDo("reset")
        .then(() => indexSaid(T("The index was emptied and every disk is being scanned again.")))
        .catch((err) => console.warn("index:", err));
    }
    INDEX_SURE = b.dataset.index === "sure";
    drawIndexReset();
  });

  function showSection(sec) {
    /* A section that moved is still asked for by its old name. */
    if (sec === "folders") sec = "disks";
    if (!SECTIONS[sec]) sec = "general";
    SECTION = sec;
    for (const b of settingsBox.querySelectorAll(".set-nav button[data-sec]")) {
      b.classList.toggle("on", b.dataset.sec === sec);
    }
    for (const el of settingsBox.querySelectorAll("section[data-sec]")) el.hidden = el.dataset.sec !== sec;
    document.getElementById("set-title").textContent = T(SECTIONS[sec][0]);
    document.getElementById("set-lead").textContent = T(SECTIONS[sec][1]);
    facesPanel.hidden = sec !== "general";
    rulesPanel.hidden = sec !== "rules";
    disksPanel.hidden = sec !== "disks";
    helpBox.hidden = sec !== "keys";
    if (sec === "general") { ARMED = ""; drawFaces(); }
    if (sec === "keys") loadHotkey();
    if (sec === "rules") loadRules();
    if (sec === "disks") { drawDisks(); loadDisks(); }
    if (sec === "index") { INDEX_SURE = false; loadIndex(); }
    if (sec === "about") {
      SERVICE.get("status", {}).then((st) => {
        document.getElementById("set-about").textContent =
          `${fmt(st.entries)} ${T("entries")}\n` +
          `${T("index")} ${bytes(st.index_bytes)}  ·  ${st.sources} ${T("sources")}  ·  ` +
          `${st.watching} ${T("watching")}`;
      }).catch(() => {});
    }
    drawSettings();
  }

  function openSettings(sec) {
    settingsBox.hidden = false;
    showSection(sec || SECTION);
  }
  function closeSettings() {
    settingsBox.hidden = true;
    facesPanel.hidden = rulesPanel.hidden = disksPanel.hidden = true;
    q.focus();
  }

  settingsBtn.addEventListener("click", () => (settingsBox.hidden ? openSettings() : closeSettings()));
  settingsBox.addEventListener("click", (e) => {
    if (e.target === settingsBox) { closeSettings(); return; }
    if (e.target.closest(".set-x")) { closeSettings(); return; }
    const nav = e.target.closest(".set-nav button[data-sec]");
    if (nav) { showSection(nav.dataset.sec); return; }
    const drop = e.target.closest("button[data-drop]");
    if (drop && KEPT) {
      KEPT.folders = (KEPT.folders || []).filter((f) => f !== drop.dataset.drop);
      keep({ folders: KEPT.folders });
      FOLDER_LIST = FOLDER_LIST && FOLDER_LIST.filter((f) => f.path !== drop.dataset.drop);
      drawSettings();
      return;
    }
    /* A folder switched on or off, watched or only walked: the list goes whole,
       and the service is asked again once the change has been written. */
    const fold = e.target.closest("button[data-fold]");
    if (fold && KEPT) {
      const key = fold.dataset.fold === "on" ? "folders_off" : "folders_unwatched";
      const path = fold.dataset.path;
      const list = (KEPT[key] || []).filter((f) => f !== path);
      if (fold.dataset.v === "off") list.push(path);
      KEPT[key] = list;
      keep({ [key]: list });
      if (FOLDER_LIST) {
        const f = FOLDER_LIST.find((x) => x.path === path);
        if (f) f[fold.dataset.fold === "on" ? "on" : "watched"] = fold.dataset.v === "on";
      }
      drawFolders();
      setTimeout(loadDisks, 600);
    }
  });
  document.getElementById("set-cols").addEventListener("click", (e) => {
    e.stopPropagation();
    const pick = document.getElementById("pick");
    if (pick) pick.click();
  });
  /* A folder is a path from the root, or from `~`. Whether it is there is the
     service's to find out: nothing here can look at the disk. */
  document.getElementById("folder-form").addEventListener("submit", (e) => {
    e.preventDefault();
    const box = e.target.querySelector("input");
    const err = document.getElementById("folder-err");
    let v = box.value.trim();
    if (!v) { err.textContent = T("Type a folder's path first."); return; }
    if (v === "~" || v.startsWith("~/")) v = HOME + v.slice(1);
    /* Windows writes `C:\x` and `\\server\share`; the index writes forward. */
    const win = /^[A-Za-z]:[\\/]/.test(v) || /^(\\\\|\/\/)/.test(v);
    if (win) v = v.replace(/\\/g, "/");
    if (!v.startsWith("/") && !win) { err.textContent = T("A folder's full path, from the root."); return; }
    v = v.replace(/\/+$/, "") || "/";
    const have = (KEPT && KEPT.folders) || [];
    if (have.includes(v)) { err.textContent = T("That folder is already in the list."); return; }
    KEPT.folders = [...have, v];
    keep({ folders: KEPT.folders });
    box.value = "";
    err.textContent = "";
    FOLDER_LIST = null;
    drawSettings();
    setTimeout(loadDisks, 600);
  });
  /* Escape closes the page before anything under it; Ctrl+, opens it. */
  document.addEventListener("keydown", (e) => {
    if (e.key === "," && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      if (settingsBox.hidden) openSettings(); else closeSettings();
    } else if (e.key === "Escape" && !settingsBox.hidden) {
      e.stopImmediatePropagation();
      closeSettings();
    }
  }, true);
  /* The old ways in still lead here. */
  for (const [id, sec] of [["faces", "general"], ["lang", "general"], ["rules", "rules"], ["disks", "disks"], ["about", "keys"]]) {
    const b = document.getElementById(id);
    if (b) b.addEventListener("click", (e) => { e.stopImmediatePropagation(); openSettings(sec); }, true);
  }

  /* What the report may reach: it is a separate closure, so anything
   * unpublished throws into a `catch` that reads as the service being down. */
  window.SCOUR = {
    service: SERVICE,
    escape: escape_,
    home: () => HOME,
    /* Handed over, not fetched again: two closures would arrive at different
     * moments. Functions, since the objects are replaced on a switch. */
    t: (msgid, vars) => T(msgid, vars),
    fmt: (n) => fmt(n),
    /* The report's redraw, called from `applyLanguage`. Null until it runs. */
    relabel: null,
    /* One menu for the whole page: the report's duplicate rows open the very
       list the search rows do, and hand over what to redraw afterwards. */
    rowMenu: (event, path, row, after) => openRowMenu(event, path, row, after),
    /* The tab strip says which view a tab is on, so switching views redraws it. */
    drawTabs: () => drawTabs(),
    /* The reader's own spelling of a moment, from the one built formatter. */
    stamp: (at) => exact(at),
    // Both arrive from the service after this runs, so both are getters.
    kinds: () => KINDS,
    barEdges: () => BAR_EDGES,
    /* The report cannot reach `keep`, so it hands over a change. */
    prefs: () => ({ dupes_open: DUPES_OPEN }),
    pref: (patch) => {
      if ("dupes_open" in patch) DUPES_OPEN = !!patch.dupes_open;
      keep(patch);
    },
  };
})();

