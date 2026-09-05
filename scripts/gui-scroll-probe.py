#!/usr/bin/env python3
"""Measure the native window with its built-in scroll/snapshot hooks (Linux).

Opens a real window for 13 seconds against the specified service. Run the
before and after release binaries separately on the same desktop and index.
The trace may contain indexed paths; keep its output local. Page latency is
not input-to-paint latency. Snapshot allocations are excluded from the sample.
"""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', type=Path)
    parser.add_argument('output', type=Path)
    parser.add_argument('--socket')
    parser.add_argument('--pages', type=int, default=100)
    parser.add_argument('--step-ms', type=int, default=50)
    args = parser.parse_args()
    if args.pages < 1 or args.step_ms < 1:
        parser.error('pages and step-ms must be positive')
    args.output.mkdir(parents=True, exist_ok=True)
    stops = [0] * max(1, 1500 // args.step_ms)
    stops += list(range(0, args.pages * 6000, 6000)) * 2
    duration = len(stops) * args.step_ms + 1500
    env = os.environ.copy()
    env.update(SCOUR_TRACE='1', SCOUR_GUI_VIEW='detail',
               SCOUR_GUI_SCROLL=','.join(map(str, stops)),
               SCOUR_GUI_SCROLL_MS=str(args.step_ms),
               SCOUR_GUI_SNAP=str(args.output.resolve() / 'window.ppm'),
               SCOUR_GUI_SNAP_MS=str(duration))
    command = [str(args.binary.resolve())]
    if args.socket:
        command += ['--socket', args.socket]
    samples = []
    with (args.output / 'trace.log').open('w') as log:
        process = subprocess.Popen(command, env=env, stdout=log, stderr=log)
        started = time.monotonic()
        try:
            while process.poll() is None:
                elapsed = time.monotonic() - started
                if elapsed > duration / 1000 + 15:
                    raise RuntimeError('GUI timeout')
                try:
                    status = Path(f'/proc/{process.pid}/status').read_text()
                    stat = Path(f'/proc/{process.pid}/stat').read_text().split(') ', 1)[1].split()
                    sample = {'seconds': round(elapsed, 3),
                              'cpu_s': (int(stat[11]) + int(stat[12])) / os.sysconf('SC_CLK_TCK')}
                    sample.update({key: int(re.search(r'^' + key + r':\s+(\d+)', status, re.M)[1])
                                   for key in ['VmRSS', 'VmHWM', 'RssAnon']})
                    samples.append(sample)
                except FileNotFoundError:
                    pass
                time.sleep(.02)
            if process.returncode:
                raise RuntimeError(f'GUI failed: {process.returncode}; see trace.log')
        finally:
            if process.poll() is None:
                process.terminate()
                process.wait(timeout=10)
    trace = (args.output / 'trace.log').read_text()
    pages = sorted(float(x) for x in re.findall(r'landed in ([\d.]+) ms', trace))
    held = [int(x) for x in re.findall(r'(\d+) rows in hand', trace)]
    # Exclude the full-frame snapshot's temporary pixel buffers and exit work.
    measured = [s for s in samples if s['seconds'] < duration / 1000 - .5]
    summary = {'binary': str(args.binary.resolve()), 'sample_seconds': measured[-1]['seconds'],
               'peak_rss_kib': max(s['VmHWM'] for s in measured),
               'last': measured[-1], 'max_cached_rows': max(held, default=0),
               'pages': len(pages),
               'page_ms_median': pages[len(pages) // 2] if pages else None,
               'page_ms_p95': pages[min(len(pages) - 1, int(len(pages) * .95))] if pages else None}
    (args.output / 'samples.json').write_text(json.dumps(samples, indent=2))
    (args.output / 'summary.json').write_text(json.dumps(summary, indent=2))
    print(json.dumps(summary, indent=2))


if __name__ == '__main__':
    main()
