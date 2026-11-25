(function () {
  /* The English source text is the msgid, so an untranslated string degrades
   * to correct English. `{name}` is the only substitution, there is no plural
   * machinery, and the catalogue test greps for `T(`. */
  let LANG = "en";
  let LANGUAGES = [];
  let STRINGS = {};
  const T = (msgid, vars) => {
    let s = Object.hasOwn(STRINGS, msgid) ? STRINGS[msgid] : msgid;
    if (vars) {
      for (const k of Object.keys(vars)) s = s.split("{" + k + "}").join(vars[k]);
    }
    return s;
  };

  /* Rebuilt on a switch, not per call: `STAMP` runs once per row per paint. */
  const DATE_PARTS = {
    day: "numeric", month: "long", year: "numeric",
    hour: "2-digit", minute: "2-digit", second: "2-digit",
  };
  /* Two decimals always, so the meter keeps its width while somebody types. */
  const MS_PARTS = { minimumFractionDigits: 2, maximumFractionDigits: 2 };
  const MB_PARTS = { minimumFractionDigits: 1, maximumFractionDigits: 1 };
  let NUMBERS = new Intl.NumberFormat("en");
  let MS = new Intl.NumberFormat("en", MS_PARTS);
  let MB = new Intl.NumberFormat("en", MB_PARTS);
  let STAMP = new Intl.DateTimeFormat("en", DATE_PARTS);
  function buildFormatters() {
    NUMBERS = new Intl.NumberFormat(LANG);
    MS = new Intl.NumberFormat(LANG, MS_PARTS);
    MB = new Intl.NumberFormat(LANG, MB_PARTS);
    STAMP = new Intl.DateTimeFormat(LANG, DATE_PARTS);
  }

  /* The element holds nothing but the msgid, so this can run again. */
  function applyMarkup(root) {
    for (const el of root.querySelectorAll("[data-t]")) {
      el.textContent = T(el.dataset.t);
    }
    /* `innerHTML` only for catalogue strings — never user input. */
    for (const el of root.querySelectorAll("[data-t-html]")) {
      el.innerHTML = T(el.dataset.tHtml);
    }
    for (const el of root.querySelectorAll("[data-t-title]")) {
      el.title = T(el.dataset.tTitle);
    }
    for (const el of root.querySelectorAll("[data-t-aria]")) {
      el.setAttribute("aria-label", T(el.dataset.tAria));
    }
    for (const el of root.querySelectorAll("[data-t-ph]")) {
      el.placeholder = T(el.dataset.tPh);
    }
  }

  let TOTAL = 0;   /* from /api/status, once the page is up */
  let TOTAL_KNOWN = false;
  /* No denominator until status lands, or `1000+ / 0` claims an empty index. */
  const withIndexTotal = (shown) =>
    TOTAL_KNOWN ? shown + " / " + fmt(TOTAL) : shown;

  const DAY = 86400;
  /* Recomputed every draw: a window stays open for hours. */
  let now = Math.floor(Date.now() / 1000);
  /* The only thing here that knows there is a daemon. */
  const TOKEN = new URLSearchParams(location.search).get("t") || "";

  /* Injected from `scour-ui`, so every face draws one list; a test reads the
   * marker out of the served page. */
  const MENU = /* @MENU@ */ [];
  /* The menu's drawings by name, `scour-ui`'s files, in the text's colour. */
  const MENU_ICONS = /* @MENUICONS@ */ {};
  /* The same, for how a disk is spoken of: kind, state and question by wire name. */
  const DISKS = /* @DISKS@ */ {};

  const SERVICE = {
    async get(route, params, signal) {
      const q = new URLSearchParams({ ...params, t: TOKEN });
      const r = await fetch(`/api/${route}?${q}`, { cache: "no-store", signal });
      if (!r.ok) throw new Error(`${route}: ${r.status}`);
      const j = await r.json();
      if (j.error) throw new Error(j.error);
      return j;
    },
    /* `signal` is not decoration: six connections, a boundary every few frames. */
    search: (q, sort, desc, limit, offset, signal) =>
      SERVICE.get("search", { q, sort, desc: desc ? 1 : 0, limit, offset: offset || 0 }, signal),
    /* Every question about the matching set, from one walk of the index. */
    explain: (q, cursor, signal) =>
      SERVICE.get("explain", cursor === null ? { q } : { q, cursor }, signal),
    sidebar: (q, edges) =>
      SERVICE.get("facets", { q, by: "kind,age", edges: edges.join(",") }),
    /* What a folder weighs: one walk of the index, 96 ms over 658,360 files.
       Scope is what makes it quick — 376 ms against 57 ms under a folder. */
    usage: (path, top, q) => SERVICE.get("usage", { path, top: top || 24, q: q || "" }),
    /* `budget_mb: 0` reads nothing — 84 ms over thirty thousand candidates —
       and answers which of them might be copies. */
    dupes: (under, budgetMb, minMb, signal) =>
      SERVICE.get("duplicates",
        { under, budget_mb: budgetMb || 0, min_mb: minMb || 1, top: 25 }, signal),
    /* Kept by the service: `localStorage` dies with the process. */
    settings: () => SERVICE.get("settings", {}),
    saveSettings: (s) => fetch(
      `/api/settings?${new URLSearchParams({ set: JSON.stringify(s), t: TOKEN })}`,
      { method: "POST" },
    ).catch(() => {}),
    /* The bridge moves them with this user's permissions; the service is only
       told to look again. Paths travel as a JSON array in the body: a name
       may hold a newline, and a list split on one names other files. */
    trash: async (paths) => {
      const r = await fetch(`/api/trash?${new URLSearchParams({ t: TOKEN })}`, {
        method: "POST",
        cache: "no-store",
        body: JSON.stringify(paths),
      });
      if (!r.ok) throw new Error(`trash: ${r.status}`);
      const j = await r.json();
      if (j.error) throw new Error(j.error);
      return j;
    },
    /* For good: the page has asked first, and nothing brings these back. */
    erase: async (paths) => {
      const r = await fetch(`/api/delete?${new URLSearchParams({ t: TOKEN })}`, {
        method: "POST",
        cache: "no-store",
        body: JSON.stringify(paths),
      });
      if (!r.ok) throw new Error(`delete: ${r.status}`);
      const j = await r.json();
      if (j.error) throw new Error(j.error);
      return j;
    },
    /* Each one selected in its folder, in the file manager. */
    reveal: async (paths) => {
      const r = await fetch(`/api/reveal?${new URLSearchParams({ t: TOKEN })}`, {
        method: "POST",
        cache: "no-store",
        body: JSON.stringify(paths),
      });
      if (!r.ok) throw new Error(`reveal: ${r.status}`);
      return r.json();
    },
    /* Asked each time the menu opens: a drive comes and goes. */
    sendTargets: () => SERVICE.get("send-targets", {}),
    send: async (to, paths) => {
      const r = await fetch(`/api/send?${new URLSearchParams({ to, t: TOKEN })}`, {
        method: "POST",
        cache: "no-store",
        body: JSON.stringify(paths),
      });
      if (!r.ok) throw new Error(`send: ${r.status}`);
      return r.json();
    },
    /* The window's drag tray, which can do what a page cannot. */
    drag: async (paths) => {
      const r = await fetch(`/api/drag?${new URLSearchParams({ t: TOKEN })}`, {
        method: "POST",
        cache: "no-store",
        body: JSON.stringify(paths),
      });
      if (!r.ok) throw new Error(`drag: ${r.status}`);
      return r.json();
    },
    /* The bridge renames; the service is told to look at both ends. */
    rename: async (path, name) => {
      const r = await fetch(`/api/rename?${new URLSearchParams({ path, name, t: TOKEN })}`, {
        method: "POST",
        cache: "no-store",
      });
      if (!r.ok) throw new Error(`rename: ${r.status}`);
      const j = await r.json();
      if (j.error) throw new Error(j.error);
      return j;
    },
    /* The list stays available with `--no-launch`; starting does not. */
    openers: (path) => SERVICE.get("openers", { path }),
    openWith: async (path, id) => {
      const r = await fetch(`/api/open-with?${new URLSearchParams({ path, id, t: TOKEN })}`, {
        method: "POST",
        cache: "no-store",
      });
      if (!r.ok) throw new Error(`open-with: ${r.status}`);
      return r.json();
    },
    /* Ask the desktop to make the pictures it has not; the bytes come from
       `/api/icon`, and the paths go in the body against a 16 KB request line. */
    thumbs: async (paths) => {
      const r = await fetch(`/api/thumb?${new URLSearchParams({ t: TOKEN })}`, {
        method: "POST",
        cache: "no-store",
        body: JSON.stringify(paths),
      });
      if (!r.ok) throw new Error(`thumb: ${r.status}`);
      return r.json();
    },
    /* The name is one of two words the server knows, never a path. */
    face: async (which) => {
      const r = await fetch(
        `/api/face?${new URLSearchParams({ face: which, t: TOKEN })}`,
        { method: "POST" },
      );
      if (!r.ok) throw new Error(await r.text());
      return r.json();
    },
    /* The desktop's key that opens Scour. Reading is a `GET`; setting and
       clearing carry a body, which is what makes them a `POST`. */
    hotkey: () => SERVICE.get("hotkey", {}),
    setHotkey: async (change) => {
      const r = await fetch(`/api/hotkey?${new URLSearchParams({ t: TOKEN })}`, {
        method: "POST",
        cache: "no-store",
        body: JSON.stringify(change),
      });
      const j = await r.json().catch(() => ({}));
      /* A refused combination says why in the body; the status alone would
         lose the one sentence worth showing. */
      if (j.error) throw new Error(j.error);
      if (!r.ok) throw new Error(`hotkey: ${r.status}`);
      return j;
    },
    status: () => SERVICE.get("status", {}),
    volumes: () => SERVICE.get("volumes", {}),
    /* What is indexed, by root: which folders of a disk are sources. */
    sources: () => SERVICE.get("sources", {}),
    /* Walk a disk's or a folder's root again, now. */
    rescan: async (path) => {
      const r = await fetch(`/api/rescan?${new URLSearchParams({ path, t: TOKEN })}`, {
        method: "POST",
        cache: "no-store",
      });
      if (!r.ok) throw new Error(`rescan: ${r.status}`);
      return r.json();
    },
    /* The Index section's buttons: `reorganize`, `reset` or `open`. */
    indexDo: async (what) => {
      const r = await fetch(`/api/index-do?${new URLSearchParams({ do: what, t: TOKEN })}`, {
        method: "POST",
        cache: "no-store",
      });
      if (!r.ok) throw new Error(`index: ${r.status}`);
      return r.json();
    },
    /* `what` is `index`, `leave` or `later`. */
    answerDisk: async (id, what) => {
      const r = await fetch(`/api/volume?${new URLSearchParams({ id, do: what, t: TOKEN })}`, {
        method: "POST",
        cache: "no-store",
      });
      if (!r.ok) throw new Error(`volume: ${r.status}`);
      return r.json();
    },
    /* The one request meant to be slow: it answers when the index would. */
    wait: (rev, ms) => SERVICE.get("wait", { rev, ms }),
    /* What can be shown, asked before anything is fetched: the bridge looks
       at the bytes. */
    peek: (path, signal) => SERVICE.get("preview", { path, meta: 1 }, signal),
    /* A URL, not a fetch: a blob holds the whole file to show one frame. */
    peekUrl: (path) =>
      `/api/preview?${new URLSearchParams({ path, t: TOKEN })}`,
    grep: (params, signal) => SERVICE.get("grep", params, signal),
    tree: (path, signal) => SERVICE.get("tree", { path, limit: 120 }, signal),
    pickFolder: async (start) => {
      const r = await fetch(`/api/pick-folder?${new URLSearchParams({ start: start || "", t: TOKEN })}`, {
        method: "POST",
        cache: "no-store",
      });
      if (!r.ok) throw new Error(`pick-folder: ${r.status}`);
      return r.json();
    },
  };

