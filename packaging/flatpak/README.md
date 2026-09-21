# Scour as a Flatpak

Four files: the manifest, the AppStream metainfo, the generated list of crate
sources, and a replacement for `scripts/scour-app` that suits a sandbox. The
menu entry and the icon are the project's own — `packaging/scour.desktop` and
`assets/scour.svg` — renamed to the app id at build time, so there is one copy
of each in the repository.

## Building it

`flatpak-builder` is not needed on the system; the Flathub build tool is
itself a Flatpak.

```bash
flatpak install --user flathub org.flatpak.Builder
flatpak install --user flathub org.freedesktop.Platform//25.08 \
                               org.freedesktop.Sdk//25.08 \
                               org.freedesktop.Sdk.Extension.rust-stable//25.08

# from the root of the repository
flatpak run org.flatpak.Builder --user --install --force-clean \
    build-dir packaging/flatpak/com.revisetouch.Scour.yml

flatpak run com.revisetouch.Scour                          # the launcher
flatpak run --command=scour com.revisetouch.Scour --version
```

The manifest's source is the local directory, which builds whatever is in the
working tree. Flathub builders have no local tree: the comment above the
source shows the `type: git` form that replaces it there.

### Regenerating the crate sources

A Flathub build has no network, so every crate of `Cargo.lock` is fetched
beforehand as its own archive. `cargo-sources.json` holds that list and is
regenerated whenever `Cargo.lock` changes:

```bash
curl -O https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/master/cargo/flatpak-cargo-generator.py
pip install aiohttp PyYAML tomlkit        # in a virtualenv
python3 flatpak-cargo-generator.py Cargo.lock \
    -o packaging/flatpak/cargo-sources.json
```

The build runs `cargo --frozen`, so a stale `cargo-sources.json` fails the
build instead of quietly reaching for the network.

## What the sandbox can and cannot do

Everything the manifest's header explains, in short.

**Works.** Indexing and searching the host filesystem (`--filesystem=host`);
the window; opening a file, revealing its folder and opening the browser face,
all through the runtime's `xdg-open`, which is the OpenURI portal; renaming;
the command line and the MCP server, run inside the sandbox.

Verified on this machine against the 25.08 runtime: `scourd` started, indexed
and listened; `scour` searched it; `flatpak run com.revisetouch.Scour` ran
`scour-open`, which exec'd `scour-gui` and opened a window; all seven binaries
answered `--version`. The service's own message names the missing mark:
"no fanotify mark for … it will not be watched live", and `scour status`
reports `watched 0`.

**No live watching.** The only watcher on Linux is a `fanotify` mark, which
needs `CAP_SYS_ADMIN`. A Flatpak does not have it, `scour-watch` cannot be
installed as a privileged helper from inside one, and there is no inotify
fallback — the crate does not compile `notify` on Linux at all. The service
still notices changes: it walks a root when that root's pulse moves (btrfs
`ctransid`, or the write-sector counter in `/proc/diskstats`) and gives every
source a full reconciliation pass every 30 minutes by default. Changes are
found later, not lost. Network and FUSE mounts have no pulse, so only the
periodic pass finds them. Somebody who wants live watching wants the tarball
and `packaging/install-service.sh`, not this.

**No system unit.** Nothing outside the sandbox starts `scourd`. The window,
the terminal face and the browser bridge start it themselves when no service
answers (`scour-launch`), so the service is a background process of the app.
The command line does not: open a face first, or start the service by hand
with `flatpak run --command=scourd com.revisetouch.Scour &`.
`SCOUR_NO_AUTOSTART=1` turns the autostart off.

**The host cannot reach the socket by default.** `scourd` listens on
`$XDG_RUNTIME_DIR/scour/scour.sock`. Inside the sandbox `XDG_RUNTIME_DIR` is
`/run/user/<uid>` as usual, but that is a directory of the app's own — on the
host, `/run/user/<uid>/.flatpak/com.revisetouch.Scour/xdg-run` — shared by
every instance of this app and by nothing else. Run the client in the same
sandbox:

```bash
flatpak run --command=scour     com.revisetouch.Scour "ext:rs dm:7d"
flatpak run --command=scour-tui com.revisetouch.Scour
flatpak run --command=scour-mcp com.revisetouch.Scour   # give this to an MCP host
```

Or name the socket from the host, which works (verified):

```bash
scour --socket /run/user/1000/.flatpak/com.revisetouch.Scour/xdg-run/scour/scour.sock "rapor"
```

`--filesystem=xdg-run/scour:create` is not requested, and not because it would
fail — it would publish the host's `/run/user/<uid>/scour` at exactly the path
this app computes, and the two would share one socket. That is the problem: a
machine with Scour installed both ways would have two services racing for one
address with two different indexes behind them.

