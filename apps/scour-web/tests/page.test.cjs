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
