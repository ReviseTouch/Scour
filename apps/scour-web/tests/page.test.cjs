const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { test } = require('node:test');
const vm = require('node:vm');

// Execute the shipped functions, with a deterministic clock and transport.
// The page as served, joined from `src/page/` by the build script; `tests/smoke.rs`
// says where. `cargo test -p scour-web --test smoke` runs this.
assert.ok(process.env.SCOUR_PAGE, 'SCOUR_PAGE names the joined page');
const html = readFileSync(process.env.SCOUR_PAGE, 'utf8');
function section(from, until) {
  const start = html.indexOf(from);
  assert.ok(start >= 0, `missing ${from}`);
  const end = html.indexOf(until, start + from.length);
  assert.ok(end > start, `missing ${until}`);
  return html.slice(start, end);
}
function context() {
  let time = 10000, next = 1;
  const timers = new Map();
  const c = vm.createContext({
    console, AbortController, Promise, Date: { now: () => time }, COST: 10,
    setTimeout(fn, ms) { const id = next++; timers.set(id, { at: time + ms, fn }); return id; },
    clearTimeout(id) { timers.delete(id); },
  });
  c.advance = (ms) => {
    const end = time + ms;
    for (;;) {
      const due = [...timers].filter(([, t]) => t.at <= end).sort((a, b) => a[1].at - b[1].at)[0];
      if (!due) break;
      timers.delete(due[0]); time = due[1].at; due[1].fn();
    }
    time = end;
  };
  return c;
}
const turns = async () => { for (let i = 0; i < 8; i++) await Promise.resolve(); };

test('expensive refreshes never overlap and keep the final update', async () => {
  const c = context();
  vm.runInContext(section('  function atMostEvery(fn, ms) {', '\n  function armFresh() {'), c);
  let calls = 0, release;
  const refresh = c.atMostEvery(() => { calls++; return new Promise(r => { release = r; }); }, 100);
  refresh();
  for (let i = 0; i < 10; i++) { c.advance(100); refresh(); }
  assert.equal(calls, 1, 'an unfinished refresh must not start more work');
  release(); await turns();
  c.advance(8999); assert.equal(calls, 1);
  c.advance(1); assert.equal(calls, 2, 'the trailing update must survive');
  release(); await turns(); c.advance(10000);
  assert.equal(calls, 2, 'no timer loop after the last change');
});

function listContext() {
  const c = context();
  Object.assign(c, {
    WINDOW: 200, KEEP_WINDOWS: 32, AHEAD_WINDOWS: 2, FRESH_MS: 1800,
    LIST: { rows: new Map(), seen: new Map(), at: new Map(), pending: new Map(), total: 20000, exact: true },
    arrivals: new Map(), fullPath: f => `${f.path}/${f.name}`, armFresh() {},
    reach: () => c.LIST.total, range: [0, 60], visibleRange: () => c.range,
  });
  vm.runInContext(section('  function putRows(start, rows, mark) {', '\n  function setTotal('), c);
  vm.runInContext(section('  function nextMissing(from, to) {', '\n  function fillWindow()'), c);
  return c;
}
function store(c, start) {
  const rows = Array.from({length: 200}, (_, i) => ({path:'/data', name:`file${start+i}`}));
  c.putRows(start, rows, false); c.keepWindow(start, 1);
}

test('a stationary viewport reads only nearby pages', () => {
  const c = listContext(); let requested = 0;
  for (let start; (start = c.nextMissing(0, 60)) !== null;) {
    store(c, start); assert.ok(++requested < 200, 'prefetch must terminate');
  }
  assert.equal(requested, 3);
  assert.equal(c.LIST.rows.size, 600);
});

test('scrolling bounds rows and path references and preserves visible pages', () => {
  const c = listContext();
  for (let start = 0; start < 20000; start += 200) { c.range = [start, start+60]; store(c, start); }
  assert.equal(c.LIST.at.size, 32);
  assert.equal(c.LIST.rows.size, 6400);
  assert.equal(c.LIST.seen.size, 6400);
  assert.ok(c.LIST.rows.has(19800));
  assert.ok(!c.LIST.rows.has(0));
  // The cache is sparse: its size says nothing about its largest row index.
  c.LIST.total = 10000; c.dropBeyond();
  assert.ok([...c.LIST.rows.keys()].every(i => i < 10000));
  assert.ok([...c.LIST.at.keys()].every(i => i < 10000));
  assert.equal(c.LIST.seen.size, c.LIST.rows.size);
});

test('hidden pages start no window requests', async () => {
  const c = listContext();
  c.document = { hidden: true }; c.HANDED_OVER = false;
  c.LIST.query = ''; c.SERVICE = { search() { assert.fail('hidden search'); } };
  vm.runInContext(section('  function fillWindow() {', '\n  let paintQueued = false;'), c);
  await c.fillWindow();
});

function transportContext() {
  const c = listContext(); const sent = [];
  Object.assign(c, {
    document: { hidden: false }, HANDED_OVER: false, generation: 1, SHOWING: 1,
    INFLIGHT: 4, SETTLED: 400, READ_AHEAD_MAX: 25, now: 0,
    fromService: row => row, repaint() {}, setTotal(total, exact) { c.LIST.total = total; c.LIST.exact = exact; },
    SERVICE: { search(query, sort, desc, limit, offset, signal) {
      return new Promise(resolve => sent.push({ query, offset, signal, resolve }));
    } },
  });
  Object.assign(c.LIST, { query: 'first', sort: 'name', desc: false, cost: 100, since: 0 });
  vm.runInContext(section('  function fillWindow() {', '\n  let paintQueued = false;'), c);
  const answer = (name) => ({ rows: [{ path: '/data', name }], capped: true, total: 20000, took_us: 100000 });
  return { c, sent, answer };
}

test('an old query cannot replace rows or clear the new pending request', async () => {
  const { c, sent, answer } = transportContext();
  const old = c.fillWindow();
  c.generation++; c.LIST.query = 'second'; c.LIST.pending.clear(); c.clearRows();
  const latest = c.fillWindow();
  assert.equal(sent.length, 2);
  sent[0].resolve(answer('old')); await old;
  assert.equal(c.LIST.rows.size, 0);
  assert.equal(c.LIST.pending.size, 1);
  sent[1].resolve(answer('new')); await latest;
  assert.equal(c.LIST.rows.get(0).name, 'new');
});

test('a fast scroll abandons old work and fills the final viewport', async () => {
  const { c, sent, answer } = transportContext();
  const old = c.fillWindow(); c.range = [2000, 2060];
  const latest = c.fillWindow();
  assert.ok(sent[0].signal.aborted);
  assert.equal(sent[1].offset, 2000);
  sent[1].resolve(answer('visible')); await latest;
  sent[0].resolve(answer('offscreen')); await old;
  assert.equal(c.LIST.rows.get(2000).name, 'visible');
  assert.ok(!c.LIST.rows.has(0));
  assert.equal(c.LIST.pending.size, 0);
});

// The duplicates workflow: the numbers a group shows and the one press that
// moves files. The page's own arithmetic, with a stub DOM under it.
function dupeContext() {
  const c = context();
  const nodes = new Map();
  Object.assign(c, {
    nodes,
    document: {
      getElementById(id) {
        if (!nodes.has(id)) nodes.set(id, { id, hidden: true, textContent: '', innerHTML: '' });
        return nodes.get(id);
      },
    },
    // The catalogue's own rule: absent means the msgid is the answer.
    T(msgid, vars) {
      let s = msgid;
      if (vars) for (const k of Object.keys(vars)) s = s.split('{' + k + '}').join(vars[k]);
      return s;
    },
    fmt: (n) => String(n),
    bytes: (n) => n + ' B',
    escape_: (s) => String(s),
    shorten: (p) => p,
    stamp: (at) => 'at ' + at,
    filter: () => '',
    scope: '',
    say() {},
  });
  vm.runInContext(section('  let dupeRun = 0;', '\n  async function drawDupes('), c);
  vm.runInContext(section('  async function drawDupes(budgetMb) {', '\n  /* Draws what `drawDupes` fetched.'), c);
  vm.runInContext(section('  function paintDupes() {', '\n  /* What the last move did.'), c);
  vm.runInContext(section('  function batches(paths) {', '\n  /* Remembered by the service'), c);
  c.read = (js) => vm.runInContext(js, c);
  return c;
}
const group = (size, paths, certainty, mtimes) =>
  ({ size, paths, certainty, mtimes: mtimes || paths.map((_, i) => i + 1), waste: 0 });