**Thumbnails are asked for and never arrive.** The runtime ships five
thumbnailer declarations (png, jpeg, gif, tiff, webp, avif, jxl, svg) and not
the `/usr/bin/gdk-pixbuf-thumbnailer` all five name — that binary is in the
Sdk, not the Platform. So a picture row is attempted once, the spawn fails,
the failure is written to the freedesktop `fail/` directory, and the row is
never asked again: icons instead of tiles, no error in front of anybody, one
cheap failed `exec` per file. Adding `gdk-pixbuf` as a manifest module would
fix the image kinds; video and PDF need thumbnailers no runtime carries.

**No clipboard.** Copying goes through `wl-copy`, `xclip` or `xsel`, none of
which is in the runtime; Copy reports that no helper was found.

**Trash is the app's own.** Deleting a file on the home filesystem moves it to
`~/.var/app/com.revisetouch.Scour/data/Trash`, not the trash the file manager
shows. On any other volume it goes to that volume's `.Trash-<uid>`, where the
file manager does find it.

**Settings and index move.** `~/.config/scour/config.toml` becomes
`~/.var/app/com.revisetouch.Scour/config/scour/config.toml`, and the index
lives under `~/.var/app/com.revisetouch.Scour/data/scour/index`. A system
installation of Scour and this one keep separate indexes and separate
settings.

## Flathub submission checklist

- **Domain verification.** The app id is `com.revisetouch.Scour`, so Flathub
  asks for proof of `revisetouch.com`: either the submitter's GitHub account
  is verified against a `.well-known` file on that domain, or the app id is
  changed to `io.github.revisetouch.Scour`. The domain is the author's, so the
  first is the one to do.
- **A repository of its own.** Flathub wants `com.revisetouch.Scour.yml` in
  `flathub/com.revisetouch.Scour`. The `type: dir` source here becomes
  `type: git` with a tag and the commit it points at.
- **Offline cargo sources.** `cargo-sources.json` must be committed beside
  the manifest and regenerated with `Cargo.lock`, as above. No build-time
  network is available.
- **Screenshots.** The metainfo points at the raw GitHub URLs of
  `docs/img/window.webp`, `browser.webp` and `terminal.webp` on `main`. They
  must keep resolving; moving or renaming them empties the store page.
  AppStream 1.0 accepts WebP.
- **Validation.** `appstreamcli validate --pedantic` passes with one pedantic
  note: `cid-contains-uppercase-letter`. That is the app id convention Flathub
  itself uses for the last segment and is not a blocker.
- **Permissions.** Four groups, each justified in the manifest:
  `--filesystem=host` (a file search tool indexes the filesystem),
  `--share=network` (the browser face's page is served on 127.0.0.1 to a
  browser outside the sandbox), the window sockets, and `--device=dri`.
  `--talk-name=org.freedesktop.Flatpak` is not requested and must never be.
- **The host-filesystem exception.** `flatpak-builder-lint` reports
  `finish-args-host-filesystem-access` as an *error*, by design: Flathub wants
  every app with `--filesystem=host` to argue for it in the submission pull
  request and grants an exception per app. The argument is the one in the
  manifest — an index of part of the filesystem answers part of the questions
  — and the comparison is with the file managers and search tools that already
  hold the same exception. Nothing smaller works: the file chooser portal
  grants one directory at a time, at the moment somebody picks it, and an
  index is built before anybody asks anything.

  ```
  $ flatpak run --command=flatpak-builder-lint org.flatpak.Builder \
        manifest packaging/flatpak/com.revisetouch.Scour.yml
  {"errors": ["finish-args-host-filesystem-access"],
   "warnings": ["runtime-update-available-to-org.freedesktop.Platform-26.08"]}
  ```

  Against a locally built repository the same linter adds two more, and both
  are artefacts of building it here rather than on Flathub — the buildbot
  mirrors screenshots to `dl.flathub.org/media` and rewrites the URLs:

  ```
  $ flatpak build-export repo build-dir master
  $ flatpak run --command=flatpak-builder-lint org.flatpak.Builder repo repo
  {"errors": ["appstream-screenshots-not-mirrored-in-ostree",
              "appstream-external-screenshot-url",
              "finish-args-host-filesystem-access"], …}
  ```
- **The runtime.** This manifest pins `25.08`, which is what the build here was
  verified against. `26.08` is out and the linter says so; bumping
  `runtime-version` is a one-line change and wants a rebuild to confirm the
  Rust extension and fontconfig still behave.
