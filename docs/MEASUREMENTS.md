# Measurements

Numbers, with the command that produced them. A claim without one of these is
an opinion.

## 2026-08-02 — first end-to-end run

Machine: Linux 6.18, NVMe. Release build.

Corpus: two project trees, **44,755 entries** (including `target/`, which is
deliberately indexed — build output is where a lot of "where did that file go"
questions actually land).

```bash
scourd --config <cfg> &
scour maintain rebuild
scour status
```

| | |
|---|---|
| entries | 44,755 |
| index on disk, before rebuild | 26.33 MiB |
| index on disk, after rebuild | 19.23 MiB |
| unsorted tail after rebuild | 0 |

### Keystroke latency

`scour search "<query>" -n 40`, whole round trip: client connect excluded,
socket, parse, search, sort, count, and 40 materialised rows included.

| query | matches | time |
|---|---|---|
| `*.toml` | 23 | 0.74 ms |
| `ext:rs` | 136 | 1.09 ms |
| `under:/…/Scour ext:rs` | 65 | 1.36 ms |
| `engine` | 69 | 1.62 ms |
| `ext:rs size:>10kb` | 43 | 1.74 ms |
| `kind:code dm:7d` | 3,559 | 2.18 ms |
| `sco` | 1,012 | 3.46 ms |
| `scour` | 902 | 4.90 ms |

Before the rebuild the same queries took 5–24 ms and reported `(full scan)`:
with every segment outside the ordered body, there is nothing to terminate
early against. That gap is what `Maintenance::Rebuild` exists to close, and
what `Status::rebuild_advised` exists to warn about.

### Still to measure

* 5M and 10M entries, on this engine rather than on the prototype. The
  prototype measured 28.2 ms and 56.5 ms for the heaviest query at those
  scales; nothing here has confirmed it.
* A subtree rename. Never measured, on either implementation.
* The rebuild threshold — how large the tail may grow before searches are
  noticeably slower. `200_000` is currently a guess.
* Peak memory during a scan of a whole home directory.