test('groups are ordered by what deleting the copies gives back, largest first', async () => {
  const c = dupeContext();
  // `waste` is deliberately wrong here: the page shows `size × (n − 1)`, its own.
  const answer = {
    groups: [
      group(10, ['/a', '/b'], 'size'),                     //  10
      group(100, ['/big1', '/big2'], 'content'),           // 100
      group(20, ['/c', '/d', '/e', '/f'], 'size'),         //  60
    ],
    candidates: 9, waste: 1, proven: 0, read: 0, unconfirmed: 2,
  };
  answer.groups[0].waste = 99999;
  c.SERVICE = { dupes: () => Promise.resolve(answer) };
  await c.drawDupes(0);
  assert.deepEqual(c.read('dupeGroups.map((g) => g.paths[0])'), ['/big1', '/c', '/a']);
  assert.deepEqual(c.read('dupeGroups.map(reclaim)'), [100, 60, 10]);
});

test('the header adds up what the shown groups would free, and how many', async () => {
  const c = dupeContext();
  c.SERVICE = { dupes: () => Promise.resolve({
    groups: [group(10, ['/a', '/b'], 'size'), group(20, ['/c', '/d', '/e'], 'content')],
    candidates: 5, waste: 0, proven: 0, read: 0, unconfirmed: 1,
  }) };
  await c.drawDupes(0);
  const said = c.nodes.get('d-sum').textContent;
  // 10 × 1 + 20 × 2 = 50.
  assert.ok(said.startsWith('2 groups · 50 B could be freed'), said);
  assert.ok(said.includes('1 confirmed identical, the rest share a size only'), said);
  // And the warning is never dropped while a group is unread.
  assert.ok(c.nodes.get('d-note').innerHTML
    .includes('Nothing should be deleted before it is confirmed.'));
});

test('the header says plainly when nothing was read, and when everything was', async () => {
  const c = dupeContext();
  const one = (certainty) => ({
    groups: [group(10, ['/a', '/b'], certainty)],
    candidates: 2, waste: 0, proven: 0, read: 0,
    unconfirmed: certainty === 'content' ? 0 : 1,
  });
  c.SERVICE = { dupes: () => Promise.resolve(one('size')) };
  await c.drawDupes(0);
  assert.ok(c.nodes.get('d-sum').textContent.includes('candidates by size — nothing was read'));
  c.SERVICE = { dupes: () => Promise.resolve(one('content')) };
  await c.drawDupes(0);
  assert.ok(c.nodes.get('d-sum').textContent.includes('all confirmed identical by reading'));
});

test('the copy kept by default is the newest, and a mark moves it', () => {
  const c = dupeContext();
  const g = group(10, ['/old', '/newest', '/middle'], 'content', [100, 300, 200]);
  c.g = g;
  assert.equal(c.read('keptOf(g)'), '/newest');
  // A path the group no longer holds cannot keep the mark.
  c.read('dupeKept.set(keyOf(g), "/middle")');
  assert.equal(c.read('keptOf(g)'), '/middle');
  c.read('dupeKept.set(keyOf(g), "/gone")');
  assert.equal(c.read('keptOf(g)'), '/newest');
  // A time of 0 is "not stated" and never wins against one that is.
  c.g = group(10, ['/untimed', '/timed'], 'content', [0, 1]);
  assert.equal(c.read('keptOf(g)'), '/timed');
});

test('only a group read end to end offers to move the other copies', () => {
  const c = dupeContext();
  c.read('dupeGroups = []');
  c.groups = [group(10, ['/a', '/b'], 'size'), group(10, ['/c', '/d', '/e'], 'content')];
  c.read('dupeGroups = groups');
  c.paintDupes();
  const drawn = c.nodes.get('d-list').innerHTML.split('<div class="dupe"');
  const unconfirmed = drawn.find((d) => d.includes('/a'));
  const confirmed = drawn.find((d) => d.includes('/c'));
  assert.ok(unconfirmed.includes('data-sweep="1" disabled'), unconfirmed);
  assert.ok(unconfirmed.includes('title="Confirm by reading first"'));
  assert.ok(unconfirmed.includes('Move the other 1 to trash'));
  assert.ok(!confirmed.includes('disabled'), confirmed);
  assert.ok(confirmed.includes('Move the other 2 to trash'));
});

test('the sweep asks once, names the kept path, and stops at no', async () => {
  const c = dupeContext();
  const g = group(10, ['/old', '/newest', '/middle'], 'content', [100, 300, 200]);
  c.groups = [g];
  c.read('dupeGroups = groups');
  let asked = null;
  c.confirm = (text) => { asked = text; return false; };
  c.drawDupes = () => { assert.fail('a refused question must not redraw'); };
  c.SERVICE = { trash: () => assert.fail('a refused question must not move files') };
  c.g = g;
  await c.read('sweep(g)');
  assert.equal(asked, 'Move 2 files to trash? The one under /newest stays.');
  // And an unconfirmed group is refused even if the press gets this far.
  c.g = group(10, ['/a', '/b'], 'size');
  asked = null;
  await c.read('sweep(g)');
  assert.equal(asked, null);
});

test('a path that will not move is named, and the rest still go', async () => {
  const c = dupeContext();
  const g = group(10, ['/keep', '/x', '/y'], 'content', [300, 100, 200]);
  c.groups = [g];
  c.read('dupeGroups = groups');
  c.g = g;
  c.confirm = () => true;
  c.SERVICE = { trash: (paths) => Promise.resolve({
    gone: paths.length - 1, refused: [paths[paths.length - 1] + ': busy'],
  }) };
  let said = null, redrew = 0;
  c.say = (text) => { said = text; };
  c.drawDupes = () => { redrew++; };
  await c.read('sweep(g)');
  assert.equal(said, '1 moved to trash · 1 stayed: /y: busy');
  assert.equal(redrew, 1, 'the group is asked for again once the move is done');
});

test('paths are moved in batches that fit one request line', () => {
  const c = dupeContext();
  c.paths = Array.from({ length: 200 }, (_, i) => '/a/very/long/path/number-' + i);
  const out = c.read('batches(paths)');
  assert.ok(out.length > 1, 'two hundred long paths do not fit one request line');
  assert.equal(out.flat().length, 200, 'no path may be dropped on the way');
  for (const b of out) {
    const len = b.reduce((n, p) => n + encodeURIComponent(p).length + 3, 0);
    assert.ok(len <= 6000 || b.length === 1, 'a batch past the bound');
  }
});

test('the kept copy is on screen even when it sits past the first few', () => {
  const c = dupeContext();
  const paths = Array.from({ length: 9 }, (_, i) => '/p' + i);
  // The newest is the eighth, well past the six a group shows.
  c.groups = [group(10, paths, 'content', paths.map((_, i) => (i === 7 ? 900 : i)))];
  c.read('dupeGroups = groups');
  c.paintDupes();
  const drawn = c.nodes.get('d-list').innerHTML.replace(/\s+/g, ' ');
  assert.ok(drawn.includes('data-keep="/p7" aria-pressed="true"'), drawn);
  // Six rows still, and the count of the rest agrees with them.
  assert.equal((drawn.match(/class="mark"/g) || []).length, 6);
  assert.ok(drawn.includes('3 more'), drawn);
  // Each row keeps its own time, though the order is no longer the answer's.
  assert.ok(drawn.includes('at 900'), drawn);
});

test('a group shows every path on demand, and the way back', () => {
  const c = dupeContext();
  const paths = Array.from({ length: 9 }, (_, i) => '/p' + i);
  c.groups = [group(10, paths, 'content')];
  c.read('dupeGroups = groups');
  c.paintDupes();
  const short = c.nodes.get('d-list').innerHTML;
  assert.equal((short.match(/class="mark"/g) || []).length, 6);
  assert.ok(short.includes('3 more'), short);
  c.read('dupeAll.add(keyOf(dupeGroups[0]))');
  c.paintDupes();
  const long = c.nodes.get('d-list').innerHTML;
  assert.equal((long.match(/class="mark"/g) || []).length, 9);
  // The control stays, or an expanded group can never be shortened again.
  assert.ok(long.includes('show fewer'), long);
});

