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
  assert.deepEqual([...steps], ['rows', 'keys', 'note']);
  assert.deepEqual([...plan(steps, () => true)], [], 'nothing goes when it fits');
  assert.deepEqual([...plan(steps, (taken) => taken.length >= 2)], ['rows', 'keys'], 'the rows read and the keys go before the line that opens the panel');
  assert.deepEqual([...plan(steps, () => false)], [...steps], 'and at worst all of it, never the count or the time');
});

// The resources panel's model: `scour_ui::resources` in the page's words.
// Each case is a case of the Rust test named beside it, with the same inputs
// and the same answers; the words are the Rust's, string for string.
const RJ = (x) => JSON.parse(JSON.stringify(x));
function resourcesModel() {
  const c = context();
  c.T = (s) => s;
  vm.runInContext(section('  /* ---------------- the resources model', '\n  /* ---------------- the resources panel'), c);
  return c;
}
const RES_RS = () => readFileSync(require('node:path').join(__dirname, '../../../crates/scour-ui/src/resources.rs'), 'utf8');
const rustStrings = (block, keys) =>
  [...block.matchAll(new RegExp(`\\b(${keys}): "((?:[^"\\\\]|\\\\.)*)"`, 'g'))].map((m) => [m[1], m[2].replace(/\\(.)/g, '$1')]);
const rustBlock = (rs, from, until) => { const at = rs.indexOf(from); assert.ok(at >= 0, from); return rs.slice(at, rs.indexOf(until, at)); };

test('every thing the service sends has a line, and only the settings cannot be emptied (resources.rs)', () => {
  const c = resourcesModel();
  // every_thing_the_service_sends_has_a_line_and_only_the_settings_cannot_be_emptied
  for (const token of ['index', 'texts', 'previews', 'page', 'state']) {
    const t = c.resThing(token);
    assert.ok(t, token);
    assert.equal(t.act.danger, token !== 'state', token);
    assert.equal(t.act.ask === '', token === 'state', token);
  }
  assert.equal(c.resThing('nonsense'), null);
  assert.ok(c.resRefused('open'));
  assert.equal(c.resRefused(''), null);
  assert.equal(c.resRefused('kept'), null);
  assert.ok(c.resRestartAct().ask === '' && !c.resSort().danger);
});

