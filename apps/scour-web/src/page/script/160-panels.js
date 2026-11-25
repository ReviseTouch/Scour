  /* Three sources reach the walk and only one can be changed here. No counts:
   * what a rule excludes is not in the index, and walking the disk for it took
   * nine minutes for `target`. Writing never touches `config.toml`. */
  /* A rule is a name a person typed and it goes into markup. */
  const esc = (v) =>
    String(v).replace(/[&<>"]/g, (c) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);

  const rulesPanel = document.getElementById("rules-panel");
  const rulesBtn = document.getElementById("rules");
  let RULES = null;

  /* A rule carries its list: `target` the name and `/home/x/target` the
     prefix are two rules, keyed `kind:value`. */
  /* `RULE_KINDS`, not `KINDS`: one script, one scope. */
  const RULE_KINDS = [["path", "paths"], ["dir", "dirs"], ["file", "files"], ["allow", "allow"]];
  const rulesOf = (g) =>
    RULE_KINDS.flatMap(([kind, key]) => (g[key] || []).map((value) => ({ kind, value })));
  const ruleId = (kind, value) => kind + ":" + value;
  /* Case-insensitively, as the service's own merge is. */
  const isOff = (id) =>
    (RULES.off || []).some((o) => o.toLowerCase() === id.toLowerCase());

  const drawRules = () => {
    if (!RULES) return;
    const group = (title, note, entries, removable) => {
      if (!entries.length && !removable) return "";
      const rows = entries.map(({ kind, value }) => {
        const id = ruleId(kind, value);
        const off = isOff(id);
        /* The checkbox is the label, so the whole row is the target. */
        return `<li class="${off ? "off" : ""}">` +
          `<label><input type="checkbox" class="onoff" data-id="${esc(id)}"` +
          ` ${off ? "" : "checked"} aria-label="${esc(T("skip this"))}" />` +
          `<code>${esc(value)}</code></label>` +
          (removable ? `<button type="button" class="drop" data-kind="${kind}"` +
                       ` data-value="${esc(value)}" title="${esc(T("remove"))}">×</button>` : "") +
          `</li>`;
      }).join("");
      return `<section><h3>${esc(title)}</h3><p class="why">${esc(note)}</p>` +
             `<ul class="rules">${rows}</ul>` +
             (removable ? `<form class="add">` +
               `<input type="text" placeholder="${esc(T("a directory name, or a path"))}" />` +
               `<button type="submit">${esc(T("add"))}</button></form>` : "") +
             `</section>`;
    };
    rulesPanel.querySelector("#rules-body").innerHTML =
      group(T("Added here"), T("The only ones this window may delete. Any rule can be switched off."),
            rulesOf(RULES.added), true) +
      group(T("From config.toml"), T("Written by hand, and left alone — deleting it here would rewrite the file. Switch it off instead."),
            rulesOf(RULES.config), false) +
      group(T("Built in"), T("Part of the program. Mostly build output and package caches, which churn constantly and bury real results."),
            rulesOf(RULES.builtin), false);
  };

  const loadRules = () =>
    SERVICE.get("rules", {}).then((r) => { RULES = r; drawRules(); }).catch(() => {});

  /* A press arms a row; the button beside the sentence starts anything. */
  const facesPanel = document.getElementById("faces-panel");
  const facesBtn = document.getElementById("faces");
  let ARMED = "";

  const drawFaces = () => {
    const row = (id, label, aside, note) => {
      const here = id === "browser";
      const armed = ARMED === id;
      return `<div class="face ${armed ? "armed" : ""}" data-face="${id}">` +
        `<button type="button" class="pick" ${here ? "disabled" : ""}>` +
        `<span class="dot">${here ? "●" : "○"}</span>` +
        `<span class="name">${esc(label)}</span>` +
        `<span class="aside">${esc(aside)}</span></button>` +
        (armed ? `<div class="say"><p>${esc(note)}</p></div>` : "") +
        `</div>`;
    };
    /* The choice is started from the foot of the settings list. */
    const foot = document.getElementById("set-restart");
    foot.hidden = !ARMED;
    if (ARMED) {
      const name = ARMED === "window" ? T("Window") : T("Terminal");
      document.getElementById("set-restart-what").textContent =
        T("Scour opens again as: {face}", { face: name });
      const go = document.getElementById("set-restart-go");
      go.disabled = false;
      go.textContent = T("Restart");
    }
    document.getElementById("faces-body").innerHTML =
      row("browser", T("Browser"), T("running now"), "") +
      row("window", T("Window"),
          "",
          T("Starts the Scour window on this machine. This page stays open.")) +
      row("tui", T("Terminal"),
          "",
          T("Opens a terminal and runs Scour in it. This page stays open."));
  };

  facesPanel.addEventListener("click", (e) => {
    const pick = e.target.closest(".pick");
    if (pick) {
      const id = pick.closest(".face").dataset.face;
      ARMED = ARMED === id ? "" : id;
      drawFaces();
      return;
    }
  });

  document.getElementById("set-restart-go").addEventListener("click", (e) => {
    const go = e.currentTarget;
    if (!ARMED) return;
    /* After a switch this tab's bridge has gone, so nothing here can start
       another face: say that rather than blame the face. */
    if (HANDED_OVER) {
      go.textContent = T("this tab is closed");
      return;
    }
    const id = ARMED;
    go.disabled = true;
    SERVICE.face(id === "window" ? "window" : "tui")
      .then((r) => {
        ARMED = "";
        drawFaces();
        facesPanel.hidden = true;
        facesBtn.setAttribute("aria-expanded", "false");
        /* Switching is moving, so the server goes; a tab cannot close
           itself, so this says so and stops asking. */
        if (r && r.closing) handedOver();
      })
      .catch((err) => {
        go.disabled = false;
        /* A fetch that never reached the bridge is a closed tab, not a
           missing program. */
        go.textContent = err instanceof TypeError ? T("this tab is closed") : T("not found");
        console.warn("face:", err);
      });
  });

  /* Nothing still asking: retrying would blame `scourd` for the bridge. */
  function handedOver() {
    HANDED_OVER = true;
    if (PEEK.on) setPeek(false, false);
    const note = document.createElement("div");
    note.className = "handover";
    note.textContent = T("Scour is open elsewhere. You can close this tab.");
    document.body.append(note);
  }

  /* The desktop's key that opens Scour. GNOME and KDE are written to from
   * here; anywhere else the row can only say what to bind by hand. */

  /* A mirror of `Key::parse`'s modifiers, so a mistake is caught before a
   * round trip. The server is the authority: when the two disagree its 400
   * is what the row shows, and these sentences are `KeyError`'s own — the
   * crate returns English, so a translated copy here would be a second
   * wording of the same refusal. */
  const HOTKEY_MODS = new Set([
    "super", "win", "meta", "cmd", "mod4",
    "ctrl", "control", "primary",
    "alt", "option", "mod1",
    "shift",
  ]);

  /* Why this is not a combination, or "" when it is one. */
  function hotkeyFault(text) {
    const s = String(text == null ? "" : text).trim();
    if (!s) return "no key given";
    let head, key;
    /* The last `+` separates the key, so `ctrl++` is Ctrl and the plus key. */
    if (s.endsWith("++")) { head = s.slice(0, -2); key = "+"; }
    else if (s.endsWith("+")) return "a key has to follow the modifiers";
    else {
      const at = s.lastIndexOf("+");
      head = at < 0 ? "" : s.slice(0, at);
      key = at < 0 ? s : s.slice(at + 1);
    }
    for (const m of head.split("+").filter(Boolean)) {
      const low = m.toLowerCase();
      if (!HOTKEY_MODS.has(low)) return "not a modifier: " + low;
    }
    return key.trim() ? "" : "modifiers alone are not a key";
  }

  /* What a press sends, or null when what was typed is not a key. */
  function hotkeyAsk(what, text) {
    if (what === "clear") return { clear: true };
    return hotkeyFault(text) ? null : { set: String(text).trim() };
  }

  /* The row from one answer: a key, no key, or a desktop nothing here can
     bind, which can only name the command. */
  function hotkeyMarkup(h) {
    if (!h) return "";
    if (!h.can_bind) {
      return `<p class="why">` +
        esc(T("This desktop cannot be bound from here. Bind a key of your choice to:")) +
        `</p><div class="hk-cmd"><code>${esc(h.command)}</code>` +
        `<button type="button" class="hk-copy">${esc(T("Copy"))}</button></div>` +
        `<p class="hk-said" role="status"></p>`;
    }
    const bound = h.key || "";
    return `<p class="hk-now">` +
      (bound ? `<code>${esc(bound)}</code>`
             : `<span class="hk-none">${esc(T("not bound"))}</span>`) +
      `</p><form class="add hk-set">` +
      `<input type="text" class="hk-key" value="${esc(bound)}"` +
      ` aria-label="${esc(T("Keyboard shortcut"))}"` +
      ` placeholder="${esc(T("for example {a} or {b}", { a: (h.examples || [])[0] || "super+f", b: (h.examples || [])[1] || "ctrl+alt+s" }))}" />` +
      `<button type="submit">${esc(bound ? T("Change") : T("Bind"))}</button>` +
      (bound ? `<button type="button" class="hk-clear">${esc(T("Remove"))}</button>` : "") +
      `</form><p class="hk-said" role="status"></p>`;
  }

  const hotkeyBody = document.getElementById("hotkey-body");
  let HOTKEY = null;

  /* `role="status"` on a line that is already there, so a screen reader hears
     the refusal without the field being rebuilt under the cursor. */
  const hotkeySaid = (text, bad) => {
    const p = hotkeyBody.querySelector(".hk-said");
    if (!p) return;
    p.textContent = text || "";
    p.classList.toggle("bad", !!bad && !!text);
  };

  const drawHotkey = (said, bad) => {
    hotkeyBody.innerHTML = hotkeyMarkup(HOTKEY);
    hotkeySaid(said, bad);
  };

  const loadHotkey = () =>
    SERVICE.hotkey()
      .then((h) => { HOTKEY = h; drawHotkey(h.error, true); })
      .catch(() => { HOTKEY = null; drawHotkey(""); });

  const sendHotkey = (change) =>
    SERVICE.setHotkey(change)
      .then((h) => {
        HOTKEY = h;
        /* KDE writes the file; `kglobalaccel` reads it at login. */
        if (h.applies === "after_login") drawHotkey(T("It takes effect after the next login."));
        else drawHotkey(h.error, true);
      })
      .catch((e) => hotkeySaid(T(e.message || String(e)), true));

  hotkeyBody.addEventListener("submit", (e) => {
    e.preventDefault();
    const box = hotkeyBody.querySelector(".hk-key");
    const typed = box ? box.value : "";
    const fault = hotkeyFault(typed);
    if (fault) { hotkeySaid(fault, true); return; }
    sendHotkey(hotkeyAsk("set", typed));
  });

  /* Nothing is said while somebody is still typing it. */
  hotkeyBody.addEventListener("input", (e) => {
    if (e.target.classList.contains("hk-key")) hotkeySaid("");
  });

  hotkeyBody.addEventListener("click", (e) => {
    if (e.target.closest(".hk-clear")) { sendHotkey(hotkeyAsk("clear")); return; }
    const copy = e.target.closest(".hk-copy");
    if (!copy || !HOTKEY) return;
    navigator.clipboard.writeText(HOTKEY.command).then(
      () => {
        copy.textContent = "✓";
        setTimeout(() => { copy.textContent = T("Copy"); }, 2000);
      },
      () => hotkeySaid(T("could not copy"), true));
  });

  facesBtn.addEventListener("click", () => {
    const open = facesPanel.hidden;
    ARMED = "";
    facesPanel.hidden = !open;
    facesBtn.setAttribute("aria-expanded", open ? "true" : "false");
    if (open) { drawFaces(); loadHotkey(); }
  });

  rulesBtn.addEventListener("click", () => {
    const open = rulesPanel.hidden;
    rulesPanel.hidden = !open;
    rulesBtn.setAttribute("aria-expanded", open ? "true" : "false");
    if (open) loadRules();
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && !rulesPanel.hidden) {
      rulesPanel.hidden = true;
      rulesBtn.setAttribute("aria-expanded", "false");
    }
  });

  /* A name goes to `exclude_dirs`; a leading slash makes it a path. */
  rulesPanel.addEventListener("submit", (e) => {
    e.preventDefault();
    const box = e.target.querySelector("input");
    const v = box.value.trim();
    if (!v || !RULES) return;
    const key = v.startsWith("/") ? "paths" : "dirs";
    const next = [...RULES.added[key], v];
    RULES.added[key] = next;
    SERVICE.saveSettings({ ["exclude_" + key]: next });
    box.value = "";
    drawRules();
  });
  rulesPanel.addEventListener("click", (e) => {
    const b = e.target.closest(".drop");
    if (!b || !RULES) return;
    const v = b.dataset.value;
    for (const key of ["paths", "dirs", "files"]) {
      if (!RULES.added[key].includes(v)) continue;
      RULES.added[key] = RULES.added[key].filter((x) => x !== v);
      SERVICE.saveSettings({ ["exclude_" + key]: RULES.added[key] });
    }
    drawRules();
  });
  /* Off is a note, not a deletion, which is what lets the two groups this
     window cannot edit still be turned off. */
  rulesPanel.addEventListener("change", (e) => {
    const box = e.target.closest(".onoff");
    if (!box || !RULES) return;
    const id = box.dataset.id;
    const rest = (RULES.off || []).filter((o) => o.toLowerCase() !== id.toLowerCase());
    RULES.off = box.checked ? rest : [...rest, id];
    SERVICE.saveSettings({ exclude_off: RULES.off });
    drawRules();
  });
  /* No rescan button: saving a rule is what asks for the walk. */

  /* The disks: a panel that switches them, and a bar that asks about one that
     came. Asked for again whenever the count waiting moves; the service
     announces that like any other change. */
  const disksPanel = document.getElementById("disks-panel");
  const disksBtn = document.getElementById("disks");
  const diskAsk = document.getElementById("disk-ask");
  let DISK_LIST = [];
  let DISKS_ASKING = -1;
  const INDEXED = new Set(["auto", "chosen", "configured"]);
  const diskName = (v) => v.label || v.roots[0] || v.id;
  const diskWord = (table, key) => T((DISKS[table] || {})[key] || key);

  /* How a disk or a folder is kept up to date, in one line: files, watched
     live or last walked, last change. The same words the window uses. */
  const keeping = (walked, live, changed, walking) => {
    if (walking) return T("Being walked…");
    const parts = [];
    if (walked) parts.push(T("{n} files", { n: fmt(walked.entries) }));
    if (live) parts.push(T("watched live"));
    else if (walked) parts.push(T("walked {when}", { when: exact(walked.at) }));
    if (changed) parts.push(T("last change {when}", { when: exact(changed) }));
    return parts.length ? parts.join(" · ") : T("Not walked yet");
  };
  const refreshBtn = (path, walking) =>
    `<button type="button" class="set-push" data-rescan="${esc(path)}"` +
    `${walking ? " disabled" : ""} title="${esc(T("Walk it again now"))}">↻</button>`;

  const drawDisks = () => {
    const rows = DISK_LIST.map(({ volume: v, state, seen }) => {
      const on = INDEXED.has(state);
      const s = seen || {};
      const size = v.size ? " · " + bytes(v.size) : "";
      return `<li class="${on ? "" : "off"}">` +
        `<label><input type="checkbox" class="onoff" data-id="${esc(v.id)}"` +
        ` ${on ? "checked" : ""} ${state === "configured" ? "disabled" : ""}` +
        ` aria-label="${esc(T("index this disk"))}" />` +
        `<span class="disk"><span class="name">${esc(diskName(v))}</span>` +
        `<span class="aside">${esc(diskWord("kinds", v.kind) + size + " · " +
                                   diskWord("states", state))}</span>` +
        `<code>${esc(v.roots.join("  "))}</code>` +
        (on ? `<span class="aside">${esc(keeping(s.walked, s.live, s.changed, s.walking))}</span>` : "") +
        `</span></label>` + (on && v.roots[0] ? refreshBtn(v.roots[0], s.walking) : "") + `</li>`;
    }).join("");
    document.getElementById("disks-body").innerHTML = rows
      ? `<ul class="rules disks">${rows}</ul>`
      : `<p class="why">${esc(T("No disks were found."))}</p>`;
  };

  const drawAsk = () => {
    const d = DISK_LIST.find((x) => x.state === "asking");
    diskAsk.hidden = !d;
    disksBtn.classList.toggle("asking", !!d);
    const badge = document.getElementById("set-badge");
    if (badge) badge.hidden = !d;
    const asking = DISK_LIST.filter((x) => x.state === "asking").length;
    const chip = document.getElementById("m-disk");
    chip.hidden = !asking;
    chip.textContent = T("{n} disk asking", { n: asking });
    settingsBtnAsking(!!d);
    if (!d) return;
    diskAsk.dataset.id = d.volume.id;
    const question = (DISKS.questions || {})[d.volume.kind] || "{name}";
    diskAsk.querySelector(".said").textContent = T(question, { name: diskName(d.volume) });
  };

  /* The folders as the service last described them: switches and last walk.
     `null` until it has, and then the settings' own list is drawn bare. */
  let FOLDER_LIST = null;

  /* Each folder: its path, how its last walk went, whether it is scanned and
     whether it is watched, and the way to take it away. */
  function drawFolders() {
    const k = KEPT || {};
    const off = new Set(k.folders_off || []);
    const unwatched = new Set(k.folders_unwatched || []);
    const rows = FOLDER_LIST || (k.folders || []).map((path) => ({
      path, on: !off.has(path), watched: !unwatched.has(path), held: false, walked: null,
    }));
    const status = (f) => {
      if (!f.on) return [T("Switched off: not in the index"), false];
      if (f.held) return [T("Inside a disk that is scanned already"), false];
      if (f.walked && f.walked.problem && !f.walking)
        return [T("Could not be read: {why}", { why: f.walked.problem }), true];
      return [keeping(f.walked, f.live, f.changed, f.walking), false];
    };
    const pair = (path, what, on, yes, no) =>
      `<span class="seg mini">` +
      `<button type="button" data-fold="${what}" data-path="${esc(path)}" data-v="on"${on ? ' aria-pressed="true"' : ""}>${esc(T(yes))}</button>` +
      `<button type="button" data-fold="${what}" data-path="${esc(path)}" data-v="off"${on ? "" : ' aria-pressed="true"'}>${esc(T(no))}</button></span>`;
    document.getElementById("folders").innerHTML = rows.map((f) => {
      const [said, bad] = status(f);
      return `<li><div class="fw"><code title="${esc(f.path)}">${esc(f.path)}</code>` +
        `<span class="fst${bad ? " bad" : ""}">${esc(said)}</span></div>` +
        pair(f.path, "on", f.on, "Scanned", "Off") +
        pair(f.path, "watch", f.watched, "Watched", "Walked only") +
        (f.on && !f.held ? refreshBtn(f.path, f.walking) : "") +
        `<button type="button" class="set-push" data-drop="${esc(f.path)}">×</button></li>`;
    }).join("");
  }

  const loadDisks = () =>
    SERVICE.volumes()
      .then((r) => {
        DISK_LIST = r.disks || [];
        FOLDER_LIST = r.folders || null;
        placeHeard(DISK_LIST, null);
        if (!settingsBox.hidden) drawFolders();
        drawAsk();
        if (!disksPanel.hidden) drawDisks();
        /* While something is being walked and it is on screen, asked again:
           the line beside it says when the walk ends. */
        const walking = DISK_LIST.some((d) => d.seen && d.seen.walking) ||
          (FOLDER_LIST || []).some((f) => f.walking);
        if (walking && (!settingsBox.hidden || !disksPanel.hidden)) setTimeout(loadDisks, 1500);
      })
      .catch(() => {});

  function disksMoved(asking) {
    if (asking === DISKS_ASKING) return;
    DISKS_ASKING = asking;
    loadDisks();
  }

  const answerDisk = (id, what) =>
    SERVICE.answerDisk(id, what).then(loadDisks).catch((e) => console.warn("disk:", e));

  diskAsk.addEventListener("click", (e) => {
    const b = e.target.closest("button[data-do]");
    if (!b || !diskAsk.dataset.id) return;
    diskAsk.hidden = true;
    answerDisk(diskAsk.dataset.id, b.dataset.do);
  });
  disksBtn.addEventListener("click", () => {
    const open = disksPanel.hidden;
    disksPanel.hidden = !open;
    disksBtn.setAttribute("aria-expanded", open ? "true" : "false");
    if (open) { drawDisks(); loadDisks(); }
  });
  /* A refresh beside a disk or a folder, wherever the list is drawn. Inside a
     label it would also flip the switch, hence the default prevented. */
  document.addEventListener("click", (e) => {
    const again = e.target.closest("button[data-rescan]");
    if (!again) return;
    e.preventDefault();
    e.stopPropagation();
    again.disabled = true;
    SERVICE.rescan(again.dataset.rescan).then(loadDisks).catch((err) => console.warn("rescan:", err));
  }, true);
  disksPanel.addEventListener("change", (e) => {
    const box = e.target.closest(".onoff");
    if (!box) return;
    answerDisk(box.dataset.id, box.checked ? "index" : "leave");
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && !disksPanel.hidden) {
      disksPanel.hidden = true;
      disksBtn.setAttribute("aria-expanded", "false");
    }
  });

  const langBtn = document.getElementById("lang");
  const langMenu = document.createElement("div");
  langMenu.className = "rowmenu langs";
  langMenu.hidden = true;
  document.body.appendChild(langMenu);

  /* From what the service ships, or it offers a language with no catalogue. */
  function buildLangMenu() {
    langMenu.innerHTML = LANGUAGES.map(({ tag, name }) =>
      `<button type="button" data-lang="${escape_(tag)}">` +
      `<span class="tick">${tag === LANG ? "✓" : ""}</span>${escape_(name)}</button>`
    ).join("");
  }

  langBtn.addEventListener("click", (e) => {
    e.stopPropagation();
    const open = langMenu.hidden;
    langMenu.hidden = !open;
    langBtn.setAttribute("aria-expanded", String(open));
    if (!open) return;
    const r = langBtn.getBoundingClientRect();
    const m = langMenu.getBoundingClientRect();
    langMenu.style.left = Math.min(r.left, innerWidth - m.width - 8) + "px";
    langMenu.style.top = r.bottom + 4 + "px";
  });
  langMenu.addEventListener("click", (e) => {
    const btn = e.target.closest("button[data-lang]");
    if (btn) chooseLanguage(btn.dataset.lang);
  });
  document.addEventListener("click", () => {
    langMenu.hidden = true;
    langBtn.setAttribute("aria-expanded", "false");
  });

  /* No `lang` on the first load: the service knows what was chosen. */
  loadLanguage("").catch(() => {
    /* Every msgid here is its own English, so a failed fetch still draws. */
    applyLanguage();
  });


