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
  vm.runInContext(section('  function atMostEvery(fn, ms) {', '\n  /* The highlight marks'), c);
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
  vm.runInContext(section('  function putRows(start, rows, mark) {', '  /* **The one way to say how long the list is.**'), c);
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
  vm.runInContext(section('  function fillWindow() {', '\n  /* **One door into the painter'), c);
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
  vm.runInContext(section('  function fillWindow() {', '\n  /* **One door into the painter'), c);
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
