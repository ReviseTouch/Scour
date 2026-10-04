# Scour

A local file index and search: the answer to "where is that file", on a machine
with five million of them. The working agreement is `AGENTS.md`; the map is
`docs/ARCHITECTURE.md`; the numbers are `docs/MEASUREMENTS.md`.

## This machine

- Working copy `/home/hasan/Projeler/Scour`, remote `origin` on GitHub. No
  second copy anywhere: **commit and push before the session ends**, and check
  with `git ls-remote origin refs/heads/main`, not `git status`.
- Not in the repository: `target/` (850,000 files, the loudest thing in the
  index), the index itself under `~/.local/share/scour/index`, the scratch
  under `/var/tmp`.
- The live service is the owner's; never point a test harness at his index or
  his settings — copy the index (reflink) and use a socket of your own.
- **Heavy builds wait their turn in `derle`**, the machine's build queue: two
  at a time, each in a cgroup of its own, because several agents build here at
  once. `derle cargo test --workspace`, `derle bash scripts/check`,
  `derle scripts/release-build`, `derle cargo xwin build …`. Run them in the
  background or with a long timeout — waiting in the queue is normal, and
  `derle --durum` says who is ahead. Nothing that only runs a built binary goes
  through it (stress scripts, measurements): build first, then run those
  without it. Debug builds unless a release is what is shipped or measured.
  The rule in full: `~/Projeler/sistem-kurulum/derleme-kuyrugu/AJANLARA.md`.