// The desktop's key: the three states of the row, the grammar it checks
// before a round trip, and what a press puts in the body.
function hotkeyContext() {
  const c = context();
  Object.assign(c, {
    T(msgid, vars) {
      let s = msgid;
      if (vars) for (const k of Object.keys(vars)) s = s.split('{' + k + '}').join(vars[k]);
      return s;
    },
    esc: (v) => String(v).replace(/[&<>"]/g, (ch) =>
      ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' })[ch]),
  });
  vm.runInContext(section('  const HOTKEY_MODS = new Set([', '\n  const hotkeyBody ='), c);
  return c;
}

test('the row says what is bound, what is not, and what cannot be', () => {
  const c = hotkeyContext();

  const bound = c.hotkeyMarkup({ desktop: 'gnome', can_bind: true, key: 'super+f', command: '/opt/scour-gui' });
  assert.ok(bound.includes('<code>super+f</code>'), bound);
  assert.ok(bound.includes('value="super+f"'), bound);
  assert.ok(bound.includes('>Change<'), bound);
  assert.ok(bound.includes('>Remove<'), bound);
  assert.ok(!bound.includes('>Bind<'), bound);

  const free = c.hotkeyMarkup({ desktop: 'kde', can_bind: true, key: null, command: 'scour-gui' });
  assert.ok(free.includes('not bound'), free);
  assert.ok(free.includes('>Bind<'), free);
  // Nothing to remove, so no button that would send a clear for nothing.
  assert.ok(!free.includes('hk-clear'), free);
  assert.ok(free.includes('for example super+f or ctrl+alt+s'), free);

  const cannot = c.hotkeyMarkup({ desktop: 'other', can_bind: false, key: null, command: 'scour-gui' });
  assert.ok(cannot.includes('This desktop cannot be bound from here.'), cannot);
  assert.ok(cannot.includes('<code>scour-gui</code>'), cannot);
  assert.ok(cannot.includes('>Copy<'), cannot);
  // No field: there is nothing this page could do with what was typed in it.
  assert.ok(!cannot.includes('hk-key'), cannot);

  // Every state keeps the line a refusal is written into.
  for (const drawn of [bound, free, cannot]) assert.ok(drawn.includes('class="hk-said"'), drawn);
  assert.equal(c.hotkeyMarkup(null), '');
});

test('a command with markup in it reaches the row as text', () => {
  const c = hotkeyContext();
  const drawn = c.hotkeyMarkup({ can_bind: false, command: 'run "<b>x</b>"' });
  assert.ok(drawn.includes('run &quot;&lt;b&gt;x&lt;/b&gt;&quot;'), drawn);
  assert.ok(!drawn.includes('<b>'), drawn);
});

test('the combination is checked against the same grammar the service parses', () => {
  const c = hotkeyContext();
  for (const good of ['super+f', 'Super+F', 'ctrl+alt+s', 'win+space', 'super+F2',
                      'Control+Option+Enter', 'ctrl+"', 'ctrl++', 'f', 'cmd+meta+x',
                      '  super+f  ']) {
    assert.equal(c.hotkeyFault(good), '', good);
  }
  assert.equal(c.hotkeyFault(''), 'no key given');
  assert.equal(c.hotkeyFault('   '), 'no key given');
  assert.equal(c.hotkeyFault(null), 'no key given');
  assert.equal(c.hotkeyFault('ctrl+'), 'a key has to follow the modifiers');
  assert.equal(c.hotkeyFault('ctrl+alt+'), 'a key has to follow the modifiers');
  assert.equal(c.hotkeyFault('hyper+f'), 'not a modifier: hyper');
  assert.equal(c.hotkeyFault('Hyper+f'), 'not a modifier: hyper');
  assert.equal(c.hotkeyFault('super+ctrl+wat+f'), 'not a modifier: wat');
});

test('a press sends the body the route reads, and sends nothing for a mistake', () => {
  const c = hotkeyContext();
  // The body as it goes over the wire: the object itself is another realm's.
  const sent = (what, text) => JSON.stringify(c.hotkeyAsk(what, text));
  assert.equal(sent('set', '  Super+F  '), '{"set":"Super+F"}');
  assert.equal(sent('clear'), '{"clear":true}');
  // Not a combination: nothing to send, so the row says so instead.
  assert.equal(c.hotkeyAsk('set', 'hyper+f'), null);
  assert.equal(c.hotkeyAsk('set', ''), null);
  // A clear is a clear whatever is in the field.
  assert.equal(sent('clear', 'hyper+f'), '{"clear":true}');
});

// The report's numbers — `scour-chart` mirrored in JavaScript — and the markup
// that reads from them. A stub DOM under it, as the duplicates have.
function chartContext() {
  const c = context();
  Object.assign(c, {
    T(msgid, vars) {
      let s = msgid;
      if (vars) for (const k of Object.keys(vars)) s = s.split('{' + k + '}').join(vars[k]);
      return s;
    },
    fmt: (n) => String(n),
    bytes: (n) => n + ' B',
    escape_: (s) => String(s),
    percent: (v, d) => v.toFixed(d === undefined ? 1 : d) + '%',
    leafOf: (p) => p.slice(p.lastIndexOf('/') + 1) || p,
    AGE: [
      { msgid: 'today', c: '--t0' }, { msgid: 'this week', c: '--t1' },
      { msgid: 'this month', c: '--t2' }, { msgid: 'six months', c: '--t3' },
      { msgid: 'this year', c: '--t4' }, { msgid: 'older', c: '--t5' },
    ],
  });
  vm.runInContext(section('  function shares(values, decimals) {', '\n  const LABEL_PX = 64;'), c);
  vm.runInContext(section('  const LABEL_PX = 64;', '\n  function breadcrumb() {'), c);
  return c;
}
// The live home: Projeler, eleven others, and what they leave over.
const HOME = [
  651571270524, 31643455926, 15738588363, 14110521835, 10863393896, 10852991366,
  9242344819, 8271160692, 7898028191, 7372347728, 5889237540, 4856318717, 20649608929,
];
const adds = (list) => Math.abs(list.reduce((a, b) => a + b, 0) - 100) < 1e-9;
// Across the sandbox boundary: a vm realm's arrays and objects are not the
// test realm's, and strict deep equality compares prototypes.
const plain = (x) => JSON.parse(JSON.stringify(x));

test('shares add up to a hundred whatever the rounding', () => {
  const c = chartContext();
  // Three equal thirds would be 33.3 each and sum to 99.9; the first takes the
  // missing tenth, being the largest by the tie rule.
  assert.deepEqual(plain(c.shares([1, 1, 1], 1)), [33.4, 33.3, 33.3]);
  assert.deepEqual(plain(c.shares([1, 1, 1], 0)), [34, 33, 33]);
  assert.deepEqual(plain(c.shares([0, 0, 0], 1)), [0, 0, 0]);
  assert.deepEqual(plain(c.shares([7], 2)), [100]);
  assert.deepEqual(plain(c.shares([], 1)), []);
  const s = plain(c.shares(HOME, 1));
  assert.ok(adds(s), String(s));
  // 81.55% of the home directory, and the largest remainder does not reach it.
  assert.equal(s[0], 81.5);
  assert.ok(adds(plain(c.shares(HOME, 0))));
});

test('a bar and a ring cut the same whole the same way', () => {
  const c = chartContext();
  const seg = c.segments([1, 3]);
  assert.equal(seg[0].start, 0);
  assert.equal(seg[0].width, 0.25);
  assert.equal(seg[1].start, 0.25);
  assert.ok(c.segments([0, 0]).every((s) => s.width === 0));
  // The ring the page draws: r=54, so the circle is this long.
  const round = 2 * Math.PI * 54;
  const d = c.dashes([1, 1, 2], round);
  assert.ok(Math.abs(d[0].length - round / 4) < 1e-9);
  assert.ok(d[0].offset === 0, 'the first dash starts at twelve o\'clock');
  assert.ok(Math.abs(d[1].offset + round / 4) < 1e-9, 'negative and cumulative');
  assert.ok(Math.abs(d[2].offset + round / 2) < 1e-9);
  assert.ok(Math.abs(d.reduce((a, x) => a + x.length, 0) - round) < 1e-9);
});

test('folding keeps the largest and sums the others into one', () => {
  const c = chartContext();
  const f = c.fold([['b', 5], ['a', 9], ['c', 1], ['d', 5]], (x) => x[1], 2);
  assert.deepEqual(plain(f.kept), [['a', 9], ['b', 5]], 'stable: b before d');
  assert.deepEqual(plain(f.rest), { count: 2, value: 6 });
  assert.equal(c.fold([['a', 1]], (x) => x[1], 3).rest, null);
});

test('a label is drawn only in a segment wide enough to hold it', () => {
  const c = chartContext();
  const word = () => 'word';
  const spans = (html) => (html.match(/<span>/g) || []).length;
  // Sixty-four pixels of a 640px bar is a tenth of it.
  assert.equal(spans(c.stripHtml([10, 90], () => '--t0', word, null, 640)), 2);
  assert.equal(spans(c.stripHtml([5, 95], () => '--t0', word, null, 640)), 1);
  // Before layout there are no pixels, so there are no words — but there is
  // still a bar, which is the half that matters.
  const blind = c.stripHtml([50, 50], () => '--t0', word, null, 0);
  assert.equal(spans(blind), 0);
  assert.equal((blind.match(/<i /g) || []).length, 2);
  // A sliver too thin to see costs no element at all.
  assert.equal((c.stripHtml([100000, 1], () => '--t0', null, null, 640).match(/<i /g) || []).length, 1);
});

test('the folder bar and the ring take one hue, darkest first, the rest its own', () => {
  const c = chartContext();
  const upto = [0, 1, 2, 3, 4, 5, 6, 7].map((i) => c.barColour(i, 8));
  assert.deepEqual(upto, ['--k0', '--k1', '--k2', '--k3', '--k4', '--k5', '--k5', '--k5']);
  assert.equal(c.barColour(8, 8), '--kx');
  // Nothing was folded away, so nothing wears the folded colour.
  assert.ok(![0, 1, 2].map((i) => c.barColour(i, -1)).includes('--kx'));
});

test('the age strip keeps the time spectrum in its own order', () => {
  const c = chartContext();
  const drawn = c.ageStrip([1, 1, 1, 1, 1, 1]);
  assert.deepEqual([...drawn.matchAll(/var\((--t[0-9])\)/g)].map((m) => m[1]),
    ['--t0', '--t1', '--t2', '--t3', '--t4', '--t5']);
  assert.ok(drawn.includes('title="today · 17%"'), drawn);
});

test('the row for what was not shown appears, unless a filter is on', () => {
  const c = chartContext();
  const kids = [
    { path: '/home/hasan/Projeler', bytes: 80, files: 8, age: [8, 0, 0, 0, 0, 0] },
    { path: '/home/hasan/.config', bytes: 10, files: 2, age: [0, 0, 10, 0, 0, 0] },
  ];
  const pcts = plain(c.shares([80, 10, 10], 1));
  const left = { count: 51, value: 10 };
  const with_ = c.folderRows(kids, left, pcts);
  assert.ok(with_.includes('>the other 51 folders and the files here<'), with_);
  // The bar behind a name is its share of the heaviest, not of the parent.
  assert.ok(with_.includes('data-path="/home/hasan/Projeler"'), with_);
  assert.ok(with_.includes('style="width:100.0%"'), with_);
  assert.ok(with_.includes('style="width:12.5%"'), with_);
  // A filtered report hands over no rest: the answer is about the matches.
  const without = c.folderRows(kids, null, pcts);
  assert.ok(!without.includes('the other'), without);
  assert.ok(without.includes('data-path="/home/hasan/.config"'), without);
  // Every folder already has a row of its own, so only the files here are left.
  assert.ok(c.folderRows(kids, { count: 0, value: 10 }, pcts)
    .includes('class="nm mute">the files here<'));
  // And nothing at all when nothing is left over.
  assert.ok(!c.folderRows(kids, { count: 0, value: 0 }, pcts).includes('the files here'));
});

test('the ring and its legend run in the same order, the folded rest last', () => {
  const c = chartContext();
  const rows = [
    { key: 'build', count: 75090, label: 'Build output' },
    { key: 'file', count: 49295, label: 'File' },
    { key: 'dir', count: 41307, label: 'Folder' },
    { key: 'data', count: 16128, label: 'Data' },
    { key: 'config', count: 5248, label: 'Config' },
    { key: 'doc', count: 4786, label: 'Document' },
    { key: '', count: 8146, label: 'the other 5 kinds' },
  ];
  const pcts = plain(c.shares(rows.map((r) => r.count), 1));
  assert.ok(adds(pcts), String(pcts));
  const ring = c.ringHtml(rows, 6, '11', 'KINDS');
  const legend = c.legendHtml(rows, 6, pcts);
  const scale = ['--k0', '--k1', '--k2', '--k3', '--k4', '--k5', '--kx'];
  assert.deepEqual([...ring.matchAll(/stroke="var\((--k[0-9x])\)"/g)].map((m) => m[1]), scale);
  assert.deepEqual([...legend.matchAll(/background:var\((--k[0-9x])\)/g)].map((m) => m[1]), scale);
  // The mock-up's ring exactly: the first dash at twelve o'clock, the rest
  // negative and cumulative behind it.
  assert.ok(ring.includes('rotate(-90 75 75)'), ring);
  assert.ok(ring.includes('r="54"') && ring.includes('stroke-width="22"'), ring);
  assert.ok(ring.includes('stroke-dashoffset="0.00"'), ring);
  assert.ok(ring.includes('>11</text>') && ring.includes('>KINDS</text>'), ring);
  // The folded row can be clicked in neither place: it names no one kind.
  assert.equal((ring.match(/data-kind=/g) || []).length, 6);
  assert.equal((legend.match(/data-kind=/g) || []).length, 6);
  assert.ok(legend.includes(' disabled>'), legend);
  assert.ok(legend.includes('>37.5%</span>'), legend);
});

test('the meter sheds pieces least useful first, and only as many as it must', () => {
  const c = context();
  vm.runInContext(section('  const METER_STEPS = [', '\n  function fitMeter() {'), c);
  const steps = vm.runInContext('METER_STEPS', c);
  const plan = vm.runInContext('meterPlan', c);
  assert.deepEqual([...steps], ['note', 'rows', 'stale-short', 'stale']);
  assert.deepEqual([...plan(steps, () => true)], [], 'nothing goes when it fits');
  assert.deepEqual([...plan(steps, (taken) => taken.length >= 2)], ['note', 'rows'], 'the index facts, then the rows read');
  assert.deepEqual([...plan(steps, (taken) => taken.length >= 3)], ['note', 'rows', 'stale-short'], 'the advisory is said short before it goes');
  assert.deepEqual([...plan(steps, () => false)], [...steps], 'and at worst all of it, never the count or the time');
});

test('a refresh keeps a visible selected row on screen when arrivals push it down', async () => {
  const c = listContext();
  Object.assign(c, {
    document: { hidden: false, activeElement: null },
    body: { contains: () => false },
    SEL: new Map([['/data/target.docx', { path: '/data', name: 'target.docx', size: 100 }]]),
    SEL_AT: new Map([['/data/target.docx', 5]]),
    focusPath: '/data/target.docx',
    SHOWING: 1, REV: 2, PER_ROW: 1, headPx: 33,
    scrollEl: { scrollTop: 0, clientHeight: 350 },
    measurePerRow() {}, pitch: () => 31, lineH: () => 31, grid: () => false,
    drawSelBar() {}, repaint() {},
  });
  c.LIST.query = '*.docx';
  c.LIST.rows.set(5, { path: '/data', name: 'target.docx', size: 100 });
  c.fillWindow = async () => {
    c.LIST.rows.clear();
    for (let i = 0; i < 40; i++) c.LIST.rows.set(i, { path: '/data', name: `new${i}.docx`, size: 10 });
    c.LIST.rows.set(40, { path: '/data', name: 'target.docx', size: 250 });
  };
  vm.runInContext(section('  function indexOfPath(p) {', '\n  function clearSel() {'), c);
  vm.runInContext(section('  function rowVisibleAt(idx) {', '\n  const SIDEBAR_LIVE_UNDER_MS'), c);
  await c.refreshRows();
  assert.ok(c.scrollEl.scrollTop > 0, 'scrolled down to keep the selected row visible');
  assert.equal(c.SEL.get('/data/target.docx').size, 250, 'refreshed the selected row metadata');
});

test('clicking the selection bar text scrolls an off-screen selected row into view and focuses it', () => {
  const c = listContext();
  let focused = false;
  Object.assign(c, {
    SEL: new Map([['/data/target.docx', { path: '/data', name: 'target.docx', size: 100 }]]),
    SEL_AT: new Map([['/data/target.docx', 4]]),
    focusPath: null, wantFocusPath: null, PER_ROW: 1, headPx: 33,
    scrollEl: { scrollTop: 3100, clientHeight: 350 },
    pool: [{ dataset: { path: '/data/target.docx' }, focus() { focused = true; } }],
    measurePerRow() {}, pitch: () => 31, lineH: () => 31, grid: () => false, repaint() {},
  });
  c.LIST.query = '*.docx';
  vm.runInContext(section('  function indexOfPath(p) {', '\n  function clearSel() {'), c);
  vm.runInContext(section('  function rowVisibleAt(idx) {', '\n  async function refreshRows() {'), c);
  vm.runInContext(section('  function focusSelected() {', '\n  selbar.addEventListener("click"'), c);
  c.focusSelected();
  assert.equal(c.scrollEl.scrollTop, 0, 'brought the off-screen selected row back into view');
  assert.equal(c.focusPath, '/data/target.docx');
  assert.equal(focused, true, 'focused the selected row');
});

// The lens: the same reading of a term the window's `scour_ui::lens` gives.
test('the lens names its fields, places and kinds as the window does', () => {
  const c = context();
  c.HOME = '/home/u';
  vm.runInContext(section('  const LENS_FIELDS = {', '\n  const LENS_LAUNCH'), c);
  vm.runInContext(section('  function lensKindOf(t) {', '\n  const quoteIf = '), c);
  vm.runInContext(section('  const quoteIf = ', '\n  function lensClose()'), c);
  const run = (src) => vm.runInContext(src, c);
  assert.equal(run('JSON.stringify(placeParts("~/Bel"))'), '["~","Bel"]');
  assert.equal(run('JSON.stringify(placeParts("/"))'), '["/",""]');
  assert.equal(run('JSON.stringify(placeParts("~/Belgeler/"))'), '["~/Belgeler",""]');
  assert.equal(run('JSON.stringify(placeParts(""))'), '["",""]');
  assert.equal(run('homeForm("/home/u/Belgeler")'), '~/Belgeler');
  assert.equal(run('homeForm("/home/uu")'), '/home/uu');
  assert.equal(run('fullForm("~/x")'), '/home/u/x');
  assert.equal(run('quoteIf("My Files")'), '"My Files"');
  const kind = (t) => run(`lensKindOf(${JSON.stringify(t)})`);
  assert.equal(kind({ raw: ':', named: false, field: null, value: ':' }), 'launch');
  assert.equal(kind({ raw: ':de', named: false, field: null, value: ':de' }), 'launch');
  assert.equal(kind({ raw: 'in:~/x', named: true, field: run('LENS_FIELDS.in'), value: '~/x' }), 'place');
  assert.equal(kind({ raw: 'dm:', named: true, field: 'dm', value: '' }), 'time');
  assert.equal(kind({ raw: 'perm:x', named: true, field: null, value: 'x' }), null);
  assert.equal(kind({ raw: 'rapor', named: false, field: null, value: 'rapor' }), 'word');
  assert.equal(kind({ raw: 'a', named: false, field: null, value: 'a' }), null);
  assert.equal(run('LENS_FIELDS["t\\u00fcr"]'), 'kind');
});

// The launcher: the same rows, shortcuts and held filters `scour_ui::lens::launch` gives.
test('the launcher groups its rows, finds their shortcuts and goes into a held filter', () => {
  const c = context();
  const TR = { Location: 'Konum', Modified: 'Değişme', Type: 'Tür', Extension: 'Uzantı', Size: 'Boyut', 'Similar spelling': 'Benzer yazım', Inside: 'İçinde' };
  c.T = (s) => s;
  // Split as `scour_ui::lens::terms`: whitespace and `|` outside quotes.
  c.terms = (text) => {
    const out = []; let s = null, quoted = false;
    for (let i = 0; i < text.length; i++) {
      const ch = text[i], cut = !quoted && (/\s/.test(ch) || ch === '|');
      if (ch === '"') quoted = !quoted;
      if (cut) { if (s !== null) { out.push([s, i]); s = null; } } else if (s === null) s = i;
    }
    if (s !== null) out.push([s, text.length]);
    return out;
  };
  vm.runInContext(section('  const fold = ', '\n  const field = '), c);
  vm.runInContext(section('  function fieldSplit(t) {', '\n  /* Terms and the whitespace'), c);
  vm.runInContext(section('  const LENS_FIELDS = {', '\n  const LENS_TIMES'), c);
  vm.runInContext(section('  const LAUNCH_HEADINGS = ', '\n  function launchHtml('), c);
  const run = (src) => JSON.parse(JSON.stringify(vm.runInContext(src, c)));
  const rows = run('launchRows("", ":")');
  assert.deepEqual(rows.map((r) => r.field), ['under', 'dm', 'kind', 'ext', 'size', 'fuzzy', 'content']);
  assert.deepEqual(rows.map((r) => r.icon), ['folder', 'clock', 'shapes', 'ext', 'size', 'approx', 'read']);
  assert.deepEqual(rows.map((r) => r.heading), ['Narrow', '', '', '', '', 'How it searches', '']);
  assert.deepEqual(rows.map((r) => r.key), [':l', ':m', ':t', ':e', ':siz', ':f', ':i']);
  c.T = (s) => TR[s] || s;
  assert.deepEqual(run('launchRows("", ":")').map((r) => r.key), [':ko', ':d', ':t', ':e', ':s', ':f', ':i']);
  assert.deepEqual(run('launchRows("", ":iç")').map((r) => r.field), ['content']);
  const held = (q) => run(`launchRows(${JSON.stringify(q)}, ":")`).filter((r) => r.held).map((r) => r.field);
  assert.deepEqual(held('rapor :'), []);
  assert.deepEqual(held('rapor !ext:pdf :'), ['ext']);
  assert.deepEqual(held('fuzzy:rapro :'), []);
  assert.deepEqual(held('tür:doc : rapor'), ['kind']);
  assert.deepEqual(held('parent:/srv :'), ['under']);
  assert.deepEqual(held(':kind'), []);
  assert.deepEqual(run('intoHeld("rapor !ext:pdf :", 15, 16, "ext")'), { text: 'rapor !ext:pdf', caret: 14 });
  assert.deepEqual(run('intoHeld("rapor :ty tür:doc;code dm:7d", 6, 9, "kind")'), { text: 'rapor tür:doc;code dm:7d', caret: 18 });
  assert.equal(run('intoHeld("rapor :", 6, 7, "dm")'), null);
});

test('subsequence and fuzzy highlighting mark letters in order and 1-edit typos', () => {
  const c = context();
  vm.runInContext(section('  function bare(s) {', '\n  function completions('), c);
  vm.runInContext(section('  function subseqSpans(name, needle) {', '\n  /* Text as a name is searched'), c);
  const run = (src) => JSON.parse(JSON.stringify(vm.runInContext(src, c)));
  assert.deepEqual(run('subseqSpans("scour-gui", "scrgui")'), [[0, 2], [4, 5], [6, 9]]);
  // Every word everywhere, as `Unaccented::marks` in Rust: `hasan salih`
  // marks both in `HASAN SALİHOĞLU`, and the second `hasan` too.
  assert.deepEqual(run('nameSpans("HASAN SALİHOĞLU-hasan", ["hasan", "salih"])'), [[0, 5], [6, 5], [16, 5]]);
  assert.deepEqual(run('nameSpans("raporapor", ["rapor", "apora"])'), [[0, 9]]);
  assert.deepEqual(run('nameSpans("main.rs", ["zzz", ""])'), []);
  assert.deepEqual(run('fuzzySpan("rapor_2026.pdf", ["raporr"])'), [0, 5]);
});

test('sparkline range scrubbing produces valid size and time terms', () => {
  const c = context();
  c.LENS = { items: [{ term: 'size:<100kb' }, { term: 'size:100kb..10mb' }, { term: 'size:10mb..1gb' }, { term: 'size:>1gb' }] };
  vm.runInContext(section('  function sparkRangeTerm(', '\n  let sparkDrag = false;'), c);
  assert.equal(c.sparkRangeTerm('size', 1, 2, ''), 'size:100kb..1gb');
  assert.equal(c.sparkRangeTerm('size', 0, 1, ''), 'size:<10mb');
  assert.equal(c.sparkRangeTerm('size', 2, 3, '!'), '!size:>10mb');
  assert.equal(c.sparkRangeTerm('time', 1, 3, ''), 'dm:30d');
});

test('drawGrep renders sortable headers and aligned metadata columns without missing globals', () => {
  const c = vm.createContext({
    Intl, Date, Math, Set, HOME: '/home/u', DAY: 86400, now: 1700000000,
    NUMBERS: new Intl.NumberFormat('en'),
    STAMP: new Intl.DateTimeFormat('en'),
    MS: new Intl.NumberFormat('en', { minimumFractionDigits: 2, maximumFractionDigits: 2 }),
    T: (s, args) => args ? s.replace(/\{(\w+)\}/g, (_, k) => args[k]) : s,
    KIND_KEY: { doc: 'Document' },
    sortKey: 'size', sortDesc: true,
    grepView: { hidden: true, innerHTML: '' },
    scrollEl: { hidden: false },
    GREP: {
      next: 50,
      files: [{
        path: '/home/u/Belgeler/dava.docx',
        size: 20480,
        mtime: 1699990000,
        kind: 'doc',
        more: true,
        hits: [{ line: 5, pre: 'Antet ', hit: 'İstanbul', post: ' Hukuk' }],
      }],
    },
  });
  vm.runInContext(section('  const fmt = (n) =>', '\n  /* The deepest mount point'), c);
  vm.runInContext(section('  const escape_ = (s) =>', '\n  const FIELDS = ['), c);
  vm.runInContext(section('  const GLYPHS = new Set([', '\n  function writeMark('), c);
  vm.runInContext(section('  const homeForm = (p) =>', '\n  const fullForm = '), c);
  vm.runInContext(section('  function drawGrep(res) {', '\n  function applySortChoice('), c);
  c.drawGrep({ scanned: 100, read: 40960, took_us: 12500, binary: 2 });
  assert.equal(c.grepView.hidden, false);
  assert.equal(c.scrollEl.hidden, true);
  assert.match(c.grepView.innerHTML, /data-gsort="size"/);
  assert.match(c.grepView.innerHTML, /20 KB/);
  assert.match(c.grepView.innerHTML, /~\/Belgeler/);
  assert.match(c.grepView.innerHTML, /<mark>İstanbul<\/mark>/);
});

test('a filter says what it holds in words, and an empty one waits for its value', () => {
  const q = { value: '', selectionStart: 0, selectionEnd: 0, setSelectionRange() {} };
  const c = vm.createContext({
    Set, Map, Math, TextEncoder,
    document: { activeElement: null, getElementById: () => ({}) },
    q, shadow: { innerHTML: '' },
    LAST_SPANS: [], SYNTAX: new Set(['colon', 'cmp', 'quote', 'not', 'or', 'sep']),
    fitQuery() {}, markHover() {}, render() {}, lensClose() {}, lensSync() {},
    T: (id) => id, KIND_KEY: { doc: 'Document' }, KIND_VALUES: ['doc'],
    LENS_TIMES: [['7d', 'last 7 days']], LENS_SIZES: [],
  });
  vm.runInContext(section('  const escape_ = (s) =>', '\n  const acts ='), c);
  vm.runInContext(section('  const ENCODER =', '\n  /* The query is laid out by the browser'), c);

  // Not being written: `7d` is said as "last 7 days", the letters stay in the text.
  c.paint('dm:7d kind:doc');
  assert.match(c.shadow.innerHTML, /class="term cap said"[^>]*>.*<span class="art said-art">last 7 days<\/span>/s);
  assert.match(c.shadow.innerHTML, /<span class="art said-art">Document<\/span>/);
  assert.match(c.shadow.innerHTML, /<span class="val">7d<\/span>/);

  // Being written (the caret is in it): as typed, in its well.
  q.selectionStart = q.selectionEnd = 4;
  c.document.activeElement = q;
  c.paint('dm:7d kind:doc');
  assert.doesNotMatch(c.shadow.innerHTML.split('data-a="6"')[0], /said-art/);
  assert.match(c.shadow.innerHTML, /class="term cap edit"/);

  // Nothing after the colon: idle, and it says what it wants.
  q.selectionStart = q.selectionEnd = 0;
  c.paint('kind:doc dm:');
  assert.match(c.shadow.innerHTML, /class="term cap idle"[^>]*>.*<span class="val" data-hint="when\?"><\/span>/s);
});

test('capsule field headers render as cap-head and block caret or backspace inside ext:', () => {
  const q = {
    value: 'ext:docx',
    selectionStart: 2,
    selectionEnd: 2,
    selectionDirection: 'forward',
    setSelectionRange(a, b, d) { this.selectionStart = a; this.selectionEnd = b; if (d) this.selectionDirection = d; },
  };
  const c = vm.createContext({
    Set, Map, Math, TextEncoder,
    document: { activeElement: q, getElementById: () => ({}) },
    q, shadow: { innerHTML: '' },
    LAST_SPANS: [], SYNTAX: new Set(['colon', 'cmp', 'quote', 'not', 'or', 'sep']),
    fitQuery() {}, markHover() {}, render() {}, lensClose() {}, lensSync() {},
    T: (id) => id, KIND_KEY: {}, KIND_VALUES: [],
  });
  vm.runInContext(section('  const escape_ = (s) =>', '\n  const acts ='), c);
  vm.runInContext(section('  const ENCODER =', '\n  /* The query is laid out by the browser'), c);
  vm.runInContext(section('  let lastQueryCaret = 0,', '\n  /* Leave the term'), c);

  c.paint('ext:docx');
  // The header's letters stay in the text, behind its picture; the value is its own box.
  assert.match(c.shadow.innerHTML, /<span class="cap-head"><span class="art head-art">.*ext:<\/span><span class="val">docx<\/span>/s);

  // Clicking inside `ex|t:docx` snaps to `ext:|docx` (index 4).
  c.clampQuerySelection();
  assert.equal(q.selectionStart, 4);

  // Pressing ArrowLeft from `ext:|docx` (4 -> 3) jumps across `ext:` to index 0.
  q.setSelectionRange(3, 3);
  c.clampQuerySelection();
  assert.equal(q.selectionStart, 0);

  // Backspace at `ext:|docx` keeps `ext:docx` intact; Backspace at `ext:|` deletes the capsule.
  q.setSelectionRange(4, 4);
  assert.equal(c.frameBackspace(), true);
  assert.equal(q.value, 'ext:docx');
  q.value = 'ext:';
  q.setSelectionRange(4, 4);
  assert.equal(c.frameBackspace(), true);
  assert.equal(q.value, '');
});

// The page is one scope: a second `function x` silently replaces the first,
// and node parses it without a word.
test('no top-level function of the page is declared twice', () => {
  const seen = new Map();
  for (const m of html.matchAll(/^ {2}(?:async )?function (\w+)\(/gm)) {
    seen.set(m[1], (seen.get(m[1]) || 0) + 1);
  }
  const twice = [...seen].filter(([, n]) => n > 1).map(([name]) => name);
  assert.deepEqual(twice, []);
});

// The place lens's model: `scour_ui::lens` (the place group) and
// `scour_ui::disks` (tiles, strip) in the page's words. Each case below is a
// case of `lens/group_tests.rs` or `disks/tiles_tests.rs`, with the same
// inputs and the same answers, so the two cannot drift apart.
const J = (x) => JSON.parse(JSON.stringify(x));
const PLACE_HOME = '/home/hasan';
function placeModel() {
  const c = context();
  vm.runInContext(section('  const fold = ', '\n  const field = '), c);
  vm.runInContext(section('  function fieldSplit(t) {', '\n  /* Terms and the whitespace'), c);
  vm.runInContext(section('  const LENS_FIELDS = {', '\n  const LENS_TIMES'), c);
  vm.runInContext(section('  /* ---------------- the place model', '\n  /* ---------------- the '), c);
  vm.runInContext(section('  const tileRoots = ', '\n  /* Each tile: held'), c);
  vm.runInContext(section('  function placeAnchor(', '\n  /* A tile clicked'), c);
  vm.runInContext(section('  function placeRest(', '\n  /* Each disk'), c);
  vm.runInContext(section('  function scopeSays(', '\n  function drawScopeChip('), c);
  return vm.runInContext(`({ placeToggle, tileToggle, placeScope, placeRemove, placeRuns, placeRunOf,
    placeTerms, scopeHolds, diskTiles, diskStrip, diskOf, placeAnchor, placeRest, scopeSays, STRIP_FLOOR, DISK_KINDS })`, c);
}

test('the place group: a disk in, a second beside it, one out, none left (lens/group_tests.rs)', () => {
  const M = placeModel();
  const pt = (q, root, pick) => J(M.placeToggle(q, PLACE_HOME, root, pick));
  const also = (q, root) => pt(q, root, 'also')[0];
  const only = (q, root) => pt(q, root, 'only')[0];
  // the_first_disk_is_one_term
  assert.deepEqual(pt('rapor', '/mnt/depo', 'only'), ['rapor under:/mnt/depo', 21]);
  assert.equal(also('', '/mnt/depo'), 'under:/mnt/depo');
  assert.equal(also('rapor ', '/'), 'rapor under:/');
  // an_empty_place_term_is_where_the_disk_goes
  assert.deepEqual(pt('rapor under: ext:pdf', '/mnt/depo', 'also'), ['rapor under:/mnt/depo ext:pdf', 21]);
  assert.equal(only('rapor under: ext:pdf', '/mnt/depo'), 'rapor under:/mnt/depo ext:pdf');
  // a_second_disk_joins_the_group_without_spaces
  assert.deepEqual(pt('rapor under:/mnt/depo ext:pdf', '/', 'also'), ['rapor under:/mnt/depo|under:/ ext:pdf', 29]);
  assert.equal(also('rapor under:/a|under:/b', '/c'), 'rapor under:/a|under:/b|under:/c');
  // taking_one_out_leaves_no_bar_behind
  const q = 'rapor under:/a|under:/b ext:pdf';
  assert.deepEqual(pt(q, '/a', 'also'), ['rapor under:/b ext:pdf', 6]);
  assert.equal(also(q, '/b'), 'rapor under:/a ext:pdf');
  assert.equal(also('under:/a | under:/b rapor', '/b'), 'under:/a rapor');
  assert.equal(also('rapor under:/a|under:/b|under:/c', '/b'), 'rapor under:/a|under:/c');
  // the_last_one_out_takes_the_term_and_a_space
  assert.deepEqual(pt('rapor under:/a ext:pdf', '/a/', 'also'), ['rapor ext:pdf', 6]);
  assert.equal(only('rapor under:/a', '/a'), 'rapor');
  assert.equal(only('under:/a/|under:/a', '/a'), '');
  // only_leaves_one_disk
  const three = 'rapor under:/a|under:/b|under:/c ext:pdf';
  assert.deepEqual(pt(three, '/b', 'only'), ['rapor under:/b ext:pdf', 14]);
  assert.equal(only(three, '/d'), 'rapor under:/d ext:pdf');
  assert.equal(only('under:/a', '/d'), 'under:/d');
});

test('the place group quotes, writes home with a tilde and leaves the rest alone (lens/group_tests.rs)', () => {
  const M = placeModel();
  const also = (q, root) => M.placeToggle(q, PLACE_HOME, root, 'also')[0];
  const only = (q, root) => M.placeToggle(q, PLACE_HOME, root, 'only')[0];
  // a_path_with_a_space_or_a_bar_is_quoted
  let q = also('rapor', '/mnt/depo/My Docs');
  assert.equal(q, 'rapor under:"/mnt/depo/My Docs"');
  q = also(q, '/srv/a|b');
  assert.equal(q, 'rapor under:"/mnt/depo/My Docs"|under:"/srv/a|b"');
  assert.equal(also(q, '/mnt/depo/My Docs'), 'rapor under:"/srv/a|b"');
  // home_is_written_and_read_with_a_tilde
  assert.equal(also('rapor', '/home/hasan/Belgeler'), 'rapor under:~/Belgeler');
  assert.equal(also('rapor under:~/Belgeler', '/mnt/depo'), 'rapor under:~/Belgeler|under:/mnt/depo');
  assert.equal(also('rapor under:~/Belgeler/|under:/mnt/depo', '/home/hasan/Belgeler'), 'rapor under:/mnt/depo');
  // an_exclusion_elsewhere_is_left_alone
  assert.equal(also('rapor !under:/mnt/depo/tmp', '/mnt/depo'), 'rapor !under:/mnt/depo/tmp under:/mnt/depo');
  const x = '!under:/x rapor under:/a|under:/b';
  assert.equal(also(x, '/a'), '!under:/x rapor under:/b');
  assert.equal(only(x, '/b'), '!under:/x rapor under:/b');
  assert.equal(also('rapor !under:/a', '/a'), 'rapor !under:/a under:/a');
  // a_second_run_is_not_the_group
  const two = 'under:/a|under:/b rapor under:/c';
  assert.equal(also(two, '/c'), 'under:/a|under:/b|under:/c rapor under:/c');
  assert.equal(also(two, '/a'), 'under:/b rapor under:/c');
});

test('runs, a member taken out, and the scope read from the text (lens/group_tests.rs)', () => {
  const M = placeModel();
  // removing_a_member_keeps_the_rest_of_its_run
  const q = 'rapor|fatura ext:pdf';
  const ts = M.placeTerms(q);
  assert.deepEqual(J(M.placeRemove(q, ts[1])), ['rapor ext:pdf', 5]);
  assert.deepEqual(J(M.placeRemove(q, ts[0])), ['fatura ext:pdf', 0]);
  assert.deepEqual(J(M.placeRemove(q, ts[2])), ['rapor|fatura', 12]);
  // runs_join_on_a_bar_with_or_without_spaces
  const r = 'a|b c | d e';
  assert.deepEqual(J(M.placeRuns(r).map((x) => x.length)), [2, 2, 1]);
  assert.equal(M.placeRunOf(r, M.placeTerms(r)[3]).length, 2);
  // the_scope_is_read_from_the_text
  const s = 'rapor under:~/Belgeler/|under:"/mnt/depo/My Docs" !under:/mnt/depo/tmp under:';
  const sc = M.placeScope(s, PLACE_HOME);
  assert.deepEqual(J(sc.within.map((h) => h.path)), ['/home/hasan/Belgeler', '/mnt/depo/My Docs']);
  assert.deepEqual(J(sc.outside.map((h) => h.path)), ['/mnt/depo/tmp']);
  assert.ok(M.scopeHolds(sc, '/mnt/depo/My Docs/'));
  assert.ok(!M.scopeHolds(sc, '/mnt/depo'));
  const [out] = M.placeRemove(s, sc.outside[0].term);
  assert.deepEqual(J(M.placeScope(out, PLACE_HOME).outside), []);
  // a_negated_member_is_neither_inside_nor_outside
  const neg = M.placeScope('!under:/a|under:/b', PLACE_HOME);
  assert.equal(neg.within.length, 1);
  assert.equal(neg.within[0].path, '/b');
  assert.equal(neg.outside.length, 0);
  assert.deepEqual(J(M.placeScope('rapor', PLACE_HOME)), { within: [], outside: [] });
  assert.ok(M.scopeHolds(M.placeScope('under:/', PLACE_HOME), '/'));
});

test('a tile goes into and out of the place group whole (lens/group_tests.rs)', () => {
  const M = placeModel();
  const tile = (q, roots, pick) => M.tileToggle(q, PLACE_HOME, roots, pick)[0];
  // a_tile_of_several_roots_goes_in_and_out_as_one
  const sys = ['/home/hasan', '/usr', '/etc'];
  const q = tile('rapor', sys, 'only');
  assert.equal(q, 'rapor under:~|under:/usr|under:/etc');
  assert.equal(tile(q, sys, 'only'), 'rapor');
  assert.equal(tile(q, sys, 'also'), 'rapor');
  const both = tile(q, ['/mnt/depo'], 'also');
  assert.equal(both, 'rapor under:~|under:/usr|under:/etc|under:/mnt/depo');
  assert.equal(tile(both, ['/mnt/depo'], 'only'), 'rapor under:/mnt/depo');
  assert.equal(tile(both, sys, 'also'), 'rapor under:/mnt/depo');
  // a_tile_half_held_is_completed_not_dropped
  const half = 'rapor under:/usr ext:pdf';
  assert.equal(tile(half, ['/usr', '/etc'], 'also'), 'rapor under:/usr|under:/etc ext:pdf');
  assert.equal(tile(half, ['/usr', '/etc'], 'only'), 'rapor under:/usr|under:/etc ext:pdf');
  assert.equal(tile(half, [], 'only'), 'rapor ext:pdf');
});

/* `tiles_tests.rs`'s fixtures: a disk as `DiskIn` borrows it. */
const fixDisk = (id, root, kind, state) =>
  ({ id, label: '', kind, fs: 'ext4', roots: [root], size: 1 << 30, state, live: false, walking: false });
function fixMachine() {
  const cachy = { ...fixDisk('uuid:cachy', '/', 'system', 'manual'), label: 'CACHY', fs: 'btrfs' };
  const depo = { ...fixDisk('uuid:depo', '/mnt/depo', 'fixed', 'configured'), label: 'Depo', fs: 'ntfs3', live: true };
  const stick = fixDisk('uuid:AB12', '/run/media/hasan/STICK', 'removable', 'asking');
  const share = { ...fixDisk('net:nas', '/mnt/nas', 'network', 'chosen'), size: 0 };
  const sources = [
    { name: 'home', roots: ['/home/hasan'] }, { name: 'depo', roots: ['/mnt/depo'] },
    { name: 'sistem', roots: ['/usr', '/etc', '/opt', '/var'] }, { name: 'net:nas', roots: ['/mnt/nas'] },
  ];
  return [[cachy, depo, stick, share], sources];
}
const FIX_FILES = { 'uuid:cachy': 4803676, 'uuid:depo': 1755949, 'uuid:AB12': 5, 'net:nas': 1000 };
const fixTiles = (M, disks, sources) => J(M.diskTiles(disks, sources, (id) => FIX_FILES[id] ?? null));

test('a tile says what a disk is and what is indexed on it (disks/tiles_tests.rs)', () => {
  const M = placeModel();
  const [disks, sources] = fixMachine();
  // the_owners_machine
  const t = fixTiles(M, disks, sources);
  assert.deepEqual(t.map((x) => x.shown), ['partly', 'live', 'asking', 'polled']);
  assert.equal(t[0].name, 'CACHY');
  assert.equal(t[0].kind, 'system');
  assert.equal(t[0].root, '/');
  assert.deepEqual(t[0].roots, ['/etc', '/home/hasan', '/opt', '/usr', '/var']);
  assert.ok(!t[0].whole);
  assert.ok(t[1].whole);
  assert.deepEqual(t[1].roots, ['/mnt/depo']);
  assert.equal(t[2].name, '/run/media/hasan/STICK', 'no label, the root');
  // a_disk_is_counted_over_the_sources_on_it
  assert.equal(t[0].count, 'file: under:/etc|under:/home/hasan|under:/opt|under:/usr|under:/var');
  assert.equal(t[1].count, 'file: under:/mnt/depo');
  assert.equal(t[2].count, null, 'nothing on the stick is indexed');
  assert.equal(t[2].files, null, 'nor counted');
  assert.equal(t[0].files, 4803676);
  // only_a_disk_with_nothing_indexed_may_be_indexed
  assert.deepEqual(t.map((x) => x.index_it), [false, false, true, false], 'never the partly one');
  const manual = J(M.diskTiles([fixDisk('uuid:m', '/', 'system', 'manual')], [], () => null))[0];
  assert.equal(manual.shown, 'off');
  assert.ok(manual.index_it && manual.count === null);
  const later = J(M.diskTiles([fixDisk('uuid:l', '/run/media/u/L', 'removable', 'later')], [], () => null))[0];
  assert.equal(later.shown, 'asking');
  // walking_wins_over_live_and_partly
  const walking = disks.map((d, i) => (i < 2 ? { ...d, walking: true } : d));
  const w = fixTiles(M, walking, sources);
  assert.equal(w[0].shown, 'walking');
  assert.equal(w[1].shown, 'walking');
  assert.equal(w[0].roots.length, 5, 'its tags stay');
});

test('a tile is counted over its own roots, less the disks inside them (disks/tiles_tests.rs)', () => {
  const M = placeModel();
  // a_disk_holding_the_root_leaves_the_disks_inside_it_out
  const cachy = { ...fixDisk('uuid:cachy', '/', 'system', 'auto'), roots: ['/', '/home'] };
  const disks = [cachy, fixDisk('uuid:depo', '/mnt/depo/', 'fixed', 'auto')];
  const sources = [{ name: 'uuid:cachy', roots: ['/'] }, { name: 'uuid:depo', roots: ['/mnt/depo'] }];
  const t = fixTiles(M, disks, sources);
  assert.equal(t[0].count, 'file: under:/ !under:/mnt/depo', 'its own second root is not another disk');
  assert.ok(t[0].whole);
  assert.equal(t[1].root, '/mnt/depo');
  // an_indexed_disk_is_counted_before_its_source_is_known
  const x = J(M.diskTiles([fixDisk('uuid:x', '/mnt/x', 'fixed', 'chosen')], [], () => null))[0];
  assert.equal(x.count, 'file: under:/mnt/x');
  assert.equal(x.shown, 'polled');
  const y = J(M.diskTiles([fixDisk('uuid:y', '/run/media/u/MY STICK', 'removable', 'chosen')], [], () => null))[0];
  assert.equal(y.count, 'file: under:"/run/media/u/MY STICK"');
  // every_kind_is_known_and_a_strange_one_is_fixed
  for (const k of M.DISK_KINDS) {
    assert.equal(M.diskTiles([fixDisk('i', '/x', k, 'auto')], [], () => null)[0].kind, k);
  }
  assert.equal(M.diskTiles([fixDisk('i', '/x', 'tape', 'auto')], [], () => null)[0].kind, 'fixed');
  // a_path_is_on_the_disk_with_the_longest_root_above_it
  const [machine] = fixMachine();
  assert.equal(M.diskOf('/mnt/depo/x', machine), 'uuid:depo');
  assert.equal(M.diskOf('/mnt/depot', machine), 'uuid:cachy');
  assert.equal(M.diskOf('/mnt/depo', machine), 'uuid:depo');
  assert.equal(M.diskOf('relative', machine), null);
});

test('the strip shares by files with a floor, and evenly before a count (disks/tiles_tests.rs)', () => {
  const M = placeModel();
  const [disks, sources] = fixMachine();
  // the_strip_shares_by_files_with_a_floor
  const s = J(M.diskStrip(M.diskTiles(disks, sources, (id) => FIX_FILES[id] ?? null)));
  assert.deepEqual(s.map((x) => x.at), [0, 1, 3], 'the stick is not on the strip');
  const sum = s.reduce((n, x) => n + x.share, 0);
  assert.ok(Math.abs(sum - 1) < 1e-9, String(sum));
  assert.equal(s[2].share, M.STRIP_FLOOR, 'a thousand files still show');
  const cachy = 4803676 / (4803676 + 1755949) * (1 - M.STRIP_FLOOR);
  assert.ok(Math.abs(s[0].share - cachy) < 1e-9);
  // an_uncounted_strip_is_even
  const even = J(M.diskStrip(M.diskTiles(disks, sources, () => null)));
  assert.ok(even.every((x) => Math.abs(x.share - 1 / 3) < 1e-9));
  assert.deepEqual(J(M.diskStrip([])), []);
  // many_small_disks_all_reach_the_floor
  const ten = 'abcdefghij'.split('').map((id) => fixDisk(id, '/x', 'fixed', 'auto'));
  const many = J(M.diskStrip(M.diskTiles(ten, [], (id) => (id === 'a' ? 1000000 : 1))));
  assert.ok(many.slice(1).every((x) => x.share === M.STRIP_FLOOR));
  assert.ok(Math.abs(many[0].share - (1 - 9 * M.STRIP_FLOOR)) < 1e-9);
});

// Page only: where the lens stays after a click, and what the scope chip says.
test('after a click the lens stays on the first term of the place group', () => {
  const M = placeModel();
  const lensOn = (text, a) => ({ ...M.placeTerms(text).find((t) => t.start === a), a, raw: text.slice(a, M.placeTerms(text).find((t) => t.start === a).end) });
  const old = lensOn('rapor under:', 6);
  assert.deepEqual(J(M.placeAnchor('rapor under:/mnt/depo|under:/', 29, old)), ['rapor under:/mnt/depo|under:/', 21]);
  // None left: an empty `under:` where the group was, so the panel stays open.
  assert.deepEqual(J(M.placeAnchor('rapor ext:pdf', 6, lensOn('rapor under:/a ext:pdf', 6))), ['rapor under: ext:pdf', 12]);
  assert.deepEqual(J(M.placeAnchor('rapor', 5, lensOn('rapor under:/a', 6))), ['rapor under:', 12]);
  // Open on an exclusion: it stays there, wherever the group went.
  const ex = lensOn('!under:/x rapor', 0);
  assert.deepEqual(J(M.placeAnchor('!under:/x rapor under:/a', 24, ex)), ['!under:/x rapor under:/a', 9]);
});

test('the folder list is weighed by the query less its places, and no stray bar', () => {
  const M = placeModel();
  assert.equal(M.placeRest('rapor under:~/Projeler|under:~/Belgeler ext:pdf'), 'rapor ext:pdf');
  assert.equal(M.placeRest('rapor|fatura under:'), 'rapor|fatura');
  assert.equal(M.placeRest('rapor !under:/x under:/a|under:/b'), 'rapor');
  assert.equal(M.placeRest('under:/a|parent:/b rapor|fatura dm:7d'), 'rapor|fatura dm:7d');
});

test('the scope chip names the disks held whole, else the folder, else everywhere', () => {
  const M = placeModel();
  const wire = ([disks, sources]) => [disks.map((d) => ({
    volume: { id: d.id, label: d.label, kind: d.kind, fs: d.fs, roots: d.roots, size: d.size },
    state: d.state, seen: { live: d.live, walking: d.walking },
  })), sources];
  const [disks, sources] = wire(fixMachine());
  const says = (q) => J(M.scopeSays(q, PLACE_HOME, disks, sources));
  const all = says('rapor');
  assert.deepEqual(all.names, []);
  assert.deepEqual(all.dots.map((d) => d.kind), ['system', 'fixed', 'removable', 'network']);
  assert.deepEqual(all.dots.map((d) => d.asking), [false, false, true, false], 'a ringed dot for the stick');
  assert.ok(all.dots.filter((d) => !d.asking).every((d) => d.lit));
  const depo = says('rapor under:/mnt/depo');
  assert.deepEqual(depo.names, ['Depo']);
  assert.deepEqual(depo.dots.map((d) => d.lit), [false, true, false, false]);
  const cachy = 'rapor under:/etc|under:~|under:/opt|under:/usr|under:/var';
  assert.deepEqual(says(cachy + '|under:/mnt/depo').names, ['CACHY', 'Depo']);
  assert.deepEqual(says(cachy).names, ['CACHY']);
  assert.deepEqual(says('rapor under:~/Belgeler/').names, ['Belgeler']);
  assert.deepEqual(says('rapor under:~/Belgeler/').dots.map((d) => d.lit), [true, false, false, false]);
  assert.deepEqual(says('under:~').names, ['~']);
  assert.deepEqual(says('rapor !under:/mnt/depo').names, [], 'an exclusion is still everywhere else');
});