test('the panel says what the Rust says, word for word (resources.rs)', () => {
  const c = resourcesModel();
  const rs = RES_RS();
  const keys = 'token|label|note|button|ask|go|done';
  const flat = (acts) => RJ(acts.flatMap((a) => keys.split('|').filter((k) => k in a).map((k) => [k, a[k]])));
  const things = c.resThings().map((t) => ({ token: t.token, label: t.label, note: t.note, ...t.act }));
  const thingsRs = rustBlock(rs, 'pub const THINGS', '\n];');
  assert.deepEqual(flat(things), rustStrings(thingsRs, keys));
  assert.deepEqual(RJ(things.map((t) => t.danger)), [...thingsRs.matchAll(/danger: (true|false)/g)].map((m) => m[1] === 'true'));
  assert.deepEqual(flat([c.resSort()]), rustStrings(rustBlock(rs, 'pub const SORT', '\n};'), keys));
  assert.deepEqual(flat([c.resRestartAct()]), rustStrings(rustBlock(rs, 'pub const RESTART', '\n};'), keys));
  assert.deepEqual(RJ(c.resRunning()), [...rustBlock(rs, 'pub const RUNNING', '\n];').matchAll(/\("(\w+)", "([^"]+)"\)/g)].map((m) => [m[1], m[2]]));
  assert.equal(c.resRefused('open'), /"open" => Some\("([^"]+)"\)/.exec(rs)[1]);
  const consts = Object.fromEntries([...rs.matchAll(/pub const (\w+): &str =\s*"((?:[^"\\]|\\.)*)";/g)].map((m) => [m[1], m[2]]));
  assert.deepEqual(RJ(c.resWords()), consts);
});

test('shares are of one core and of the machine (resources.rs)', () => {
  const c = resourcesModel();
  // shares_are_of_one_core_and_of_the_machine
  const a = { uptime_ms: 10000, service_ms: 1000, busy: 100, total: 1000 };
  const b = { uptime_ms: 12000, service_ms: 1500, busy: 150, total: 1400 };
  const s = c.resShares(a, b);
  assert.ok(Math.abs(s.service - 25) < 0.01, JSON.stringify(s));
  assert.ok(Math.abs(s.machine - 12.5) < 0.01, JSON.stringify(s));
  // The same answer twice, and a service that started again in between.
  assert.equal(c.resShares(a, a), null);
  assert.equal(c.resShares(b, a), null);
});

test('the index pages are a line of their own and not held (resources.rs)', () => {
  const c = resourcesModel();
  // the_index_pages_are_a_line_of_their_own_and_not_held
  const MB = 2 ** 20;
  const both = c.resServiceLines({ service: 360 * MB, peak: 528 * MB, own: 47 * MB, mapped: 313 * MB });
  assert.deepEqual(RJ(both), [
    { label: 'Service', note: '', bytes: 47 * MB, peak: 0 },
    { label: 'Index pages', note: "a cache the system takes back when it needs to", bytes: 313 * MB, peak: 0 },
  ]);
  assert.equal(c.resServiceHeld({ service: 360 * MB, own: 47 * MB }), 47 * MB);
  // An older service: one line with its peak, all of it held.
  const one = c.resServiceLines({ service: 360 * MB, peak: 528 * MB });
  assert.deepEqual(RJ(one.map((l) => [l.note, l.peak])), [['at most {n} since it started', 528 * MB]]);
  assert.equal(c.resServiceHeld({ service: 360 * MB }), 360 * MB);
  assert.deepEqual(RJ(c.resServiceLines({ service: 0, peak: 0, own: 0, mapped: 0 })), []);
  assert.equal(c.resServiceLines({ service: 0, peak: 0, own: 5, mapped: 0 }).length, 1);
});

test('uptime is said in its two largest units, and bars never vanish (resources.rs)', () => {
  const c = resourcesModel();
  // uptime_is_said_in_its_two_largest_units
  assert.deepEqual(RJ(c.resUptime(42000)), ['{a} s', 42, 0]);
  assert.deepEqual(RJ(c.resUptime(82000)), ['{a} min {b} s', 1, 22]);
  assert.deepEqual(RJ(c.resUptime(4380000)), ['{a} h {b} min', 1, 13]);
  assert.deepEqual(RJ(c.resUptime(97200000)), ['{a} d {b} h', 1, 3]);
  // bars_are_shares_of_the_largest_and_never_vanish
  assert.deepEqual(RJ(c.resBars([545, 0, 296])), [1, 0, 296 / 545]);
  assert.deepEqual(RJ(c.resBars([2 ** 30, 1])), [1, 0.01]);
  assert.deepEqual(RJ(c.resBars([])), []);
  assert.deepEqual(RJ(c.resBars([0, 0])), [0, 0]);
  // format.rs: compact_bytes
  assert.equal(c.compactBytes(667264614, ','), '636,4 MB');
  assert.equal(c.compactBytes(667264614, '.'), '636.4 MB');
  assert.equal(c.compactBytes(2147483648, ','), '2,0 GB');
  assert.equal(c.compactBytes(0, '.'), '0.0 MB');
  assert.equal(c.compactBytes(1073741823, '.'), '1024.0 MB', 'the unit is chosen before the rounding, as there');
  assert.equal(c.compactBytes(1310720, '.'), '1.2 MB', 'a tie goes to the even tenth');
  assert.deepEqual([c.decimalMark('tr'), c.decimalMark('tr-TR'), c.decimalMark('en')], [',', ',', '.']);
  assert.equal(c.resFill('{n} entries · parts: {parts}', { n: '7', parts: '8' }), '7 entries · parts: 8');
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

// The content field's model: `scour_ui::pattern` (pieces, edit, launch) in
// the page's words. Each case is one of `pattern.rs`, `pattern/edit.rs` or
// `pattern/launch.rs`'s tests, with the same inputs and answers.
const PJ = (x) => JSON.parse(JSON.stringify(x));
function patternModel() {
  const c = context();
  c.T = (s) => s;
  vm.runInContext(section('  const escape_ = (s) =>', '\n  const FIELDS = ['), c);
  vm.runInContext(section('  /* ---------------- the content pattern', '\n  /* ---------------- the content field'), c);
  vm.runInContext(section('  /* ---------------- the content field', '\n  /* ---- drawn ---- */'), c);
  c.CONTENT_ITEMS = vm.runInContext('CONTENT_ITEMS', c);
  return c;
}

test('a content pattern is taken apart as the search takes it (pattern.rs)', () => {
  const c = patternModel();
  const shown = (input) => {
    const ch = Array.from(input);
    return PJ(c.patternPieces(input)).map((p) => [ch.slice(p.start, p.end).join(''), p.kind, p.ops.map((i) => ch[i])]);
  };
  assert.deepEqual(shown('icra "dava dosyası" !karar a;b'), [
    ['icra', 'word', []], ['"dava dosyası"', 'phrase', ['"', '"']], ['!karar', 'not', ['!']], ['a;b', 'or', [';']],
  ]);
  assert.deepEqual(shown('(a | b) x ; y ! z'), [
    ['(a | b)', 'or', ['(', '|', ')']], ['x ; y', 'or', [';']], ['! z', 'not', ['!']],
  ]);
  assert.deepEqual(shown('şirket'), [['şirket', 'word', []]]);
  // A tilde forgives, and each alternative is its own capsule.
  assert.deepEqual(shown('~rapor'), [['~rapor', 'similar', ['~']]]);
  assert.deepEqual(shown('!~taslak ~a|b'), [['!~taslak', 'not', ['!', '~']], ['~a|b', 'or', ['~', '|']]]);
  assert.deepEqual(shown('a~b'), [['a~b', 'word', []]]);
  assert.deepEqual(shown('~'), [['~', 'similar', ['~']]]);
  assert.equal(shown('"~x"')[0][1], 'phrase');
  const alts = (input) => {
    const ch = Array.from(input);
    return PJ(c.patternPieces(input)).map((p) => p.alts.map((a) => [ch.slice(a.start, a.end).join(''), a.phrase, a.similar]));
  };
  assert.deepEqual(alts('icra ~a|"b c" !(x | y)'), [
    [['icra', false, false]],
    [['a', false, true], ['b c', true, false]],
    [['x', false, false], ['y', false, false]],
  ]);
  // An operator with no word yet: a capsule with a hole.
  assert.deepEqual(shown('!'), [['!', 'not', ['!']]]);
  assert.deepEqual(shown('ara !'), [['ara', 'word', []], ['!', 'not', ['!']]]);
  assert.deepEqual(shown(';'), [[';', 'or', [';']]]);
  assert.deepEqual(shown('; x'), [['; x', 'or', [';']]]);
  assert.deepEqual(shown('""'), [['""', 'phrase', ['"', '"']]]);
  const holes = (input) => PJ(c.patternPieces(input)).flatMap((p, k) =>
    p.alts.filter((a) => a.start === a.end).map((a) => [k, a.start, a.phrase, a.similar]));
  assert.deepEqual(holes('!'), [[0, 1, false, false]]);
  assert.deepEqual(holes('ara !'), [[1, 5, false, false]]);
  assert.deepEqual(holes('!~'), [[0, 2, false, true]]);
  assert.deepEqual(holes('~'), [[0, 1, false, true]]);
  assert.deepEqual(holes('""'), [[0, 1, true, false]]);
  assert.deepEqual(holes('!""'), [[0, 2, true, false]]);
  assert.deepEqual(holes('"'), [[0, 1, true, false]]);
  assert.deepEqual(holes('ara;'), [[0, 4, false, false]]);
  assert.deepEqual(holes('ara ;'), [[0, 5, false, false]]);
  assert.deepEqual(holes('a;;'), [[0, 3, false, false]]);
  assert.deepEqual(holes(';x'), []);
  assert.deepEqual(holes('a;b'), []);
  assert.ok(c.patternEmpty(c.patternPieces('!')[0]));
  assert.ok(!c.patternEmpty(c.patternPieces('ara;')[0]));
  assert.deepEqual(PJ(c.patternHoleAt(c.patternPieces('x ara;'), 6)), [1, 1]);
  assert.equal(c.patternHoleAt(c.patternPieces('x ara;'), 5), null);
  assert.deepEqual(alts('"ab c'), [[['ab c', true, false]]]);
  // The caret's piece.
  const p = c.patternPieces('ab cd');
  assert.equal(c.patternPieceAt(p, 0), 0);
  assert.equal(c.patternPieceAt(p, 2), 0);
  assert.equal(c.patternPieceAt(p, 4), 1);
  assert.equal(c.patternPieceAt(c.patternPieces('ab  cd'), 3), null);
});

test('the pill rewrites the pattern (pattern/edit.rs)', () => {
  const c = patternModel();
  assert.equal(c.patternRemove('a b c', 1), 'a c');
  assert.equal(c.patternRemove('a b c', 2), 'a b');
  assert.equal(c.patternRemove('a', 0), '');
  assert.equal(c.patternToggleNot('a b', 1), 'a !b');
  assert.equal(c.patternToggleNot('a !b', 1), 'a b');
  assert.equal(c.patternToggleNot('a ! b', 1), 'a b');
  assert.equal(c.patternToggleNot('x|y', 0), '!x|y');
  assert.equal(c.patternToggleNot('x ; !y', 0), 'x ; y');
  assert.equal(c.patternToggleSimilar('rapor', 0, 0), '~rapor');
  assert.equal(c.patternToggleSimilar('~rapor', 0, 0), 'rapor');
  assert.equal(c.patternToggleSimilar('a|b', 0, 1), 'a|~b');
  assert.equal(c.patternToggleSimilar('!~x', 0, 0), '!x');
  assert.equal(c.patternToggleSimilar('"a b"', 0, 0), '"a b"');
  assert.ok(c.patternPieces('~a')[0].alts[0].similar);
});

test('an operator opens a capsule and backspace in its hole takes it (pattern/edit.rs)', () => {
  const c = patternModel();
  const b = (t, k) => PJ(c.patternBackspace(t, k));
  assert.deepEqual(b('!', 1), ['', 0]);
  assert.deepEqual(b('ara !', 5), ['ara ', 4]);
  assert.deepEqual(b('!~', 2), ['', 0]);
  assert.deepEqual(b('~', 1), ['', 0]);
  assert.deepEqual(b('""', 1), ['', 0]);
  assert.deepEqual(b('x !"" y', 4), ['x  y', 2]);
  assert.deepEqual(b('ara;', 4), ['ara', 3]);
  assert.deepEqual(b('ara ;', 5), ['ara ', 4]);
  assert.deepEqual(b('a;~', 3), ['a', 1]);
  assert.deepEqual(b(';', 1), ['', 0]);
  assert.deepEqual(b('"ab c" x', 1), ['ab c x', 0]);
  assert.deepEqual(b('"ab', 1), ['ab', 0]);
  assert.equal(c.patternBackspace('ara', 3), null);
  assert.equal(c.patternBackspace('!ara', 1), null);
  assert.equal(c.patternBackspace('a;b', 2), null);
  assert.equal(c.patternBackspace('', 0), null);
  // A space in a hole is not written; quotes come in pairs.
  assert.deepEqual(PJ(c.patternSpace('!', 1)), ['!', 1]);
  assert.deepEqual(PJ(c.patternSpace('ara;', 4)), ['ara;', 4]);
  assert.equal(c.patternSpace('""', 1), null);
  // A space keeps its capsule: after a letter the word becomes a phrase.
  const sp = (t, k) => PJ(c.patternSpace(t, k));
  assert.deepEqual(sp('osmanlı', 7), ['"osmanlı "', 9]);
  assert.deepEqual(sp('osmanlı', 3), ['"osm anlı"', 5]);
  assert.deepEqual(sp('!osm', 4), ['!"osm "', 6]);
  assert.deepEqual(sp('a;osm', 5), ['a;"osm "', 7]);
  assert.deepEqual(sp('~osm', 4), ['~"osm "', 6]);
  assert.equal(c.patternSpace('osm', 0), null);
  assert.equal(c.patternSpace('ara ', 4), null);
  assert.equal(c.patternSpace('"a"', 2), null);
  // A paste is typed: words in one capsule; a pattern as it is.
  const pa = (t, a, b, x) => PJ(c.patternPaste(t, a, b, x));
  assert.deepEqual(pa('', 0, 0, 'osmanlı devleti'), ['"osmanlı devleti"', 16]);
  assert.deepEqual(pa('!', 1, 1, 'osmanlı\ndevleti\n'), ['!"osmanlı devleti"', 17]);
  assert.deepEqual(pa('ara ', 4, 4, 'a b'), ['ara "a b"', 8]);
  assert.deepEqual(pa('"x "', 3, 3, 'a b'), ['"x a b"', 6]);
  assert.deepEqual(pa('ara', 3, 3, 'ba'), ['araba', 5]);
  assert.deepEqual(pa('', 0, 0, '"iş kazası" !ret'), ['"iş kazası" !ret', 16]);
  assert.deepEqual(pa('', 0, 0, 'a;b c'), ['a;b c', 5]);
  assert.deepEqual(pa('ara xyz', 4, 7, 'a b'), ['ara "a b"', 8]);
  assert.deepEqual(pa('"a"', 3, 3, ' !b'), ['"a" !b', 6]);
  assert.deepEqual(pa('x', 0, 0, '!b '), ['!b x', 3]);
  // Tab: out of the capsule, past one space, the phrase tidied.
  const lv = (t, k) => PJ(c.patternLeave(t, k));
  assert.deepEqual(lv('osmanlı', 7), ['osmanlı ', 8]);
  assert.deepEqual(lv('osmanlı', 2), ['osmanlı ', 8]);
  assert.deepEqual(lv('"osmanlı "', 9), ['osmanlı ', 8]);
  assert.deepEqual(lv('"osmanlı devleti "', 17), ['"osmanlı devleti" ', 18]);
  assert.deepEqual(lv('"dava d', 7), ['"dava d" ', 9]);
  assert.deepEqual(lv('!"osmanlı "', 10), ['!osmanlı ', 9]);
  assert.deepEqual(lv('x;"a b "', 7), ['x;"a b" ', 8]);
  assert.deepEqual(lv('"a;b"', 4), ['"a;b" ', 6]);
  assert.deepEqual(lv('ara !', 5), ['ara ', 4]);
  assert.deepEqual(lv('a b', 1), ['a  b', 2]);
  assert.deepEqual(lv('a  b', 1), ['a  b', 2]);
  assert.equal(c.patternLeave('ara ', 4), null);
  assert.deepEqual(PJ(c.patternQuote('', 0)), ['""', 1]);
  assert.deepEqual(PJ(c.patternQuote('x !', 3)), ['x !""', 4]);
  assert.equal(c.patternQuote('x', 1), null);
  assert.deepEqual(PJ(c.patternQuote('"ab"', 3)), ['"ab"', 4]);
  assert.equal(c.patternQuote('"ab', 3), null);
  assert.equal(c.patternQuote('x y', 2), null);
});

test('a colon asks, letters narrow it, and choosing writes in its place (pattern/launch.rs)', () => {
  const c = patternModel();
  assert.deepEqual(PJ(c.contentAsking('rapor :', 7)), [6, '']);
  assert.deepEqual(PJ(c.contentAsking('rapor :ben', 10)), [6, 'ben']);
  assert.deepEqual(PJ(c.contentAsking(':', 1)), [0, '']);
  assert.equal(c.contentAsking('"a :', 4), null);
  assert.equal(c.contentAsking('10:3', 4), null);
  assert.equal(c.contentAsking('rapor', 5), null);
  const same = (s) => s;
  const tokens = (typed, name) => PJ(c.contentItems(typed, name).map((i) => i.token));
  assert.equal(c.contentItems('', same).length, 5);
  assert.deepEqual(tokens('sim', same), ['similar']);
  const tr = (m) => ({ 'Similar spelling': 'Benzer yazım', Without: 'Değil' })[m] || m;
  assert.deepEqual(tokens('BEN', tr), ['similar']);
  assert.deepEqual(tokens('xyz', same), []);
  const by = (t) => c.CONTENT_ITEMS.find((i) => i.token === t);
  assert.deepEqual(PJ(c.contentChoose('rapor :', 6, 7, by('phrase'))), ['rapor ""', 7]);
  assert.deepEqual(PJ(c.contentChoose('rapor :ve', 6, 9, by('or'))), ['rapor|', 6]);
  assert.deepEqual(PJ(c.contentChoose(':ben x', 0, 4, by('similar'))), ['~ x', 1]);
  assert.deepEqual(PJ(c.contentChoose('a :c', 2, 4, by('case'))), ['a ', 2]);
  // The Rust model's labels and about lines are the page's, word for word.
  const rust = readFileSync(require('node:path').join(__dirname, '../../../crates/scour-ui/src/pattern/launch.rs'), 'utf8');
  for (const it of c.CONTENT_ITEMS) {
    assert.ok(rust.includes(`token: "${it.token}",\n        label: "${it.label}",\n        about: "${it.about}",`), it.token);
  }
});

/* The text of a drawn layer outside its art, as the caret's offsets read it. */
function layerLetters(html) {
  let out = '', art = 0;
  const stack = [];
  for (const m of html.matchAll(/<(\/?)(span|i)([^>]*)>|([^<]+)/g)) {
    if (m[4] !== undefined) { if (!art) out += m[4]; continue; }
    if (m[1]) { if (stack.pop()) art--; continue; }
    const isArt = /class="art\b/.test(m[3]);
    stack.push(isArt);
    if (isArt) art++;
  }
  return out.replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&quot;/g, '"').replace(/&#39;/g, "'").replace(/&amp;/g, '&');
}

test('the content field draws every piece as its capsule, the one being written too', () => {
  const c = patternModel();
  const text = 'icra "dava dosyası" ~rapor a|~b !taslak';
  const n = Array.from(text).length;
  const caps = (html) => [...html.matchAll(/<span class="gc gk-([\w ]+)" data-k="(\d+)"/g)].map((m) => [m[1], +m[2]]);
  const ands = (html) => (html.match(/class="art gand"/g) || []).length;
  // Caret at the end: the last piece is a capsule too, ringed.
  const atEnd = c.grepLayerHtml(text, n);
  assert.deepEqual(caps(atEnd), [['word', 0], ['phrase', 1], ['similar', 2], ['or many', 3], ['not live', 4]]);
  assert.equal(ands(atEnd), 4);
  assert.match(atEnd, /<span class="z">&quot;<\/span><i class="art gq">“<\/i>dava dosyası<i class="art gq">”<\/i><span class="z">&quot;<\/span>/);
  assert.match(atEnd, /data-k="2" data-a="20" data-b="26"><span class="z">~<\/span><span class="art gh">similar<\/span><span class="gb"><span class="gw" data-alt="0">rapor<\/span>/);
  assert.match(atEnd, /<span class="art gh">either<\/span><span class="gb"><span class="gw" data-alt="0">a<\/span><span class="art gor">or<\/span><span class="gw" data-alt="1"><span class="z">\|<\/span><span class="z">~<\/span><i class="art gsim">≈<\/i>b<\/span>/);
  assert.match(atEnd, /<span class="z">!<\/span><span class="art gh">without<\/span><span class="gb"><span class="gw" data-alt="0">taslak<\/span>/);
  assert.equal(layerLetters(atEnd), text, 'every letter once, in order');
  // The caret elsewhere moves the ring and nothing else.
  const inside = c.grepLayerHtml(text, 23);
  assert.deepEqual(caps(inside), [['word', 0], ['phrase', 1], ['similar live', 2], ['or many', 3], ['not', 4]]);
  assert.equal(inside.replace(/ live/g, ''), atEnd.replace(/ live/g, ''));
  assert.equal(c.grepLayerHtml(text, -1), atEnd.replace(/ live/g, ''));
  // An operator with no word yet: its capsule, a slot where the word goes.
  const bang = c.grepLayerHtml('ara !', 5);
  assert.match(bang, /<span class="art gand">and<\/span><span class="z"> <\/span><span class="gc gk-not live" data-k="1" data-a="4" data-b="5"><span class="z">!<\/span><span class="art gh">without<\/span><span class="gb"><span class="gw hole" data-alt="0"><span class="art gslot" data-c="5"><\/span><\/span>/);
  assert.equal(layerLetters(bang), 'ara !');
  const or = c.grepLayerHtml('ara;', 4);
  assert.match(or, /data-alt="0">ara<\/span><span class="art gor">or<\/span><span class="gw hole" data-alt="1"><span class="z">;<\/span><span class="art gslot" data-c="4">/);
  assert.equal(layerLetters(c.grepLayerHtml('""', 1)), '""');
  // The "and" keeps its room after a space; its word shows when the caret is there.
  assert.match(c.grepLayerHtml('ara ', 4), /<span class="art gand">and/);
  assert.match(c.grepLayerHtml('ara ', 1), /<span class="art gand mute">and/);
  for (const t of ['a  <b> "c', '(a | b) c', 'x ; y ! z', '!~', 'a;;', '"ab c', '; x', 'x !"" y']) {
    assert.equal(layerLetters(c.grepLayerHtml(t, 1)), t, t);
  }
  assert.equal(c.grepLayerHtml('', 0), '');
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
  const q = { value: '', selectionStart: 0, selectionEnd: 0, setSelectionRange(a, b) { this.selectionStart = a; this.selectionEnd = b; } };
  const c = paintContext(q);

  // Not being written: `7d` is said as "last 7 days", the letters stay in the text.
  c.paint('dm:7d kind:doc');
  assert.match(c.shadow.innerHTML, /class="term cap said"[^>]*>.*<span class="art said-art">last 7 days<\/span>/s);
  assert.match(c.shadow.innerHTML, /<span class="art pv" data-k="0" data-at="14">Document<\/span>/);
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

  // A run of bars is said as the days it covers; a size as its preset; `empty:` is a size.
  c.document.activeElement = null;
  const said = (text) => { c.paint(text); return [...c.shadow.innerHTML.matchAll(/<span class="art said-art">([^<]*)<\/span>/g)].map((m) => m[1]); };
  assert.deepEqual(said('dm:3w size:10mb..1gb'), ['last 3 weeks', '10 MB to 1 GB']);
  assert.match(said('dm:<2025-01-01')[0], /^before 2025$/);
  assert.deepEqual(said('!empty: bos:'), ['Empty', 'Empty']);
  assert.match(c.shadow.innerHTML, /class="term cap neg said"[^>]*><span class="cap-head"><span class="art head-art"><span class="bang">!<\/span><svg/);
});

/* A context that paints the query layer with the real models behind it. */
function paintContext(q) {
  q.addEventListener = () => {};
  const c = vm.createContext({
    Set, Map, Math, TextEncoder, JSON,
    document: { activeElement: null, getElementById: () => ({}) },
    q, shadow: { innerHTML: '', querySelector: () => null, clientWidth: 0 }, getComputedStyle: () => ({}),
    LAST_SPANS: [], SYNTAX: new Set(['colon', 'cmp', 'quote', 'not', 'or', 'sep']),
    fitQuery() {}, markHover() {}, render() {}, lensClose() {}, lensSync() {},
    placeCapsDrawn: () => [], placeCapKey: () => null, placeFit() {},
    queryCaret: () => ({ at: q.selectionStart, editing: !!q.editing }),
    placeZero: (s) => (s ? '<span class="z">' + s + '</span>' : ''),
    T: (id, v) => (v ? id.replace(/\{(\w+)\}/g, (_, k) => v[k]) : id), KIND_KEY: { doc: 'Document', code: 'Code' },
    Date, Intl, LANG: 'en', DAY: 86400, fmt: String,
  });
  vm.runInContext(section('  const escape_ = (s) =>', '\n  const acts ='), c);
  vm.runInContext(section('  const ENCODER =', '\n  /* The query is laid out by the browser'), c);
  vm.runInContext(section('  const LENS_FIELDS = {', '\n  /* Counting a query'), c);
  vm.runInContext(section('  const quoteIf = ', '\n  /* A path as a person'), c);
  for (const name of ['the terms', 'the place model', 'the place rules', 'the caret over a place capsule',
    'the time in words', 'the time model', 'the type and size model']) {
    vm.runInContext(section('  /* ---------------- ' + name, '\n  /* ---------------- '), c);
  }
  vm.runInContext(section('  /* ---------------- the list capsule', '\n  /* Whether a row has a picture'), c);
  vm.runInContext('lastQueryCaret = 0', c);
  return c;
}

test('a kind or ext list at rest is a chip a value, raw while edited, and Backspace takes one value', () => {
  const q = { value: '', selectionStart: 0, selectionEnd: 0, setSelectionRange(a, b) { this.selectionStart = a; this.selectionEnd = b; } };
  const c = paintContext(q);
  const chips = () => [...c.shadow.innerHTML.matchAll(/<span class="art pv( more)?"[^>]*>([^<]*)<\/span>/g)].map((m) => m[2]);
  // At rest: each value its chip — a kind by its name, an extension as written — a dot between.
  c.paint('kind:doc;code ext:.pdf ; odt !kind:build');
  assert.deepEqual(chips(), ['Document', 'Code', 'pdf', 'odt', 'build']);
  assert.match(c.shadow.innerHTML, /class="term cap list" data-a="0" data-b="13" data-h="5">/);
  assert.match(c.shadow.innerHTML, /data-at="8">Document<\/span><span class="z">;<\/span><span class="z">code<\/span><span class="art psep" data-at="9">·<\/span>/);
  assert.match(c.shadow.innerHTML, /class="term cap list neg"[^>]*><span class="cap-head"><span class="art head-art"><span class="bang">!<\/span>/);
  // Every letter of the query stays in the layer.
  assert.equal(lettersOf(c.shadow.innerHTML), 'kind:doc;code ext:.pdf ; odt !kind:build');
  // The caret resting at its end keeps it whole; editing there shows the letters.
  c.document.activeElement = q;
  q.selectionStart = q.selectionEnd = 13;
  c.paint('kind:doc;code rapor');
  assert.deepEqual(chips(), ['Document', 'Code']);
  q.editing = true;
  c.paint('kind:doc;code rapor');
  assert.deepEqual(chips(), []);
  assert.match(c.shadow.innerHTML, /class="term cap edit"/);
  q.editing = false;
  // Backspace at a chip's edge takes that value; Delete the one after the edge.
  const key = (text, at, back) => {
    q.value = text; q.selectionStart = q.selectionEnd = at;
    c.listCapsDrawn(text, { at, editing: false });
    return J(c.listCapKey(back));
  };
  assert.deepEqual(key('kind:doc;code rapor', 13, true), ['kind:doc rapor', 8]);
  assert.deepEqual(key('kind:doc;code rapor', 8, true), ['kind:code rapor', 5]);
  assert.deepEqual(key('kind:doc;code', 5, false), ['kind:code', 5]);
  assert.deepEqual(key('kind:doc;code', 8, false), ['kind:doc', 8]);
  assert.deepEqual(key('!ext:pdf ; odt', 14, true), ['!ext:pdf', 8]);
  assert.deepEqual(key('rapor kind:doc', 14, true), ['rapor', 5]);
  assert.equal(key('kind:doc;code', 7, true), null, 'inside a value the letters are the text\'s own');
  // The arrows stop at the start of the values and at each chip's edge.
  q.value = 'kind:doc;code;image x'; c.listCapsDrawn(q.value, null);
  const steps = [];
  q.selectionStart = q.selectionEnd = 5;
  while (c.listArrow(true)) steps.push(q.selectionStart);
  assert.deepEqual(steps, [8, 13, 19]);
});

test('capsule field headers render as cap-head and block caret or backspace inside ext:', () => {
  const q = {
    value: 'ext:docx',
    selectionStart: 2,
    selectionEnd: 2,
    selectionDirection: 'forward',
    setSelectionRange(a, b, d) { this.selectionStart = a; this.selectionEnd = b; if (d) this.selectionDirection = d; },
  };
  const c = paintContext(q);
  c.document.activeElement = q;
  q.editing = true; // being typed: drawn raw, not as a chip
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
  vm.runInContext(section('  function placeParts(v) {', '\n  function lensClose()'), c);
  vm.runInContext(section('  /* ---------------- the terms', '\n  /* ---------------- '), c);
  vm.runInContext(section('  /* ---------------- the place model', '\n  /* ---------------- the '), c);
  vm.runInContext(section('  /* ---------------- the caret over a place capsule', '\n  /* ---------------- '), c);
  vm.runInContext(section('  /* ---------------- the place rules', '\n  /* ---------------- '), c);
  vm.runInContext(section('  /* ---------------- where the query looks', '\n  /* ---------------- '), c);
  vm.runInContext(section('  function scopeSays(', '\n  /* As many names'), c);
  return vm.runInContext(`({ placeHead, placeAtHead, placeToggle, tileToggle, placeScope, placeRemove, placeReplace, placeRuns, placeRunOf,
    placeTerms, scopeHolds, diskTiles, diskStrip, diskOf, scopeSays, STRIP_FLOOR, DISK_KINDS, quotedPath,
    placeTermAt, placeAddAt, placeAddWith, placeExclude, placeSettle, placeAt, withoutPlaces, spots, spotRemove,
    chipLabel, chipDots, tileHeld, tileLit, crumbs, crumbsOn, tileOf, capDisk, placeCapsAt, exclusionCaps, capPartAt, capFold,
    capStops, capStep, capBlocks, opensByItself, editedTerm, caretResting, caretEditing,
    negatePlaces, unnegatePlaces, exclusionBlocks })`, c);
}

test('the place group: a disk in, a second beside it, one out, none left (lens/group_tests.rs)', () => {
  const M = placeModel();
  const pt = (q, root, pick) => J(M.placeToggle(q, PLACE_HOME, root, pick));
  const also = (q, root) => pt(q, root, 'also')[0];
  const only = (q, root) => pt(q, root, 'only')[0];
  // the_first_disk_is_one_term
  assert.deepEqual(pt('rapor', '/mnt/depo', 'only'), ['under:/mnt/depo rapor', 15]);
  assert.equal(also('', '/mnt/depo'), 'under:/mnt/depo');
  assert.equal(also('rapor ', '/'), 'under:/ rapor ');
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
  assert.equal(q, 'under:"/mnt/depo/My Docs" rapor');
  q = also(q, '/srv/a|b');
  assert.equal(q, 'under:"/mnt/depo/My Docs"|under:"/srv/a|b" rapor');
  assert.equal(also(q, '/mnt/depo/My Docs'), 'under:"/srv/a|b" rapor');
  // home_is_written_and_read_with_a_tilde
  assert.equal(also('rapor', '/home/hasan/Belgeler'), 'under:~/Belgeler rapor');
  assert.equal(also('rapor under:~/Belgeler', '/mnt/depo'), 'rapor under:~/Belgeler|under:/mnt/depo');
  assert.equal(also('rapor under:~/Belgeler/|under:/mnt/depo', '/home/hasan/Belgeler'), 'rapor under:/mnt/depo');
  // an_exclusion_elsewhere_is_left_alone
  assert.equal(also('rapor !under:/mnt/depo/tmp', '/mnt/depo'), 'under:/mnt/depo rapor !under:/mnt/depo/tmp');
  const x = '!under:/x rapor under:/a|under:/b';
  assert.equal(also(x, '/a'), '!under:/x rapor under:/b');
  assert.equal(only(x, '/b'), '!under:/x rapor under:/b');
  assert.equal(also('rapor !under:/a', '/a'), 'under:/a rapor !under:/a');
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
  assert.equal(q, 'under:~|under:/usr|under:/etc rapor');
  assert.equal(tile(q, sys, 'only'), 'rapor');
  assert.equal(tile(q, sys, 'also'), 'rapor');
  const both = tile(q, ['/mnt/depo'], 'also');
  assert.equal(both, 'under:~|under:/usr|under:/etc|under:/mnt/depo rapor');
  assert.equal(tile(both, ['/mnt/depo'], 'only'), 'under:/mnt/depo rapor');
  assert.equal(tile(both, sys, 'also'), 'under:/mnt/depo rapor');
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
  assert.deepEqual([x.roots, x.whole], [['/mnt/x'], true], 'a click has its root to write');
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

// `lens/machine.rs`: the owner's machine as tiles — the system disk indexed
// through `home` and `sistem`, Depo, a stick asking.
const SYSTEM = 'under:/etc|under:~|under:/opt|under:/usr|under:/var';
function ruleMachine(M) {
  const disk = (id, label, kind, roots, state) =>
    ({ id, label, kind, fs: 'fs', roots, size: 1, state, live: false, walking: false });
  const disks = [
    disk('cachy', 'CACHY', 'system', ['/'], 'manual'),
    disk('depo', 'Depo', 'fixed', ['/mnt/depo'], 'configured'),
    disk('stick', 'STICK', 'removable', ['/run/media/hasan/STICK'], 'asking'),
  ];
  const sources = [
    { name: 'home', roots: [PLACE_HOME] }, { name: 'sistem', roots: ['/usr', '/etc', '/opt', '/var'] },
    { name: 'depo', roots: ['/mnt/depo'] },
  ];
  return M.diskTiles(disks, sources, () => null);
}

test('Add joins, narrows and widens in place, and never takes a folder out (lens/folders_tests.rs)', () => {
  const M = placeModel();
  const H = PLACE_HOME;
  const add = (q, p) => M.placeAddWith(q, H, p, null)[0];
  // add_joins_the_group_and_says_where_it_wrote
  let [q, at] = M.placeAddWith('rapor under:/mnt/depo ext:pdf', H, '/usr', null);
  assert.equal(q, 'rapor under:/mnt/depo|under:/usr ext:pdf');
  assert.equal(q.slice(0, at), 'rapor under:/mnt/depo|under:/usr');
  assert.equal(add('rapor', '/home/hasan/Belgeler'), 'under:~/Belgeler rapor');
  assert.equal(add('', '/'), 'under:/');
  // add_never_takes_a_folder_out
  const g = 'rapor under:/mnt/depo|under:/usr';
  assert.deepEqual(J(M.placeAddWith(g, H, '/usr/', null)), [g, g.length]);
  assert.equal(add(add(g, '/opt'), '/opt'), add(g, '/opt'));
  // a_folder_inside_a_member_narrows_it_where_it_stands
  [q, at] = M.placeAddWith('rapor under:~|under:/mnt/depo', H, '~/Belgeler', null);
  assert.equal(q, 'rapor under:~/Belgeler|under:/mnt/depo');
  assert.equal(q.slice(0, at), 'rapor under:~/Belgeler');
  assert.equal(add('rapor ' + SYSTEM, '/home/hasan/Projeler/Scour'),
    'rapor under:/etc|under:~/Projeler/Scour|under:/opt|under:/usr|under:/var');
  // a_wider_folder_takes_every_member_it_holds
  assert.equal(add('rapor under:~/a|under:/mnt/depo|under:~/b ext:pdf', '~'), 'rapor under:~|under:/mnt/depo ext:pdf');
  assert.equal(add('x ' + SYSTEM, '/'), 'x under:/');
  assert.equal(add('under:~', '/home/hasan2'), 'under:~|under:/home/hasan2', '/home/hasan2 is not inside ~');
  // an_empty_place_or_one_being_typed_gives_way
  assert.equal(add('rapor under: ext:pdf', '/mnt/depo'), 'rapor under:/mnt/depo ext:pdf');
  const typed = 'rapor under:/mnt/depo|under:~/gel';
  const at_ = (q, p, c) => M.placeAddAt(q, H, p, c)[0];
  assert.equal(at_(typed, '~/Belgeler', M.caretEditing(typed.length)), 'rapor under:/mnt/depo|under:~/Belgeler');
  const walked = 'rapor under:~/';
  assert.equal(at_(walked, '~/Belgeler', M.caretEditing(walked.length)), 'rapor under:~/Belgeler');
  const other = 'rapor under:/mnt/depo ext:pd';
  assert.equal(at_(other, '/usr', M.caretEditing(other.length)), 'rapor under:/mnt/depo|under:/usr ext:pd');
  // add_takes_away_the_same_folders_exclusion
  [q, at] = M.placeAddWith('!under:~/x rapor under:/usr', H, '~/x', null);
  assert.equal(q, 'rapor under:/usr|under:~/x');
  assert.equal(q.slice(0, at), 'rapor under:/usr|under:~/x');
  assert.equal(add('rapor !under:~/x/y', '~/x'), 'under:~/x rapor !under:~/x/y');
  // quoted_paths_are_read_and_written_quoted
  q = add('rapor', '/mnt/depo/My Docs');
  assert.equal(q, 'under:"/mnt/depo/My Docs" rapor');
  assert.equal(add(q, '/mnt/depo/My Docs'), q);
  assert.equal(add(q, '/mnt/depo/My Docs/2025 Q1'), 'under:"/mnt/depo/My Docs/2025 Q1" rapor');
});

test('where it looks is the first thing the query says (lens/group_tests.rs)', () => {
  const M = placeModel();
  assert.equal(M.placeHead('under:/a|under:/b !under:/a/x rapor'), 29);
  assert.equal(M.placeHead('rapor under:/a'), 0);
  assert.equal(M.placeHead('!under:/x rapor'), 9);
  assert.deepEqual(J(M.placeAtHead('rapor', 'under:', 0)), ['under: rapor', 6]);
  assert.deepEqual(J(M.placeAtHead('', 'under:', 0)), ['under:', 6]);
  assert.deepEqual(J(M.placeAtHead('under:/a rapor', '!under:/b', 8)), ['under:/a !under:/b rapor', 18]);
  assert.deepEqual(J(M.placeAtHead('under:/a', '!under:/b', 8)), ['under:/a !under:/b', 18]);
  assert.equal(M.placeExclude('rapor !under:/tmp', PLACE_HOME, '/srv')[0], 'rapor !under:/tmp !under:/srv');
  assert.equal(M.placeExclude('rapor', PLACE_HOME, '/srv')[0], '!under:/srv rapor');
});

test('Exclude toggles its own term and takes a held folder out (lens/folders_tests.rs)', () => {
  const M = placeModel();
  const H = PLACE_HOME;
  // exclude_writes_its_own_term_and_takes_it_back
  const g = 'rapor under:/mnt/depo|under:/usr';
  const [out, at] = M.placeExclude(g, H, '/home/hasan/x y');
  assert.equal(out, 'rapor under:/mnt/depo|under:/usr !under:"~/x y"');
  assert.equal(at, out.length);
  assert.deepEqual(J(M.placeExclude(out, H, '~/x y/')), [g, g.length]);
  // excluding_a_held_folder_takes_it_out_of_the_group
  assert.equal(M.placeExclude(g, H, '/usr')[0], 'rapor under:/mnt/depo !under:/usr');
  assert.equal(M.placeExclude('under:~', H, '~')[0], '!under:~');
});

test('the caret settles, and the list shows the folder the value names (lens/folders_tests.rs)', () => {
  const M = placeModel();
  const H = PLACE_HOME;
  // the_caret_rests_on_the_group_or_an_empty_place: its empty places, through place_settle
  assert.deepEqual(J(M.placeSettle('under: rapor', 9, false)), ['under: rapor', 6]);
  assert.deepEqual(J(M.placeSettle('rapor ext:pdf', 6, false)), ['rapor under: ext:pdf', 12]);
  assert.deepEqual(J(M.placeSettle('rapor', 99, false)), ['rapor under:', 12]);
  assert.deepEqual(J(M.placeSettle('rapor ext:pdf', 8, false)), ['rapor ext:pdf under:', 20], 'inside a term, after it');
  // the_caret_settles_at_the_groups_end_or_stays_on_an_exclusion
  let q = 'rapor under:/a|under:/b ext:pdf';
  assert.equal(q.slice(0, M.placeSettle(q, 6, false)[1]), 'rapor under:/a|under:/b');
  q = 'rapor under:/a !under:/x !under:/y';
  assert.equal(q.slice(0, M.placeSettle(q, 25, true)[1]), q, 'on the exclusion it was on');
  assert.equal(q.slice(0, M.placeSettle(q, 15, true)[1]), 'rapor under:/a !under:/x');
  assert.equal(q.slice(0, M.placeSettle(q, 25, false)[1]), 'rapor under:/a');
  assert.equal(M.placeSettle('rapor under:/a', 6, true)[1], 14);
  assert.deepEqual(J(M.placeSettle('rapor ext:pdf', 6, true)), ['rapor under: ext:pdf', 12]);
  assert.equal(M.placeSettle('rapor under:/a|under: x', 0, false)[1], 21);
  // the_list_shows_the_folder_the_value_names
  assert.deepEqual(J(M.placeAt('~/Bel', H)), [H, 'Bel']);
  assert.deepEqual(J(M.placeAt('', H)), [H, '']);
  assert.deepEqual(J(M.placeAt('', '')), ['/', '']);
  assert.deepEqual(J(M.placeAt('/mnt/depo/', H)), ['/mnt/depo', '']);
  assert.deepEqual(J(M.placeAt('/usr', H)), ['/', 'usr']);
});

test('without_places leaves no bar in any arrangement (lens/folders_tests.rs)', () => {
  const M = placeModel();
  const cases = [
    ['rapor under:/a|under:/b', 'rapor'],
    ['under:/a|under:/b rapor', 'rapor'],
    ['rapor under:/a | under:/b ext:pdf', 'rapor ext:pdf'],
    ['rapor|fatura under:/a|under:/b ext:pdf|ext:odt', 'rapor|fatura ext:pdf|ext:odt'],
    ['under:/a|ext:pdf rapor', 'rapor'],
    ['rapor !under:/a|under:/b', 'rapor'],
    ['rapor under: ext:pdf', 'rapor ext:pdf'],
    ['rapor parent:~/x !under:/tmp', 'rapor !under:/tmp'],
    ['under:"/a b|c" "x y"', '"x y"'],
    [SYSTEM + ' rapor under:/mnt/depo', 'rapor'],
    ['rapor', 'rapor'],
    ['', ''],
  ];
  for (const [q, want] of cases) {
    const got = M.withoutPlaces(q);
    assert.equal(got, want, q);
    assert.ok(!got.trim().startsWith('|') && !got.trim().endsWith('|'), q);
  }
});

test('where the query looks: a disk held whole is one entry, and each goes alone (lens/spots_tests.rs)', () => {
  const M = placeModel();
  const H = PLACE_HOME;
  const tiles = ruleMachine(M);
  const names = (q) => J(M.spots(q, H, tiles).map((s) => [s.name, s.not]));
  // a_disk_held_whole_is_one_entry
  const q = 'rapor ' + SYSTEM + '|under:/mnt/depo';
  const s = M.spots(q, H, tiles);
  assert.equal(s.length, 2);
  assert.deepEqual([s[0].name, s[0].kind, s[0].tile], ['CACHY', 'system', 0]);
  assert.equal(s[0].terms.length, 5);
  assert.deepEqual([s[1].name, s[1].tile], ['Depo', 1]);
  assert.equal(M.spotRemove(s[0], q)[0], 'rapor under:/mnt/depo');
  assert.equal(M.spotRemove(s[1], q)[0], 'rapor ' + SYSTEM);
  // a_disk_held_in_part_is_its_folders
  assert.deepEqual(names('rapor under:~|under:/usr|under:/mnt/depo/PROJELER !under:~/tmp'),
    [['~', false], ['/usr', false], ['/mnt/depo/PROJELER', false], ['~/tmp', true]]);
  // entries_follow_the_text_and_each_goes_alone
  const e = 'under:"/mnt/depo/My Docs"|under:/mnt/depo !under:/mnt/depo/tmp rapor';
  const es = M.spots(e, H, tiles);
  assert.deepEqual(J(es.map((x) => x.name)), ['/mnt/depo/My Docs', 'Depo', '/mnt/depo/tmp']);
  assert.equal(es[0].short, 'My Docs');
  assert.equal(M.spotRemove(es[0], e)[0], 'under:/mnt/depo !under:/mnt/depo/tmp rapor');
  assert.equal(M.spotRemove(es[1], e)[0], 'under:"/mnt/depo/My Docs" !under:/mnt/depo/tmp rapor');
  assert.equal(M.spotRemove(es[2], e)[0], 'under:"/mnt/depo/My Docs"|under:/mnt/depo rapor');
});

test('the chip says names cut at max, and lights the disks the query can reach (lens/spots_tests.rs)', () => {
  const M = placeModel();
  const H = PLACE_HOME;
  const tiles = ruleMachine(M);
  // the_chip_says_names_and_cuts_at_max
  const say = (q, max) => M.chipLabel(M.spots(q, H, tiles), max, 'Everywhere');
  assert.equal(say('rapor', 3), 'Everywhere');
  assert.equal(say('rapor !under:/tmp', 3), 'Everywhere');
  assert.equal(say(SYSTEM + '|under:/mnt/depo', 3), 'CACHY + Depo');
  assert.equal(say('under:~|under:/usr|under:/mnt/depo/PROJELER', 3), '~ + usr + PROJELER');
  assert.equal(say('under:~|under:/usr|under:/mnt/depo/PROJELER', 1), '~ +2');
  assert.equal(say('under:~/Belgeler|under:/', 0), 'Belgeler +1');
  assert.equal(say('under:/', 2), '/');
  // the_dots_light_the_disks_the_query_can_reach
  const dots = (q) => J(M.chipDots(tiles, M.placeScope(q, H)).map((d) => [d.kind, d.asking, d.lit]));
  assert.deepEqual(dots('rapor'), [['system', false, true], ['fixed', false, true], ['removable', true, false]]);
  assert.deepEqual(dots('under:/mnt/depo/x').slice(0, 2), [['system', false, false], ['fixed', false, true]]);
  assert.deepEqual(dots('under:/').slice(0, 2), [['system', false, true], ['fixed', false, true]], 'a folder above a disk searches it');
  assert.ok(M.tileHeld(tiles[1], M.placeScope('under:/mnt/depo/', H)));
  assert.ok(!M.tileHeld(tiles[0], M.placeScope('under:~|under:/usr', H)));
  assert.ok(M.tileHeld(tiles[0], M.placeScope(SYSTEM, H)));
});

/* `capsule_tests.rs`'s `said`: each part as its tile's name, its crumbs joined, `[n]` when raw. */
function capSaid(cap) {
  return J(cap.parts.map((p) => (p.raw ? '[' + p.spot.terms.length + ']'
    : p.spot.tile !== null ? p.spot.name : p.crumbs.map((c) => c.label).join(' › '))));
}

test('a run of under: terms is one capsule of parts (lens/capsule_tests.rs)', () => {
  const M = placeModel();
  const H = PLACE_HOME;
  const tiles = ruleMachine(M);
  const caps = (q, caret) => M.placeCapsAt(q, H, tiles, caret === null ? null : M.caretResting(caret));
  // the_system_disk_is_one_part_of_one_capsule
  let q = 'rapor ' + SYSTEM + ' ext:pdf';
  let c = caps(q, null);
  assert.equal(c.length, 1);
  assert.equal(q.slice(c[0].start, c[0].end), SYSTEM);
  assert.deepEqual(capSaid(c[0]), ['CACHY']);
  assert.equal(c[0].parts[0].spot.kind, 'system');
  assert.equal(c[0].parts[0].spot.terms.length, 5);
  // folders_are_crumbs_in_tilde_form
  q = 'under:~/Projeler/Scour|under:"/mnt/depo/My Docs"|under:/ rapor';
  const f = caps(q, null)[0];
  assert.deepEqual(capSaid(f), ['~ › Projeler › Scour', 'Depo › My Docs', '/']);
  assert.deepEqual(J(f.parts[0].crumbs[2]), { label: 'Scour', path: '/home/hasan/Projeler/Scour' });
  assert.equal(f.parts[0].crumbs[0].path, H);
  assert.deepEqual(J(f.parts[1].crumbs[0]), { label: 'Depo', path: '/mnt/depo' });
  assert.deepEqual(J(f.parts.map((p) => p.disk === null ? null : tiles[p.disk].name)), ['CACHY', 'Depo', 'CACHY']);
  assert.equal(M.capDisk(f), null, 'three parts on two disks');
  // a_capsule_on_one_disk_names_it_and_a_folder_from_root_its_disk
  let one = caps('under:/mnt/depo/PROJELER/Scour rapor', null)[0];
  assert.deepEqual(capSaid(one), ['Depo › PROJELER › Scour']);
  assert.equal(tiles[M.capDisk(one)].name, 'Depo');
  one = caps('under:/srv/www|under:~/x', null)[0];
  assert.deepEqual(capSaid(one), ['CACHY › srv › www', '~ › x']);
  assert.equal(tiles[M.capDisk(one)].name, 'CACHY');
  one = caps('under:/mnt/depo/a|under:', null)[0];
  assert.equal(tiles[M.capDisk(one)].name, 'Depo', 'an empty member keeps the header');
  // a_mixed_or_negated_run_is_not_one_capsule
  assert.deepEqual(J(caps('under:/a|ext:pdf', null)), []);
  assert.deepEqual(J(caps('!under:/a|under:/b', null)), []);
  assert.deepEqual(J(caps('under:/a|parent:/b', null)), []);
  c = caps('under:/a !under:/b under:/mnt/depo|under:/c', null);
  assert.deepEqual(J(c.map((x) => [x.start, x.end])), [[0, 8], [19, 43]]);
  assert.deepEqual(capSaid(c[1]), ['Depo', 'CACHY › c']);
  // an_empty_member_is_raw
  const e = caps('rapor under:/mnt/depo|under:', null)[0];
  assert.deepEqual(capSaid(e), ['Depo', '[1]']);
  assert.deepEqual(J(e.parts[1].crumbs), []);
});

test('a resting caret leaves a disk whole, an edited member comes apart (lens/capsule_tests.rs)', () => {
  const M = placeModel();
  const tiles = ruleMachine(M);
  const at = (q, caret) => capSaid(M.placeCapsAt(q, PLACE_HOME, tiles, caret)[0]);
  // a_caret_resting_on_a_tile_leaves_it_whole
  let q = SYSTEM + '|under:/mnt/depo';
  const first = 'under:/etc'.length;
  const c = M.placeCapsAt(q, PLACE_HOME, tiles, M.caretResting(first))[0];
  assert.deepEqual(capSaid(c), ['CACHY', 'Depo']);
  assert.ok(c.parts[0].caret && !c.parts[0].raw);
  assert.deepEqual(at(q, M.caretResting(q.length)), ['CACHY', 'Depo']);
  assert.deepEqual(at(q, M.caretResting(0)), ['CACHY', 'Depo']);
  // a_member_being_edited_is_raw_and_its_tile_comes_apart
  const tilde = q.indexOf('~');
  const apart = ['CACHY › etc', '[1]', 'CACHY › opt', 'CACHY › usr', 'CACHY › var', 'Depo'];
  assert.deepEqual(at(q, M.caretResting(tilde)), apart, 'strictly inside, however it got there');
  assert.deepEqual(at(q, M.caretEditing(tilde + 1)), apart);
  assert.deepEqual(at(q, M.caretEditing(q.length)), ['CACHY', '[1]']);
  q = 'rapor under:~/Belgeler';
  assert.deepEqual(at(q, M.caretResting(q.length)), ['~ › Belgeler'], 'a folder at rest is crumbs');
  assert.deepEqual(at(q, M.caretEditing(q.length)), ['[1]'], 'typed into, its letters');
});

test('every offset of a run has a part, and folding keeps the caret part (lens/capsule_tests.rs)', () => {
  const M = placeModel();
  const tiles = ruleMachine(M);
  const caps = (q, caret) => M.placeCapsAt(q, PLACE_HOME, tiles, caret === null ? null : M.caretResting(caret));
  // every_offset_of_the_run_has_a_part
  const q = 'rapor ' + SYSTEM + '|under:/mnt/depo|under:~/x';
  const c = caps(q, null)[0];
  assert.deepEqual(capSaid(c), ['CACHY', 'Depo', '~ › x']);
  assert.equal(M.capPartAt(c, c.start - 1), null);
  assert.equal(M.capPartAt(c, c.end + 1), null);
  assert.equal(M.capPartAt(c, c.start), 0);
  const depo = q.indexOf('under:/mnt');
  assert.equal(M.capPartAt(c, depo), 0, 'the | before Depo is drawn after CACHY');
  assert.equal(M.capPartAt(c, depo + 1), 1);
  assert.equal(M.capPartAt(c, c.end), 2);
  for (let at = c.start + 1; at <= c.end; at++) {
    const k = M.capPartAt(c, at);
    const raw = caps(q, at)[0].parts.find((p) => p.raw);
    if (!raw) continue;
    const t = raw.spot.terms[0];
    assert.ok(t.start < at && at <= t.end, String(at));
    assert.ok(c.parts[k].spot.terms.some((x) => x.start === t.start && x.end === t.end), String(at));
  }
  // folding_keeps_the_caret_part_and_counts_the_rest
  const five = 'under:/a|under:/b|under:/c|under:/d|under:/e';
  let fc = caps(five, null)[0];
  M.capFold(fc, 2);
  assert.deepEqual(J([fc.parts.map((p) => p.shown), fc.hidden]), [[true, true, false, false, false], 3]);
  fc = caps(five, five.length)[0];
  M.capFold(fc, 2);
  assert.deepEqual(J([fc.parts.map((p) => p.shown), fc.hidden]), [[true, false, false, false, true], 3]);
  M.capFold(fc, 9);
  assert.equal(fc.hidden, 0);
  M.capFold(fc, 0);
  assert.equal(fc.hidden, 4);
});

test('the arrows stop once a collapsed part, at every letter of a raw one (lens/caret_tests.rs)', () => {
  const M = placeModel();
  const tiles = ruleMachine(M);
  const cap = (q, caret) => M.placeCapsAt(q, PLACE_HOME, tiles, caret)[0];
  const walk = (c, q, forward) => {
    let at = forward ? c.start : c.end;
    const out = [at];
    for (let next; (next = M.capStep(c, q, at, forward)) !== null; at = next) out.push(next);
    return out;
  };
  const ends = (q) => M.placeTerms(q).map((t) => t.end);
  // the_arrows_cross_a_collapsed_disk_in_one_step
  let q = 'rapor ' + SYSTEM + '|under:/mnt/depo ext:pdf';
  let c = cap(q, M.caretResting(6 + 'under:/etc'.length));
  const varEnd = q.indexOf('|under:/mnt');
  assert.deepEqual(walk(c, q, true), [6, varEnd, c.end]);
  assert.deepEqual(walk(c, q, false), [c.end, varEnd, 6]);
  assert.equal(M.capStep(c, q, c.end, true), null, '→ at the end leaves the capsule');
  assert.equal(M.capStep(c, q, 3, true), null, 'outside it, the face moves');
  // a_block_of_hidden_parts_is_one_stop
  q = 'under:/a|under:/b|under:/c|under:/d|under:/e';
  const e = ends(q);
  c = cap(q, null);
  M.capFold(c, 2);
  assert.deepEqual(J(M.capBlocks(c)), [[2, 3]]);
  assert.deepEqual(walk(c, q, true), [0, e[0], e[1], e[4]]);
  c = cap(q, M.caretResting(e[2]));
  M.capFold(c, 2);
  assert.deepEqual(J(M.capBlocks(c)), [[1, 1], [3, 2]]);
  assert.deepEqual(walk(c, q, true), [0, e[0], e[1], e[2], e[4]]);
  // a_raw_part_stops_at_every_letter_and_never_inside_a_collapsed_one (offsets in UTF-16 here)
  q = 'under:/mnt/depo|under:~/Müzik';
  c = cap(q, M.caretEditing(q.length));
  const raw = M.placeTerms(q)[1];
  const stops = J(M.capStops(c, q));
  const letters = [];
  for (let o = raw.start; o <= raw.end; o++) letters.push(o);
  assert.deepEqual(stops.slice(0, 2), [0, 'under:/mnt/depo'.length]);
  assert.deepEqual(stops.slice(2), letters);
  assert.ok(!stops.includes(3), "not inside Depo's letters");
  const u = q.indexOf('ü');
  assert.equal(M.capStep(c, q, u, true), u + 'ü'.length);
});

test('a lens opens by itself only where the person edits, and the edited term is said once (lens/caret_tests.rs)', () => {
  const M = placeModel();
  // a_lens_opens_by_itself_only_where_the_person_edits
  const rests = (q) => M.opensByItself(q, M.caretResting(q.length));
  const types = (q) => M.opensByItself(q, M.caretEditing(q.length));
  let q = 'rapor under:~/Belgeler';
  assert.ok(!rests(q) && types(q));
  assert.ok(M.opensByItself(q, M.caretResting(q.length - 1)), 'inside it');
  assert.ok(rests('rapor under:'), 'nothing chosen yet');
  assert.ok(!rests(SYSTEM) && !rests('rapor !under:/tmp'));
  assert.ok(types('rapor !under:/tmp'));
  assert.ok(M.opensByItself('!under:/a|under:/b', M.caretResting(9)), 'not a capsule: as before');
  assert.ok(M.opensByItself('under:/a|ext:pdf', M.caretResting(8)));
  assert.ok(rests('rapor dm:7d') && !rests('rapor'));
  // the_edited_term_is_said_once
  q = 'rapor under:~/Bel';
  assert.equal(M.editedTerm(q, M.caretEditing(q.length)).start, 6);
  assert.equal(M.editedTerm(q, M.caretResting(q.length)), null);
  assert.equal(M.editedTerm(q, M.caretResting(14)).start, 6);
  assert.ok(M.editedTerm('under:', M.caretResting(6)));
  assert.equal(M.editedTerm(q, M.caretEditing(6)), null, 'before its first letter');
});

test('Add lets only the member being edited give way, and every writer quotes | and ; (lens/folders_tests.rs)', () => {
  const M = placeModel();
  const H = PLACE_HOME;
  const at = (q, p, c) => M.placeAddAt(q, H, p, c)[0];
  // add_at_lets_only_the_member_being_edited_give_way
  let q = 'rapor under:/mnt/depo';
  assert.equal(at(q, '/usr', M.caretResting(q.length)), 'rapor under:/mnt/depo|under:/usr');
  q = 'rapor under:~/gel';
  assert.equal(at(q, '~/Belgeler', M.caretResting(q.length)), 'rapor under:~/gel|under:~/Belgeler');
  assert.equal(at(q, '~/Belgeler', M.caretEditing(q.length)), 'rapor under:~/Belgeler');
  q = 'rapor under:/mnt/depo';
  assert.equal(at(q, '/usr', M.caretEditing(q.length)), 'rapor under:/usr');
  assert.equal(at(q, '/usr', M.caretResting(14)), 'rapor under:/usr', 'inside it');
  q = 'rapor under:/a ext:pd';
  assert.equal(at(q, '/b', M.caretEditing(q.length)), 'rapor under:/a|under:/b ext:pd');
  assert.equal(at('rapor under:', '/b', M.caretResting(0)), 'rapor under:/b');
  // every_path_writer_quotes_a_bar_and_a_semicolon (no `lens::place` here; the page writes through quotedPath)
  for (const [path, written] of [['/srv/a;b', 'under:"/srv/a;b"'], ['/srv/a|b', 'under:"/srv/a|b"'], ['/srv/a b', 'under:"/srv/a b"']]) {
    assert.equal(M.placeAddWith('', H, path, null)[0], written);
    assert.equal(M.placeExclude('', H, path)[0], '!' + written);
    assert.equal('under:' + M.quotedPath(path), written);
    assert.equal(M.placeScope(written, H).within[0].path, path, 'read back whole');
  }
});

test('a group turned round is one exclusion each, and back (lens/negate_tests.rs)', () => {
  const M = placeModel();
  const NONE_OF = '!under:/etc !under:~ !under:/opt !under:/usr !under:/var !under:/mnt/depo';
  // a_group_turned_round_is_one_exclusion_each_and_back
  const q = 'rapor ' + SYSTEM + '|under:/mnt/depo ext:pdf';
  const [out, at] = M.negatePlaces(q, 10);
  assert.equal(out, 'rapor ' + NONE_OF + ' ext:pdf');
  assert.equal(out.slice(0, at), 'rapor ' + NONE_OF);
  const [back, at2] = M.unnegatePlaces(out, out.length - 9);
  assert.equal(back, q);
  assert.equal(back.slice(0, at2), 'rapor ' + SYSTEM + '|under:/mnt/depo');
  // quotes_spaces_and_empty_members
  const sp = 'under:"/mnt/depo/My Docs" | under:~';
  const [neg] = M.negatePlaces(sp, 0);
  assert.equal(neg, '!under:"/mnt/depo/My Docs" !under:~');
  assert.equal(M.unnegatePlaces(neg, 0)[0], 'under:"/mnt/depo/My Docs"|under:~');
  assert.equal(M.negatePlaces('under:/a|under:', 0)[0], '!under:/a');
  assert.deepEqual(J(M.negatePlaces('rapor under:/a', 2)), ['rapor under:/a', 2]);
  assert.equal(M.negatePlaces('under:/a|ext:pdf', 0)[0], 'under:/a|ext:pdf');
  assert.equal(M.unnegatePlaces('!under:/a|!under:/b', 0)[0], '!under:/a|!under:/b');
  // exclusions_side_by_side_are_one_block
  const spans = (s) => J(M.exclusionBlocks(s).map((b) => b.map((t) => t.start)));
  assert.deepEqual(spans('!under:/a !under:/b rapor !under:/c'), [[0, 10], [26]]);
  assert.deepEqual(spans('!under:/a|under:/b'), []);
  assert.deepEqual(spans('!under:/a !ext:pdf !under:/b'), [[0], [19]]);
  // an_excluded_disk_is_one_part_of_one_negated_capsule
  const tiles = ruleMachine(M);
  const x = 'rapor ' + NONE_OF + ' !under:/tmp';
  let caps = M.exclusionCaps(x, PLACE_HOME, tiles, null);
  assert.equal(caps.length, 1);
  assert.deepEqual(J(caps[0].parts.map((p) => p.spot.name)), ['CACHY', 'Depo', '/tmp']);
  assert.ok(caps[0].parts.every((p) => p.spot.not));
  assert.equal(x.slice(caps[0].start, caps[0].end), NONE_OF + ' !under:/tmp');
  caps = M.exclusionCaps(x, PLACE_HOME, tiles, M.caretEditing(x.indexOf('~') + 1));
  assert.equal(caps[0].parts.length, 7, 'editing one root takes its disk apart');
  assert.ok(caps[0].parts[1].raw);
});

test('a spot leaves its own run, a disk left out whole is one entry, an empty member takes its bar (lens/spots_tests.rs, group_tests.rs)', () => {
  const M = placeModel();
  const H = PLACE_HOME;
  const tiles = ruleMachine(M);
  // a_spot_of_any_run_is_taken_out_of_its_own_run
  let q = 'under:/a rapor under:/mnt/depo|under:/c';
  const caps = M.placeCapsAt(q, H, tiles, null);
  const depo = caps[1].parts[0].spot;
  assert.equal(depo.name, 'Depo');
  assert.deepEqual(J(M.spotRemove(depo, q)), ['under:/a rapor under:/c', 15]);
  assert.equal(M.spotRemove(caps[1].parts[1].spot, q)[0], 'under:/a rapor under:/mnt/depo');
  // a_disk_left_out_whole_is_one_negated_entry
  q = 'rapor !under:/etc !under:~ !under:/opt !under:/usr !under:/var !under:/mnt/depo/tmp';
  const s = M.spots(q, H, tiles);
  assert.deepEqual(J(s.map((x) => [x.name, x.not])), [['CACHY', true], ['/mnt/depo/tmp', true]]);
  assert.deepEqual(J(M.spotRemove(s[0], q)), ['rapor !under:/mnt/depo/tmp', 6]);
  assert.equal(M.chipLabel(s, 3, 'Everywhere'), 'Everywhere');
  // dropping_an_empty_member_takes_its_bar_too
  for (const [text, at, want] of [
    ['rapor under:/a|under: ext:pdf', 2, 'rapor under:/a ext:pdf'],
    ['rapor under:|under:/a', 1, 'rapor under:/a'],
    ['rapor under: | under:/a', 1, 'rapor under:/a'],
    ['rapor under: ext:pdf', 1, 'rapor ext:pdf'],
  ]) {
    const t = M.placeTerms(text)[at];
    assert.equal(M.placeReplace(text, t, '')[0], want, text);
    assert.equal(M.placeReplace(text, t, '')[0].trim(), want, text);
  }
});

// Page only: what the scope chip is drawn from.
test('the scope chip is drawn from the spots and the dots of the volumes answer', () => {
  const M = placeModel();
  const wire = ([disks, sources]) => [disks.map((d) => ({
    volume: { id: d.id, label: d.label, kind: d.kind, fs: d.fs, roots: d.roots, size: d.size },
    state: d.state, seen: { live: d.live, walking: d.walking },
  })), sources];
  const [disks, sources] = wire(fixMachine());
  const says = (q) => J(M.scopeSays(q, PLACE_HOME, disks, sources));
  const label = (q) => M.chipLabel(M.scopeSays(q, PLACE_HOME, disks, sources).spots, 9, 'Everywhere');
  const all = says('rapor');
  assert.deepEqual(all.dots.map((d) => d.kind), ['system', 'fixed', 'removable', 'network']);
  assert.deepEqual(all.dots.map((d) => d.asking), [false, false, true, false], 'a ringed dot for the stick');
  assert.equal(label('rapor'), 'Everywhere');
  assert.deepEqual(says('rapor under:/mnt/depo').dots.map((d) => d.lit), [false, true, false, false]);
  assert.equal(label('rapor under:/etc|under:~|under:/opt|under:/usr|under:/var|under:/mnt/depo'), 'CACHY + Depo');
  assert.equal(label('rapor under:~/Belgeler/'), 'Belgeler');
  assert.equal(label('rapor !under:/mnt/depo'), 'Everywhere', 'an exclusion is still everywhere else');
});

/* The page's place capsule, drawn and edited: a context with the model and
   the owner's machine as the `volumes` and `sources` answers. */
function capContext(value, caret) {
  const q = { value, selectionStart: caret, selectionEnd: caret, addEventListener() {} };
  const c = context();
  Object.assign(c, { q, HOME: PLACE_HOME, T: (id) => id });
  vm.runInContext(section('  const escape_ = (s) =>', '\n  const SIZE_RE'), c);
  vm.runInContext(section('  function fieldSplit(t) {', '\n  /* Terms and the whitespace'), c);
  vm.runInContext(section('  const LENS_FIELDS = {', '\n  const LENS_TIMES'), c);
  vm.runInContext(section('  const homeForm = (p) =>', '\n  function lensClose()'), c);
  vm.runInContext(section('  /* ---------------- the terms', '\n  /* ---------------- '), c);
  vm.runInContext(section('  /* ---------------- the place model', '\n  /* ---------------- the '), c);
  vm.runInContext(section('  /* ---------------- the caret over a place capsule', '\n  /* ---------------- '), c);
  vm.runInContext(section('  /* ---------------- the place rules', '\n  /* ---------------- '), c);
  vm.runInContext(section('  /* ---------------- where the query looks', '\n  /* ---------------- '), c);
  vm.runInContext(section('  function capHint(raw) {', '\n  /* A filter with nothing after'), c);
  vm.runInContext(section('  function capHeadOf(text, a, b) {', '\n  /* The canonical name'), c);
  vm.runInContext(section('  /* ---------------- the place capsule', '\n  /* Whether a row'), c);
  vm.runInContext(section('  const DISK_ART = {', '\n\n  /* A place lens opens'), c);
  const disk = (id, label, kind, roots, state) => ({ volume: { id, label, kind, fs: 'fs', roots, size: 1 }, state, seen: {} });
  c.PLACE_DISKS = [disk('cachy', 'CACHY', 'system', ['/'], 'manual'), disk('depo', 'Depo', 'fixed', ['/mnt/depo'], 'configured')];
  c.PLACE_SOURCES = [{ name: 'home', roots: [PLACE_HOME] }, { name: 'sistem', roots: ['/usr', '/etc', '/opt', '/var'] },
    { name: 'depo', roots: ['/mnt/depo'] }];
  return c;
}

/* The text of drawn HTML outside every `.art`: what the layer holds as letters. */
function lettersOf(html) {
  const stack = [];
  let out = '';
  for (const m of html.matchAll(/<(\/?)(\w+)([^>]*)>|([^<]+)/g)) {
    if (m[4] !== undefined) { if (!stack.includes(true)) out += m[4]; continue; }
    if (m[2] === 'svg' || m[2] === 'path' || m[2] === 'circle' || m[2] === 'rect') {
      if (m[1]) stack.pop(); else if (!m[3].endsWith('/')) stack.push(true);
      continue;
    }
    if (m[1]) stack.pop(); else stack.push(/class="[^"]*\bart\b/.test(m[3]));
  }
  return out.replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&quot;/g, '"').replace(/&#39;/g, "'").replace(/&amp;/g, '&');
}

test('the Location capsule keeps every letter of its run, and says the disks and folders it holds', () => {
  const run = SYSTEM + '|under:/mnt/depo|under:~/Projeler/Scour';
  const c = capContext('rapor ' + run, null);
  const [cap] = vm.runInContext('placeCapsDrawn(q.value, null)', c);
  const html = c.placeCapHtml(c.q.value, cap, -1);
  assert.equal(lettersOf(html), run, 'the run, letter for letter, in order');
  assert.match(html, /class="art pv"[^>]*><i class="pdot" style="background:var\(--disk-system\)"><\/i>CACHY<\/span>/);
  assert.match(html, />Depo<\/span>/);
  assert.match(html, /<span class="pv">~<\/span><i class="cs">›<\/i><span class="pv">Projeler<\/span><i class="cs">›<\/i><span class="pv"><b>Scour<\/b><\/span>/,
    'each step its own chip');
  assert.doesNotMatch(html, /class="hst/, 'on several disks the header is a folder, with no state');
  assert.equal((html.match(/class="art psep"/g) || []).length, 2, 'a quiet · between the parts');
  assert.equal((html.match(/class="term cap/g) || []).length, 1, 'one capsule');
  // Folded: the letters stay, the rest is "+N".
  c.capFold(cap, 1);
  const folded = c.placeCapHtml(c.q.value, cap, -1);
  assert.equal(lettersOf(folded), run);
  assert.match(folded, /class="art pv more" data-blk="1" data-at="\d+">\+2<\/span>/, 'one +N for the hidden block');
  // The caret in `~`: its letters as typed, in the well; the rest collapsed.
  const at = c.q.value.indexOf('~') + 1;
  const [edit] = vm.runInContext('placeCapsDrawn(q.value, caretEditing(' + at + '))', c);
  const raw = c.placeCapHtml(c.q.value, edit, at);
  assert.equal(lettersOf(raw), run);
  assert.match(raw, /class="term cap place edit"/);
  assert.match(raw, /<span class="pw" data-k="\d" data-hint="which folder\?"><span class="r-value">~<\/span><\/span>/);
  assert.match(raw, /<b>etc<\/b>/, 'editing a root takes its disk apart, as the model says');
  assert.match(raw, />Depo<\/span>/);
  // On one disk: steps from the disk's name, and the header that disk's
  // picture with its state after the name.
  const one = capContext('rapor under:/mnt/depo/PROJELER/Scour', null);
  const [oc] = vm.runInContext('placeCapsDrawn(q.value, null)', one);
  const oh = one.placeCapHtml(one.q.value, oc, -1);
  assert.match(oh, /<span class="nm">Location<\/span><i class="hst [a-z]+"><\/i>/);
  assert.match(oh, /<span class="pv">Depo<\/span><i class="cs">›<\/i><span class="pv">PROJELER<\/span><i class="cs">›<\/i><span class="pv"><b>Scour<\/b><\/span>/);
  assert.doesNotMatch(oh, /M2\.5 13V5\.5/, 'not the folder');
});

test('Backspace and Delete at a collapsed part take the whole part; typing there starts a word after', () => {
  const run = 'rapor ' + SYSTEM + '|under:/mnt/depo ext:pdf';
  const key = (caret, back) => { const c = capContext(run, caret); return c.placeCapKey(back); };
  const tilde = run.indexOf('under:~');
  assert.equal(key(tilde, true)[0], 'rapor under:/mnt/depo ext:pdf', 'Backspace after the | takes the disk');
  const depo = run.indexOf('under:/mnt');
  assert.equal(key(depo, true)[0], 'rapor under:/mnt/depo ext:pdf', 'Backspace before Depo takes the disk on its left');
  assert.equal(key(depo, false)[0], 'rapor ' + SYSTEM + ' ext:pdf', 'Delete before Depo takes Depo');
  assert.equal(key(6, false)[0], 'rapor under:/mnt/depo ext:pdf', 'Delete at the start takes the first part');
  assert.equal(key(6, true), null, 'Backspace at the start is the text\'s own');
  assert.equal(key(depo + 9, true), null, 'inside a member the letters are edited');
  assert.equal(key(depo - 1, false)[0], 'rapor ' + SYSTEM + ' ext:pdf', 'Delete at a member\'s end takes the next part, never the |');
  const empty = capContext('rapor under:/mnt/depo|under:', 28);
  assert.deepEqual(J(empty.placeCapKey(true)), ['rapor under:/mnt/depo', 21], 'an empty member goes with its |');
  const typed = capContext(run, tilde);
  assert.deepEqual(J(typed.placeCapType('x')), ['rapor ' + SYSTEM + '|under:/mnt/depo x ext:pdf', run.indexOf(' ext') + 2]);
  assert.equal(capContext(run, depo + 3).placeCapType('x'), null);
  assert.equal(key(depo - 1, true)[0], 'rapor under:/mnt/depo ext:pdf', 'Backspace at a disk\'s edge at rest takes the disk');
  assert.equal(capContext('rapor under:~/x ext:pdf', 15).placeCapType('y'), null, 'at a folder\'s edge, typing edits it');
});

// The time, type and size panels' model: `scour_ui::lens` `time.rs`,
// `time_pick.rs`, `time_said.rs`, `values.rs`, `size.rs`, `rest.rs` and
// `split.rs` in the page's words. Each case below is a case of the Rust
// tests named beside it, with the same inputs and the same answers.
function lensModel() {
  const c = context();
  Object.assign(c, {
    Date, Intl, DAY: 86400, LANG: 'en', fmt: (n) => String(n),
    T: (s, v) => (v ? s.replace(/\{(\w+)\}/g, (_, k) => v[k]) : s),
  });
  vm.runInContext(section('  const FIELDS = [', '\n  /* Two or more letters'), c);
  vm.runInContext(section('  function fieldSplit(t) {', '\n  /* Where a term is drawn'), c);
  vm.runInContext(section('  const LENS_FIELDS = {', '\n  /* Counting a query'), c);
  vm.runInContext(section('  const quoteIf = ', '\n  /* A path as a person'), c);
  for (const name of ['the terms', 'the place model', 'the place rules', 'the time in words', 'the time model', 'the type and size model']) {
    vm.runInContext(section('  /* ---------------- ' + name, '\n  /* ---------------- '), c);
  }
  return c;
}
const lensRun = (c, src) => J(vm.runInContext(src, c));
const DAYS = 86400;

test('the presets and the scales count what their terms find (lens/time_tests.rs)', () => {
  const M = lensModel();
  const run = (src) => lensRun(M, src);
  const band = (edges, age) => {
    const days = Math.floor(Math.max(age, 0) / DAYS);
    const i = edges.findIndex((e) => days <= e);
    return i < 0 ? edges.length : i;
  };
  // the_presets_are_the_lens_s_bands_with_their_edges
  assert.deepEqual(run('TIME_PRESETS.map((p) => [p.value, p.label])'), run('LENS_TIMES'));
  assert.deepEqual(run('TIME_PRESETS.map((p) => p.edge)'), [0, 1, 6, 29, 364]);
  // a_preset_counts_what_its_term_finds
  const edges = run('PRESET_EDGES');
  for (const p of run('TIME_PRESETS')) {
    const window = run(`timeRel(${JSON.stringify(p.value)}).secs`);
    for (let hours = 0; hours < 730 * 24; hours++) {
      const age = hours * 3600 + 1800;
      assert.equal(age <= window, band(edges, age) <= edges.indexOf(p.edge), `${p.value} at ${hours} h`);
    }
  }
  // today_is_the_first_band_alone_and_each_preset_holds_the_newer_ones
  assert.deepEqual(run('presetCounts([["1", 3], ["0", 5], ["older", 100], ["29", 7], ["364", 11]])'), [5, 8, 8, 15, 26]);
  assert.deepEqual(run('presetCounts([])'), [0, 0, 0, 0, 0]);
  assert.deepEqual(run('byEdge(PRESET_EDGES, [["6", 2], ["9", 50], ["older", 4]])'), [0, 0, 2, 0, 0, 4]);
  // each_scale_asks_its_bars_in_the_language_s_units
  assert.deepEqual(run('scaleEdges("day")'), Array.from({ length: 30 }, (_, i) => i));
  const at = (s, i) => run(`scaleEdges("${s}")`)[i];
  assert.deepEqual([run('scaleEdges("week")').length, at('week', 0), at('week', 1), at('week', 25)], [26, 6, 13, 181]);
  assert.deepEqual([run('scaleEdges("month")').length, at('month', 0), at('month', 23)], [24, 29, 719]);
  assert.deepEqual([run('scaleEdges("year")').length, at('year', 0), at('year', 9)], [10, 364, 3649]);
  for (const s of run('TIME_SCALES')) {
    const e = run(`scaleEdges("${s}")`);
    assert.equal(e.length, run(`TIME_SCALE.${s}.bars`), s);
    assert.ok(e.every((x, i) => i === 0 || e[i - 1] < x), `${s} not ascending`);
  }
  assert.deepEqual(run('TIME_SCALES.map((s) => TIME_SCALE[s].label)'), ['Day', 'Week', 'Month', 'Year']);
  assert.deepEqual(run('TIME_MODES.map((m) => MODE_LABEL[m])'), ['Between', 'Before', 'After']);
  // the_bars_run_oldest_first_and_the_last_ends_now
  const now = 1800000000;
  const h = run(`histogram("week", [["6", 9], ["13", 4], ["181", 2], ["older", 70]], ${now})`);
  assert.equal(h.bars.length, 26);
  assert.equal(h.bars[25].count, 9, 'the newest week is drawn last');
  assert.equal(h.bars[24].count, 4);
  assert.equal(h.bars[0].count, 2, 'the oldest week is drawn first');
  assert.deepEqual([h.older, h.peak], [70, 9]);
  assert.deepEqual([h.bars[25].from, h.bars[25].to], [now - 7 * DAYS, now]);
  assert.equal(h.bars[0].from, now - 26 * 7 * DAYS);
  assert.deepEqual([h.from, h.to], [now - 26 * 7 * DAYS, now]);
  assert.ok(h.bars.every((b, i) => i === 0 || h.bars[i - 1].to === b.from));
  assert.equal(run('TIME_SCALE.month.ends'), 'MonthYear');
  // a_bar_spans_the_ages_its_facet_band_counts
  for (const s of run('TIME_SCALES')) {
    const bars = run(`histogram("${s}", [], ${now}).bars`);
    bars.forEach((bar, i) => {
      assert.equal(band(run(`scaleEdges("${s}")`), now - (bar.from + bar.to) / 2), bars.length - 1 - i, `${s} bar ${i}`);
    });
  }
  // the_time_panel_counts_over_the_query_less_its_own_term
  assert.equal(run('timeAsk("rapor dm:7d ext:pdf")'), 'rapor ext:pdf');
  assert.equal(run('timeAsk("rapor !modified:7d|dm:1y kind:doc")'), 'rapor kind:doc');
});

test('a run of bars is written relative and reads back to itself (lens/time_pick_tests.rs)', () => {
  const M = lensModel();
  const run = (src) => lensRun(M, src);
  const sel = (scale, mode, first, last) => ({ scale, mode, first, last });
  const term = (q, s) => run(`timeTerm(placeTermAt(${JSON.stringify(q)}, ${q.length}), ${JSON.stringify(s)})`);
  const preset = (q, i) => run(`timePresetTerm(placeTermAt(${JSON.stringify(q)}, ${q.length}), ${i})`);
  const pick = (v) => run(`timePick(${JSON.stringify(v)})`);
  // a_run_reaching_now_is_a_window_and_one_before_it_a_band_older_first
  assert.equal(term('dm:', sel('day', 'between', 29, 29)), 'dm:1d');
  assert.equal(term('dm:', sel('day', 'between', 23, 29)), 'dm:7d');
  assert.equal(term('dm:', sel('day', 'between', 0, 22)), 'dm:30d..7d');
  assert.equal(term('dm:', sel('week', 'between', 24, 22)), 'dm:4w..1w');
  assert.equal(term('dm:', sel('year', 'between', 8, 8)), 'dm:2y..1y');
  assert.equal(term('dm:', sel('month', 'before', 0, 20)), 'dm:<3m');
  assert.equal(term('dm:', sel('week', 'after', 23, 25)), 'dm:>3w');
  assert.equal(term('dm:', sel('day', 'before', 10, 29)), '');
  assert.equal(term('dm:', sel('day', 'between', 0, 99)), 'dm:30d');
  // a_choice_keeps_the_negation_and_writes_the_canonical_field
  assert.equal(term('!modified:7d', sel('week', 'between', 24, 25)), '!dm:2w');
  assert.equal(preset('!dm:30d..7d', 2), '!dm:7d');
  assert.equal(preset('dm:', 0), 'dm:today');
  // every_run_reads_back_to_itself
  for (const scale of run('TIME_SCALES')) {
    const n = run(`TIME_SCALE.${scale}.bars`);
    for (let first = 0; first < n; first++) {
      for (let last = first; last < n; last++) {
        for (const [mode, a, b] of [['between', first, last], ['before', 0, last], ['after', first, n - 1]]) {
          const s = sel(scale, mode, a, b);
          const written = term('dm:', s);
          if (!written) continue;
          const back = run(`timePick(placeTermAt(${JSON.stringify(written)}, ${written.length}).value)`);
          assert.ok(back, written);
          assert.deepEqual(back.selected, s, written);
        }
      }
    }
  }
  // a_preset_reads_back_to_itself_and_to_its_bars
  run('TIME_PRESETS').forEach((p, i) => {
    const back = pick(p.value);
    assert.equal(back.preset, i, p.value);
    assert.ok(back.selected, p.value);
  });
  assert.deepEqual(pick('7d').selected, sel('day', 'between', 23, 29));
  assert.deepEqual(pick('1y').selected, sel('year', 'between', 9, 9));
  // other_spellings_of_the_same_choice_read_the_same
  const p = (v) => pick(v)?.preset ?? null;
  assert.deepEqual(['24h', 'bugün', '48saat', 'dün', '1w', 'hafta', '1m', '365d', '1yıl'].map(p), [0, 0, 1, 1, 2, 2, 3, 4, 4]);
  assert.equal(p('>7d'), null, 'After is not a preset');
  const bars = (v) => pick(v)?.selected ?? null;
  assert.deepEqual(bars('2hafta'), sel('week', 'between', 24, 25));
  assert.deepEqual(bars('14d'), sel('day', 'between', 16, 29));
  assert.deepEqual(bars('60d'), sel('month', 'between', 22, 23));
  assert.deepEqual(bars('30d..'), sel('day', 'between', 0, 29));
  assert.deepEqual(bars('..7d'), sel('day', 'before', 0, 22));
  assert.deepEqual(bars('<=7D'), sel('day', 'before', 0, 22));
  assert.deepEqual(bars('>=3w'), sel('week', 'after', 23, 25));
  // what_the_panel_cannot_show_is_none
  for (const v of ['', '2026-11-03', '2026-11-12..2026-11-25', '<2025-01-01', '7d..30d', '=7d', '12h', '31d..7d', '11y', '<10y', '<0d', '..', 'rapor']) {
    assert.equal(pick(v), null, v);
  }
  // a_negated_band_round_trips_through_the_panel
  for (const q of ['!dm:4w..1w', '!dm:<3m', '!dm:>2y', '!dm:7d']) {
    const back = run(`timePick(placeTermAt(${JSON.stringify(q)}, ${q.length}).value)`);
    assert.equal(back.preset !== null ? preset(q, back.preset) : term(q, back.selected), q);
  }
  // a_selection_names_the_bars_it_was_drawn_from
  const h = run('histogram("day", [["0", 1], ["6", 2], ["7", 4]], 0)');
  const s = pick('7d').selected;
  assert.equal(h.bars.slice(s.first, s.last + 1).reduce((n, b) => n + b.count, 0), 3, 'today and the seventh day, not the eighth');
});

test('a dm: value is said in words and dates, as little as is true (lens/time_said_tests.rs)', () => {
  const M = lensModel();
  const run = (src) => lensRun(M, src);
  const OFFSET = 3 * 3600;
  const day = (y, m, d) => run(`daysFromCivil(${y}, ${m}, ${d})`);
  const now = day(2026, 11, 25) * DAYS + 12 * 3600;
  const said = (v) => run(`timeSaid(${JSON.stringify(v)}, ${now}, ${OFFSET})`);
  const on = (y, m, d, form) => ({ day: day(y, m, d), form });
  // a_window_is_its_preset_or_a_count_of_its_unit
  assert.deepEqual(said('7d'), { label: 'last 7 days' });
  assert.deepEqual(said('24h'), { label: 'today' });
  assert.deepEqual(said('hafta'), { label: 'last 7 days' });
  assert.deepEqual(said('>1y'), { label: 'last year' });
  assert.deepEqual(said('30d..'), { label: 'last 30 days' });
  assert.deepEqual(said('3w'), { count: 'last {n} weeks', n: 3 });
  assert.deepEqual(said('2YIL'), { count: 'last {n} years', n: 2 });
  assert.deepEqual(said('12h'), { count: 'last {n} hours', n: 12 });
  assert.deepEqual(said('1h'), { count: 'last hour', n: 1 });
  // a_relative_band_is_said_as_the_days_it_covers_now
  assert.deepEqual(said('30d..7d'), { from: on(2026, 10, 26, 'DayMonth'), to: on(2026, 11, 18, 'DayMonth') });
  assert.deepEqual(said('13d..0d'), { from: on(2026, 11, 12, 'Day'), to: on(2026, 11, 25, 'DayMonth') }, '12–25 Nov');
  assert.deepEqual(said('<30d'), { bound: 'before {date}', date: on(2026, 10, 26, 'DayMonth') });
  assert.equal(said('7d..30d'), null, 'newer first holds nothing');
  // dates_say_as_little_as_is_true
  assert.deepEqual(said('2026-11-12..2026-11-25'), { from: on(2026, 11, 12, 'Day'), to: on(2026, 11, 25, 'DayMonth') });
  assert.deepEqual(said('2025-12-26..2026-01-03'), { from: on(2025, 12, 26, 'DayMonthYear'), to: on(2026, 1, 3, 'DayMonth') });
  assert.deepEqual(said('2025-02-03..2025-03-12'), { from: on(2025, 2, 3, 'DayMonth'), to: on(2025, 3, 12, 'DayMonthYear') });
  const alone = (y, m, d, f) => ({ date: on(y, m, d, f) });
  assert.deepEqual(said('2026-11-03'), alone(2026, 11, 3, 'DayMonth'));
  assert.deepEqual(said('=2024-11-03'), alone(2024, 11, 3, 'DayMonthYear'));
  assert.deepEqual(said('2026-03-01..2026-03-31'), alone(2026, 3, 1, 'MonthYear'));
  assert.deepEqual(said('2028-02-01..2028-02-29'), alone(2028, 2, 1, 'MonthYear'));
  assert.deepEqual(said('2025-01-01..2025-12-31'), alone(2025, 1, 1, 'Year'));
  assert.deepEqual(said('2026-11-03..2026-11-03'), alone(2026, 11, 3, 'DayMonth'));
  // a_bound_is_said_with_its_year_or_month_when_it_falls_on_one
  const b = (m, y, mo, d, f) => ({ bound: m, date: on(y, mo, d, f) });
  assert.deepEqual(said('<2025-01-01'), b('before {date}', 2025, 1, 1, 'Year'));
  assert.deepEqual(said('>=2026-03-01'), b('since {date}', 2026, 3, 1, 'MonthYear'));
  assert.deepEqual(said('2026-03-01..'), b('since {date}', 2026, 3, 1, 'MonthYear'));
  assert.deepEqual(said('>2024-12-31'), b('after {date}', 2024, 12, 31, 'Year'));
  assert.deepEqual(said('..2026-04-30'), b('until {date}', 2026, 4, 30, 'MonthYear'));
  assert.deepEqual(said('<=2026-11-03'), b('until {date}', 2026, 11, 3, 'DayMonth'));
  assert.deepEqual(said('>2025-01-01'), b('after {date}', 2025, 1, 1, 'DayMonthYear'));
  // what_is_not_a_time_says_nothing
  for (const v of ['', 'rapor', '2025', '2025-03', '..', '=7d', '2026-13-01', '7x']) assert.equal(said(v), null, v);
  assert.deepEqual(run(`civil(${day(2026, 11, 25)})`), [2026, 11, 25]);
  // The face's part: the same days in the page's language.
  const words = (v, lang) => { M.LANG = lang; return run(`timeWords(timeSaid(${JSON.stringify(v)}, ${now}, ${OFFSET}))`); };
  assert.equal(words('<2025-01-01', 'en'), 'before 2025');
  assert.equal(words('3w', 'en'), 'last 3 weeks');
  assert.match(words('13d..0d', 'tr'), /^12\s*[–-]\s*25 Kas$/);
  assert.match(words('2025-12-26..2026-01-03', 'tr'), /^26 Ara 2025\s–\s3 Oca$/);
  assert.equal(words('2026-03-01..2026-03-31', 'tr'), 'Mar 2026');
});

test('kind and ext values go in, come out and are left out on one side only (lens/values_tests.rs)', () => {
  const M = lensModel();
  const run = (src) => lensRun(M, src);
  const vt = (q, field, v, ex) => run(`valueToggle(${JSON.stringify(q)}, "${field}", ${JSON.stringify(v)}, ${ex})`);
  const toggle = (q, f, v) => vt(q, f, v, false)[0];
  const exclude = (q, f, v) => vt(q, f, v, true)[0];
  const picked = (q, f) => run(`picked(${JSON.stringify(q)}, "${f}")`);
  // the_tiles_are_the_kinds_in_their_order_with_the_core_s_msgids
  assert.deepEqual(run('KIND_LABELS.map(([k]) => k)'), run('KIND_VALUES'));
  assert.deepEqual(['doc', 'build', 'media'].map((k) => run(`kindMsgid("${k}")`)), ['Document', 'Build output', null]);
  // a_value_is_keyed_as_the_facet_keys_it
  const key = (f, v) => run(`valueKey("${f}", ${JSON.stringify(v)})`);
  assert.deepEqual([key('ext', '.PDF'), key('kind', 'belge'), key('kind', 'RESİM'), key('kind', 'çıktı'), key('kind', 'media'), key('kind', 'Code')],
    ['pdf', 'doc', 'image', 'build', 'media', 'code']);
  // toggling_a_value_puts_it_in_or_takes_it_out_of_the_one_term
  let q = toggle('rapor', 'kind', 'doc');
  assert.equal(q, 'rapor kind:doc');
  q = toggle(q, 'kind', 'code');
  assert.equal(q, 'rapor kind:doc;code');
  assert.equal(toggle(q, 'kind', 'doc'), 'rapor kind:code');
  assert.equal(toggle('rapor kind:doc', 'kind', 'doc'), 'rapor');
  assert.equal(toggle('kind:doc rapor', 'kind', 'doc'), 'rapor');
  assert.equal(toggle('', 'ext', 'pdf'), 'ext:pdf');
  assert.equal(toggle('rapor kind:', 'kind', 'doc'), 'rapor kind:doc');
  const [t2, caret] = vt('kind:doc rapor', 'kind', 'code', false);
  assert.deepEqual([t2.slice(0, caret), t2.slice(caret)], ['kind:doc;code', ' rapor']);
  // values_the_panel_did_not_offer_are_kept
  assert.equal(toggle('ext:pdf;xy', 'ext', 'docx'), 'ext:pdf;xy;docx');
  assert.equal(toggle('ext:pdf;xy', 'ext', 'pdf'), 'ext:xy');
  const values = [['pdf', '.pdf'], ['docx', '.docx']];
  const c = run(`listKept(placeTermAt("ext:pdf;xy", 10), ${JSON.stringify(values)})`);
  assert.deepEqual([c[0].term, c[0].hint], ['ext:xy', '✓']);
  assert.equal(c[1].term, 'ext:pdf;xy;docx');
  assert.ok(c.every((x) => x.stays));
  assert.equal(run(`listKept(placeTermAt("!ext:.PDF", 9), ${JSON.stringify(values)})`)[0].term, '!ext:');
  // excluding_is_a_term_of_its_own_and_a_value_is_on_one_side_only
  q = exclude('kind:doc', 'kind', 'build');
  assert.equal(q, 'kind:doc !kind:build');
  q = exclude(q, 'kind', 'exec');
  assert.equal(q, 'kind:doc !kind:build;exec');
  assert.equal(exclude(q, 'kind', 'build'), 'kind:doc !kind:exec');
  assert.equal(toggle(q, 'kind', 'build'), 'kind:doc;build !kind:exec');
  assert.equal(exclude('kind:doc;code', 'kind', 'code'), 'kind:doc !kind:code');
  assert.equal(exclude('kind:doc !kind:build', 'kind', 'build'), 'kind:doc');
  assert.equal(exclude('!kind:font|x', 'kind', 'build'), '!kind:font|x !kind:build');
  // Not in the Rust: a query ending in a space still puts one space before the new term.
  assert.equal(exclude('kind:doc rapor ', 'kind', 'build'), 'kind:doc rapor !kind:build');
  // the_query_says_what_is_in_and_what_is_out_whatever_its_spelling
  const p = picked('tür:belge;resim !tür:derleme ext:pdf', 'kind');
  assert.deepEqual([p.on, p.off], [['doc', 'image'], ['build']]);
  assert.deepEqual(picked('ext:.PDF ; odt', 'ext').on, ['pdf', 'odt']);
  assert.deepEqual(picked('rapor', 'ext'), { on: [], off: [] });
  assert.equal(toggle('tür:belge', 'kind', 'code'), 'tür:belge;code');
  assert.equal(toggle('tür:belge;code', 'kind', 'doc'), 'tür:code');
  assert.equal(toggle('ext:pdf ; odt', 'ext', 'odt'), 'ext:pdf');
  // a_value_leaving_an_or_run_leaves_no_bar_behind
  assert.equal(toggle('kind:doc|ext:pdf rapor', 'kind', 'doc'), 'ext:pdf rapor');
  assert.equal(toggle('kind:doc|ext:pdf', 'kind', 'code'), 'kind:doc;code|ext:pdf');
  // every_pick_round_trips
  for (const q0 of ['kind:doc;code !kind:build', '!ext:log;tmp', 'tür:doc ext:pdf;odt']) {
    for (const field of ['kind', 'ext']) {
      const pk = picked(q0, field);
      for (const [vals, ex] of [[pk.on, false], [pk.off, true]]) {
        let text = q0;
        for (const v of vals) text = vt(text, field, v, ex)[0];
        for (const v of vals) text = vt(text, field, v, ex)[0];
        assert.deepEqual(picked(text, field), pk, `${q0} ${field}`);
      }
    }
  }
  // the_kinds_are_counted_without_kinds_and_the_extensions_within_them
  let a = run('typeAsk("rapor kind:doc !kind:build ext:pdf")');
  assert.deepEqual(a, { kinds: 'rapor ext:pdf', exts: 'rapor kind:doc !kind:build', oneWalk: false });
  a = run('typeAsk("rapor kind:doc|ext:pdf dm:7d")');
  assert.deepEqual(a, { kinds: 'rapor dm:7d', exts: 'rapor dm:7d', oneWalk: true });
  assert.equal(run('typeAsk("tür:doc ; code rapor").kinds'), 'rapor');
});

test('the size presets, which one a term is, and their counts (lens/size_tests.rs)', () => {
  const M = lensModel();
  const run = (src) => lensRun(M, src);
  const t = (q) => `placeTermAt(${JSON.stringify(q)}, ${q.length})`;
  // the_presets_are_empty_then_the_lens_s_bands
  assert.deepEqual(run('SIZE_PRESETS[0]'), { label: 'Empty', term: 'empty:' });
  assert.deepEqual(run('SIZE_PRESETS.slice(1).map((p) => [p.term.replace("size:", ""), p.label])'), run('LENS_SIZES'));
  assert.equal(run('NOT_EMPTY'), '!empty:');
  // every_preset_round_trips_with_and_without_its_negation
  run('SIZE_PRESETS').forEach((p, i) => {
    for (const q of [p.term, '!' + p.term]) {
      assert.equal(run(`sizePresetOf(${JSON.stringify(q)}, ${t(q)})`), i, q);
      assert.equal(run(`sizePresetTerm(${t(q)}, ${i})`), q);
    }
  });
  assert.equal(run(`sizePresetTerm(${t('!empty:')}, 4)`), '!size:>1gb');
  assert.equal(run(`sizePresetTerm(${t('size:>1gb')}, 0)`), 'empty:');
  // other_spellings_read_and_strangers_do_not
  for (const [q, want] of [['bos:', 0], ['EMPTY:', 0], ['!bos:', 0], ['size:<100KB', 1], ['sizE:10MB..1GB', 3],
    ['size:>0', null], ['size:0', null], ['size:>2gb', null], ['empty:x', null], ['rapor', null]]) {
    assert.equal(run(`sizePresetOf(${JSON.stringify(q)}, ${t(q)})`), want, q);
  }
  // each_preset_is_counted_over_the_query_less_its_size_terms
  const c = run('sizeCounts("rapor size:>1gb dm:7d !empty:")');
  assert.deepEqual([c[0], c[4]], ['rapor dm:7d empty:', 'rapor dm:7d size:>1gb']);
  assert.equal(run('sizeCounts("")[2]'), 'size:100kb..10mb');
  assert.equal(run('sizeCounts("size:<1kb|rapor ext:pdf")[1]'), 'ext:pdf size:<100kb');
});

test('a list goes on across a spaced semicolon, and the drawn terms agree (lens/split_tests.rs)', () => {
  const M = lensModel();
  const terms = (q) => run(`placeTerms(${JSON.stringify(q)})`);
  const run = (src) => lensRun(M, src);
  const raw = (q) => terms(q).map((x) => q.slice(x.start, x.end));
  // a_list_goes_on_across_a_spaced_semicolon_as_the_service_reads_it
  for (const q of ['ext:pdf ; odt', 'ext:pdf ;odt', 'ext:pdf; odt', 'ext:pdf ; ; odt', 'ext:pdf; ;odt']) {
    const ts = terms(q);
    assert.equal(ts.length, 1, q);
    assert.deepEqual([ts[0].start, ts[0].end, ts[0].field], [0, q.length, 'ext'], q);
    assert.deepEqual(ts[0].value.split(';').filter(Boolean), ['pdf', 'odt'], q);
  }
  assert.deepEqual([terms('tür:doc ; code')[0].field, terms('tür:doc ; code')[0].value], ['kind', 'doc;code']);
  assert.deepEqual([terms('!ext:pdf ; odt')[0].not, terms('!ext:pdf ; odt')[0].value], [true, 'pdf;odt']);
  const cases = [
    ['ext:pdf;odt ; rs', ['ext:pdf;odt ; rs']],
    // a_list_ends_where_its_next_piece_cannot_be_a_value
    ['ext:pdf odt', ['ext:pdf', 'odt']], ['ext:pdf ; dm:7d', ['ext:pdf ;', 'dm:7d']], ['ext:pdf;dm:7d', ['ext:pdf', 'dm:7d']],
    ['kind:doc;!code', ['kind:doc', '!code']], ['ext:pdf ; !odt', ['ext:pdf ;', '!odt']], ['ext:pdf ; "odt"', ['ext:pdf ;', '"odt"']],
    ['ext:pdf ; odt ; dm:7d', ['ext:pdf ; odt ;', 'dm:7d']], ['ext: ; odt', ['ext: ; odt']],
    // a_bar_still_cuts_and_a_list_after_one_is_the_service_s
    ['ext:pdf ; odt|rs', ['ext:pdf ; odt', 'rs']], ['x|ext:pdf ; odt', ['x', 'ext:pdf', 'odt']],
    ['x | ext:pdf ; odt', ['x', 'ext:pdf ; odt']], ['ext:pdf|x ; odt', ['ext:pdf', 'x', 'odt']],
    // a_lone_semicolon_is_a_separator_and_other_words_are_as_they_were
    ['rapor ; fatura', ['rapor', 'fatura']], ['under:/a ; under:/b', ['under:/a', 'under:/b']],
    // a_semicolon_outside_a_list_cuts_as_the_service_cuts
    ['rapor;fatura', ['rapor', 'fatura']], ['under:/a;b', ['under:/a', 'b']],
    ['under:"/a;b" "x;y"', ['under:"/a;b"', '"x;y"']], ['under:/a;', ['under:/a']],
    ['under:/a;b|under:/c', ['under:/a', 'b', 'under:/c']], ['dm:7d;rapor', ['dm:7d', 'rapor']],
    ['fuzzy:ab;cd', ['fuzzy:ab', 'cd']], ['ext:pdf ; odt;rapor x;y', ['ext:pdf ; odt;rapor', 'x', 'y']],
    ['ext:pdf;dm:7d;x', ['ext:pdf', 'dm:7d', 'x']],
    ['a|!b "x ; y" under:"/m y"', ['a', '!b', '"x ; y"', 'under:"/m y"']], ['', []], [' ; ', []],
  ];
  for (const [q, want] of cases) assert.deepEqual(raw(q), want, q);
  assert.equal(terms('ext:pdf ; dm:7d')[1].field, 'dm');
  assert.equal(run('placeRuns("ext:pdf ; odt|rs").length'), 1, 'one run: (pdf or odt) or rs');
  assert.deepEqual(run('placeRuns("under:/a;b|under:/c").map((r) => r.length)'), [1, 2], 'under a, and (b or under c)');
  assert.equal(terms('rapor;fatura')[1].start, 6);
  // the_caret_in_a_spaced_list_is_in_that_list
  for (const c of [13, 14, 15, 19]) assert.equal(run(`placeTermAt("rapor ext:pdf ; odt", ${c}).field`), 'ext', String(c));
  // The capsules are cut the same way, but for two things the drawing does
  // on purpose: a list's trailing `;` stays outside its pill, and outside a
  // list a `;` reads as the space the service takes it for.
  const drawn = (q) => run(`terms(${JSON.stringify(q)})`).map(([a, b]) => q.slice(a, b));
  for (const [q, want] of cases) {
    assert.deepEqual(drawn(q), want.map((w) => w.replace(/[\s;]+$/, (m) => (/^ext:\s*$/.test(w) ? m : ''))), q);
  }
  assert.deepEqual(drawn('rapor;fatura'), ['rapor', 'fatura']);
});

test('a panel write rests where the same lens stays open (lens/settle_tests.rs)', () => {
  const M = lensModel();
  const run = (src) => lensRun(M, src);
  const t = (q, at) => `placeTermAt(${JSON.stringify(q)}, ${at})`;
  const shown = ([text, at]) => text.slice(0, at) + '|' + text.slice(at);
  // a_written_term_never_touches_its_neighbour
  const tog = (q, v, ex) => run(`valueToggle(${JSON.stringify(q)}, "kind", ${JSON.stringify(v)}, ${ex})`)[0];
  assert.equal(tog('kind:doc rapor ', 'build', true), 'kind:doc rapor !kind:build');
  assert.equal(tog('kind:doc rapor ', 'doc', true), 'rapor !kind:doc');
  assert.equal(tog('rapor', 'doc', false), 'rapor kind:doc');
  assert.equal(tog('rapor ', 'doc', false), 'rapor kind:doc');
  assert.equal(tog('', 'doc', false), 'kind:doc');
  assert.equal(tog('   ', 'doc', true), '!kind:doc');
  assert.equal(tog('kind:doc|ext:pdf rapor', 'doc', true), 'ext:pdf rapor !kind:doc');
  assert.equal(tog('a|kind:doc|b', 'doc', false), 'a|b');
  assert.equal(run('withTerm("rapor ", "size:>1gb")'), 'rapor size:>1gb');
  assert.equal(run('withTerm(" ", " ")'), '');
  // a_choice_rests_at_the_end_of_what_it_wrote
  const choice = (q, at, by, f) => shown(run(`choiceSettle(${JSON.stringify(q)}, ${t(q, at)}, ${JSON.stringify(by)}, "${f}")`));
  assert.equal(choice('dm:7d rapor', 5, 'dm:30d', 'dm'), 'dm:30d| rapor');
  assert.equal(choice('dm:7d|rapor', 5, 'dm:1y', 'dm'), 'dm:1y||rapor');
  const w = run(`sizePresetTerm(${t('rapor size:>1gb', 15)}, 0)`);
  assert.equal(choice('rapor size:>1gb', 15, w, 'size'), 'rapor empty:|');
  // a_choice_that_takes_its_term_out_leaves_an_empty_one_in_its_place
  const gone = (q, at) => choice(q, at, '', 'dm');
  assert.equal(gone('rapor dm:7d ', 11), 'rapor dm:|');
  assert.equal(gone('rapor dm:7d', 11), 'rapor dm:|');
  assert.equal(gone('dm:7d rapor', 5), 'dm:| rapor');
  assert.equal(gone('dm:7d|rapor', 5), 'dm:| rapor');
  assert.equal(gone('a|dm:7d|b', 7), 'a|b dm:|');
  assert.equal(gone('dm:7d', 5), 'dm:|');
  // a_toggle_rests_on_the_term_of_the_side_it_changed
  const s = (q, f, v, ex) => shown(run(`valueSettle(${JSON.stringify(q)}, "${f}", "${v}", ${ex})`));
  assert.equal(s('rapor', 'kind', 'doc', false), 'rapor kind:doc|');
  assert.equal(s('kind:doc rapor', 'kind', 'build', true), 'kind:doc rapor !kind:build|');
  assert.equal(s('kind:doc;code', 'kind', 'code', true), 'kind:doc !kind:code|');
  assert.equal(s('kind:doc rapor kind:code', 'kind', 'image', false), 'kind:doc;image| rapor kind:code');
  assert.equal(s('kind:doc !kind:build', 'kind', 'doc', false), '!kind:build|');
  assert.equal(s('kind:doc !kind:build', 'kind', 'build', true), 'kind:doc|');
  assert.equal(s('rapor kind:doc dm:7d', 'kind', 'doc', false), 'rapor kind:| dm:7d');
  assert.equal(s('kind:doc', 'kind', 'doc', false), 'kind:|');
  assert.equal(s('!ext:log', 'ext', 'log', true), 'ext:|');
  // an_extension_from_the_kind_tiles_moves_the_lens_to_its_row
  assert.equal(s('kind:doc', 'ext', 'pdf', false), 'kind:doc ext:pdf|');
  const [q1, c1] = run('valueSettle("kind:doc", "ext", "pdf", false)');
  assert.equal(run(`${t(q1, c1)}.field`), 'ext');
  const [q2, c2] = run('valueSettle("kind:doc", "kind", "build", true)');
  assert.equal(run(`${t(q2, c2)}.field`), 'kind');
});

test('empty: opens the size lens and says Empty (lens/size_tests.rs)', () => {
  const M = lensModel();
  M.HOME = '/home/u';
  vm.runInContext(section('  /* ---------------- the caret over a place capsule', '\n  /* ---------------- '), M);
  vm.runInContext(section('  const LAUNCH_HEADINGS = ', '\n  function launchHtml('), M);
  const run = (src) => lensRun(M, src);
  const t = (q) => `placeTermAt(${JSON.stringify(q)}, ${q.length})`;
  for (const q of ['empty:', 'bos:', 'BOS:', 'Empty:', '!empty:', 'rapor !bos:']) {
    assert.equal(run(`placeLensOf(${JSON.stringify(q)}, ${t(q)})`), 'size', q);
    assert.equal(run(`opensByItself(${JSON.stringify(q)}, { at: ${q.length}, editing: false })`), true, q);
    assert.equal(run(`sizeSaid(${JSON.stringify(q)}, ${t(q)})`), 'Empty', q);
  }
  assert.equal(run(`placeLensOf("boş:", ${t('boş:')})`), null);
  assert.equal(run(`sizeSaid("size:>1gb", ${t('size:>1gb')})`), 'over 1 GB');
  assert.equal(run(`sizeSaid("size:>0", ${t('size:>0')})`), null);
  assert.deepEqual(run('launchRows("rapor empty: :", ":").filter((r) => r.held).map((r) => r.field)'), ['size']);
  // The page's own reading of a term opens the same panel.
  vm.runInContext(section('  function lensKindOf(t) {', '\n  /* `lens::quoted`'), M);
  assert.equal(run('lensKindOf({ raw: "!bos:", named: true, field: LENS_FIELDS.bos, value: "" })'), 'size');
});

test('a value shows on every scale it lines up with, and a mode turns about the run (lens/time_pick_tests.rs)', () => {
  const M = lensModel();
  const run = (src) => lensRun(M, src);
  const sel = (scale, mode, first, last) => ({ scale, mode, first, last });
  const on = (v, s) => run(`timeBarsOn(${JSON.stringify(v)}, "${s}")`);
  // a_value_shows_on_every_scale_it_lines_up_with
  assert.deepEqual(on('7d', 'week'), sel('week', 'between', 25, 25));
  assert.deepEqual(on('7d', 'day'), sel('day', 'between', 23, 29));
  assert.equal(on('7d', 'month'), null);
  assert.deepEqual(on('14d', 'week'), sel('week', 'between', 24, 25));
  assert.equal(on('3d', 'week'), null);
  assert.deepEqual(on('1y', 'year'), sel('year', 'between', 9, 9));
  assert.equal(on('1y', 'day'), null, "past the day scale's 30 bars");
  assert.deepEqual(on('<2w', 'day'), sel('day', 'before', 0, 15));
  assert.deepEqual(on('4w..1w', 'day'), sel('day', 'between', 2, 22));
  assert.deepEqual(on('>1m', 'day'), sel('day', 'after', 0, 29));
  assert.equal(on('2026-11-03', 'day'), null);
  for (const v of ['7d', '4w..1w', '<3m', '>2y', '60d', 'today']) {
    const s = run(`timePick(${JSON.stringify(v)}).selected`);
    assert.deepEqual(on(v, s.scale), s, v);
  }
  // switching_the_mode_turns_about_the_run_s_older_edge
  const remode = (q, s, m) => run(`timeRemode(placeTermAt(${JSON.stringify(q)}, ${q.length}), ${JSON.stringify(s)}, "${m}")`);
  const r = sel('day', 'between', 20, 25);
  const [before, bt] = remode('dm:', r, 'before');
  assert.deepEqual([before, bt], [sel('day', 'before', 0, 19), 'dm:<10d']);
  const [after, at] = remode('dm:', r, 'after');
  assert.deepEqual([after, at], [sel('day', 'after', 20, 29), 'dm:>10d']);
  assert.deepEqual(remode('dm:', before, 'after')[0], after);
  assert.deepEqual(remode('dm:', after, 'before')[0], before);
  assert.deepEqual(remode('dm:', before, 'between'), [sel('day', 'between', 0, 19), 'dm:30d..10d']);
  assert.deepEqual(remode('dm:', r, 'between')[0], r);
  for (const [s] of [remode('dm:', r, 'before'), remode('dm:', r, 'after')]) {
    const term = run(`timeTerm(placeTermAt("dm:", 3), ${JSON.stringify(s)})`);
    assert.deepEqual(run(`timePick(placeTermAt(${JSON.stringify(term)}, ${term.length}).value).selected`), s, term);
  }
  // switching_the_mode_of_a_run_at_either_end_of_the_axis
  const now = sel('day', 'between', 23, 29);
  assert.equal(remode('!dm:', now, 'after')[1], '!dm:>7d');
  assert.equal(remode('!dm:', now, 'before')[1], '!dm:<7d');
  assert.equal(remode('!dm:', sel('day', 'between', 0, 5), 'before'), null, 'nothing older on the axis');
  const all = sel('day', 'before', 0, 29);
  assert.equal(run(`timeTerm(placeTermAt("!dm:", 4), ${JSON.stringify(all)})`), '', 'a Before covering every bar is no filter');
  assert.equal(remode('!dm:', all, 'after'), null, 'nothing newer than now');
});

// What a capsule does to the list: which query its effect is counted
// without, which capsules an empty list's rescue answer marks, and which
// `|` runs are framed.
function marksModel() {
  const M = lensModel();
  vm.runInContext(section('  /* ---------------- what a capsule does to the list', '\n  /* ---- drawn ---- */'), M);
  return M;
}

test('a capsule\'s effect is counted without it, a group without its whole run', () => {
  const M = marksModel();
  const w = (q, a, b) => M.effectWithout(q, a, b);
  assert.equal(w('rapor ext:pdf dm:7d', 6, 13), 'rapor dm:7d');
  assert.equal(w('rapor under:/a|under:/b ext:pdf', 6, 23), 'rapor ext:pdf');
  assert.equal(w('ext:pdf|dm:today rapor', 0, 7), 'dm:today rapor', 'its | goes with it');
  assert.equal(w('rapor !kind:build', 6, 17), 'rapor');
  assert.equal(w('kind:doc;code', 0, 13), '');
  assert.equal(w('rapor !under:/a !under:/b x', 6, 25), 'rapor x', 'a red block of exclusions, whole');
});

test('every capsule whose removal rescues an empty list is marked, none other', () => {
  const M = marksModel();
  const text = 'rapor ext:xlsx dm:today size:>1gb kind:doc';
  const rescue = (pairs) => ({ text, by: new Map(pairs.map(([t, n]) => [t, { term: t, n, capped: false }])) });
  const marked = (r, caps) => caps.filter(([a, b]) => M.rescueOf(text, a, b, r)).map(([a]) => a);
  const caps = [[6, 14], [15, 23], [24, 33], [34, 42]];
  assert.deepEqual(marked(rescue([['ext:xlsx', 312]]), caps), [6]);
  assert.deepEqual(marked(rescue([['ext:xlsx', 312], ['size:>1gb', 4]]), caps), [6, 24], 'two rescuers, both');
  assert.equal(M.rescueOf(text, 6, 14, rescue([['ext:xlsx', 312]])).n, 312);
  assert.deepEqual(marked(rescue([]), caps), [], 'a filter that was not asked about gets no mark');
  assert.equal(M.rescueOf(text + ' x', 6, 14, rescue([['ext:xlsx', 312]])), null, 'an answer for another text marks nothing');
  // A group capsule is marked when a member of it rescues.
  const g = 'a under:/x|under:/y';
  const r = { text: g, by: new Map([['under:/y', { n: 3 }]]) };
  assert.equal(M.rescueOf(g, 2, 19, r).n, 3);
});

test('terms joined by | are framed, unless one capsule draws them already', () => {
  const M = marksModel();
  const text = 'ext:pdf|dm:today rapor|fatura under:/a|under:/b x';
  const runs = (caps) => J(M.orRuns(text, caps)).map((r) => text.slice(r.start, r.end));
  assert.deepEqual(runs([]), ['ext:pdf|dm:today', 'rapor|fatura', 'under:/a|under:/b']);
  assert.deepEqual(runs([{ start: 30, end: 47 }]), ['ext:pdf|dm:today', 'rapor|fatura'], 'a Location capsule is its own frame');
  assert.deepEqual(J(M.orRuns('a | b', [])), [{ start: 0, end: 5, members: [0, 4] }]);
  assert.deepEqual(J(M.orRuns('ext:pdf ; odt', [])), [], 'a list is one term');
});
