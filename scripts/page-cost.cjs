// Deterministic workload against the shipped page, or a saved earlier page.
// node scripts/page-cost.cjs [page.html]
const fs = require('node:fs');
const vm = require('node:vm');
const html = fs.readFileSync(process.argv[2] || 'apps/scour-web/src/page.html', 'utf8');
function section(from, to) {
  const a = html.indexOf(from), b = html.indexOf(to, a + from.length);
  if (a < 0 || b < 0) throw new Error(`missing section ${from}`);
  return html.slice(a, b);
}
function run() {
  const c = vm.createContext({
    Date, WINDOW: 200, FRESH_MS: 1800, AHEAD_WINDOWS: 2, KEEP_WINDOWS: 32,
    LIST: { rows: new Map(), seen: new Map(), at: new Map(), pending: new Map(), total: 20000 },
    arrivals: new Map(), fullPath: f => `${f.path}/${f.name}`, armFresh() {},
    reach: () => 20000, visibleRange: () => c.range, range: [0, 60],
  });
  vm.runInContext(section('  function putRows(start, rows, mark) {', '  /* **The one way to say how long the list is.**'), c);
  vm.runInContext(section('  function nextMissing(from, to) {', '\n  function fillWindow()'), c);
  function store(start) {
    const rows = Array.from({ length: 200 }, (_, i) => ({
      path: `/data/project-${Math.floor((start+i)/40)}/documents`, name: `report-${start+i}.pdf`,
      size: start+i, mtime: 1785000000, kind: 'document', ext: 'pdf', is_dir: false,
    }));
    c.putRows(start, rows, false);
    if (c.keepWindow) c.keepWindow(start, 1);
    else c.LIST.at.set(start, 1);
  }
  let requests = 0;
  for (let start; (start = c.nextMissing(0, 60)) !== null;) {
    store(start);
    if (++requests > 200) throw new Error('unbounded prefetch');
  }
  const stationary = { requests, rows: c.LIST.rows.size };
  for (let start = 0; start < 20000; start += 200) {
    c.range = [start, start+60]; store(start);
  }
  return { stationary, scrolling: { pages: c.LIST.at.size, rows: c.LIST.rows.size, paths: c.LIST.seen.size } };
}
console.log(JSON.stringify(run(), null, 2));
