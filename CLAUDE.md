# Scour

A local file index and search: the answer to "where is that file", on a machine
with two million of them.

---

## Where this lives, and where it does not

| | |
|---|---|
| working copy | `/home/hasan/Projeler/Scour` — Linux, ext4 |
| remote | `origin` → `https://github.com/hasantr/Scour` (**private**) |
| Windows copy | **none** |

**This project was born on Linux and has no second working copy.** That makes it
the exception among its neighbours, which are all synchronised between a Linux
and a Windows copy through GitHub. There is nothing here to keep in step with —
but there is also nothing else holding the work: for its first five days this
repository existed on exactly one disk, in one directory, with no remote at all,
and that is why `origin` was added on 2026-08-07.

So the one rule that matters: **commit and push before the session ends.** An
unpushed commit is one disk failure away from never having existed. Verify with
the remote itself rather than with `git status`, which will happily report a
branch as up to date against a ref nobody fetched:

```bash
git ls-remote origin refs/heads/$(git branch --show-current)
```

If a Windows copy is ever made, it is cloned **from GitHub** — never copied from
here. Copying a working tree copies its `.git`, and two copies that were never
told about each other diverge silently.

## What is not in the repository

- `target/` — a release build is a few minutes; the build tree is 850,000 files
  and the single loudest source of noise in an index of this machine.
- The index itself, which lives in `~/.local/share/scour/index` and is rebuilt
  by scanning. Nothing in it is authored.
- The measurement scratch under `/var/tmp` and the harness configs; the tools
  that matter are committed as `examples/`.

## The one architectural rule

**No crate depends on another except `scour-core`.** Core is types and traits and
nothing else. Concrete types are named in exactly one file, `apps/scourd/src/wire.rs`
— if a second file ever needs to say `NativeIndex`, something above it has stopped
being written against the trait.

When an implementation crate seems to need another implementation crate, the thing
that is missing is a trait in core. Add the trait, not the dependency.

## How a change is argued for here

Performance claims carry the command that produced them, and they live in
`docs/MEASUREMENTS.md`. A claim without one is an opinion. The negative results
are in there too, and they are the more useful half — several plausible ideas
have been measured and rejected, and the record is what stops them being
proposed again.

Four rules, each of which was learned by breaking it:

1. **Alternate.** Never compare two numbers taken at different times; this
   machine drifts more than 10% across a day.
2. **One binary per experiment.** An A/B was invalidated by rebuilding the
   service halfway through. Use an explicit control, not an unset variable.
3. **Check what else is running, at both ends of the measurement.** A `cargo
   build` writes tens of thousands of files into a watched source and has twice
   been mistaken for the service's own cost. So has an open window.
4. **Measure the broad case, not the convenient one.** `/api/facets` is 2.6 ms
   on `trabzon` and 810 ms on the empty query; measuring only the first hid a
   tenth of a core for hours.

## Reading the code

`docs/ROADMAP.md` says what is left and why in that order. `docs/AUDIT.md`,
`docs/REVIEW-MEMORY.md` and the other reviews record what was investigated and
what was deliberately left open — a section headed "found, not fixed" means the
decision is still yours to make, not that nobody noticed.

Comments here carry the measurement and the reason, not a description of the
code beside them. Match that.
