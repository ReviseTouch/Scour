#!/usr/bin/env python3
"""Exercise a private daemon and index; never connect to the user's service.

Build first: cargo build --release -p scourd
Run: python3 scripts/reliability-probe.py [number-of-files]
"""
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import time

BINARY = Path(__file__).resolve().parent.parent / 'target/release/scourd'


def call(address, op, **fields):
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(10)
        connection.connect(str(address))
        connection.sendall((json.dumps({'id': 1, 'op': op, **fields}) + '\n').encode())
        reply = json.loads(connection.makefile('rb').readline())
        if 'error' in reply:
            raise RuntimeError(reply['error'])
        return reply['ok']


def inventory(root):
    paths = [root, *root.rglob('*')]
    return {str(p): (p.is_dir(), p.lstat().st_size if not p.is_dir() else None) for p in paths}


def indexed(address):
    out = {}
    offset = 0
    while True:
        page = call(address, 'search', query='', sort='name', descending=False,
                    page={'offset': offset, 'limit': 200, 'count_cap': 1000000})
        for hit in page['hits']:
            out[hit['path']] = (hit['is_dir'], None if hit['is_dir'] else hit['meta']['size'])
        if len(page['hits']) < 200:
            return out
        offset += 200


def converge(address, expected):
    began = time.monotonic()
    last = {}
    while time.monotonic() - began < 45:
        last = indexed(address)
        if last == expected:
            return round((time.monotonic() - began) * 1000, 1)
        time.sleep(.1)
    raise AssertionError({'missing': list(expected.keys() - last.keys())[:8],
                          'ghosts': list(last.keys() - expected.keys())[:8],
                          'metadata': [p for p in last.keys() & expected.keys() if last[p] != expected[p]][:8]})


def main():
    number = int(sys.argv[1]) if len(sys.argv) > 1 else 2000
    with tempfile.TemporaryDirectory(prefix='scour-reliability-') as temp:
        base = Path(temp); root = base / 'files'; root.mkdir()
        for i in range(number):
            folder = root / f'dir-{i % 20:02}'
            folder.mkdir(exist_ok=True)
            (folder / f'file-{i:06}.txt').write_text('initial')
        address = base / 'service.sock'
        config = base / 'config.toml'
        config.write_text(f'''[index]
dir = {json.dumps(str(base / 'state' / 'index'))}
[[source]]
name = "probe"
roots = [{json.dumps(str(root))}]
watch = false
[exclude]
allow = [{json.dumps(str(root))}]
[service]
commit_interval_ms = 50
commit_idle_ms = 50
poll_interval_secs = 1
reconcile_interval_secs = 2
[scan]
threads = 2
''')
        with (base / 'daemon.log').open('w+') as log:
            process = subprocess.Popen([str(BINARY), '--config', str(config), '--socket', str(address)],
                                       stdout=log, stderr=log)
            try:
                for _ in range(200):
                    if address.exists():
                        break
                    if process.poll() is not None:
                        log.seek(0); raise RuntimeError(log.read())
                    time.sleep(.05)
                results = {'files_seeded': number, 'initial_ms': converge(address, inventory(root))}
                # No watcher and no explicit rescan: all changes need recovery.
                for i in range(100):
                    (root / 'dir-00' / f'new-{i}.pdf').write_bytes(b'new file')
                for path in list((root / 'dir-01').glob('*.txt'))[:30]:
                    path.write_bytes(b'updated file with a different size')
                (root / 'dir-02').rename(root / 'renamed-directory')
                shutil.rmtree(root / 'dir-03')
                results['churn_ms'] = converge(address, inventory(root))
                # Atomic replacement and a hard link, with ordinary directory deletion.
                target = root / 'replacement.txt'
                staging = root / 'staging.txt'; staging.write_bytes(b'atomic replacement')
                staging.replace(target)
                os.link(target, root / 'hard-link.txt')
                results['replacement_ms'] = converge(address, inventory(root))
                results['verified_entries'] = len(inventory(root))
                results['status'] = call(address, 'status')
                print(json.dumps(results, indent=2))
            finally:
                if process.poll() is None:
                    try:
                        call(address, 'shutdown')
                        process.wait(timeout=10)
                    except (OSError, RuntimeError, subprocess.TimeoutExpired):
                        process.terminate()
                        process.wait(timeout=10)


if __name__ == '__main__':
    main()
