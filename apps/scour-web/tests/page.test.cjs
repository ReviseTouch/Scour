const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const { test } = require('node:test');
const vm = require('node:vm');

// Execute the shipped functions, with a deterministic clock and transport.
const html = readFileSync(process.env.SCOUR_PAGE || join(__dirname, '../src/page.html'), 'utf8');
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
