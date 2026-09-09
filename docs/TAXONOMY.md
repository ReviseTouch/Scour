# File kinds

`Kind` decides what `kind:` matches, what the rail lists and how the report
groups bytes. It is a `u8` in an on-disk column: **never renumber a variant,
never reuse a discriminant.** A retired kind keeps its number and stays
decodable; new kinds are appended; when the meaning of a number changes,
`FORMAT` in `scour-index-native` is bumped and every index is rebuilt.

Measured on 1,644,841 entries and 473.8 GB of one developer's Linux home.

| # | identifier | token | what belongs in it | entries | bytes |
|---|---|---|---|---:|---:|
| 0 | `File` | `file` | Nothing matched. The honest unknown. | 6.3% | 0.8% |
| 1 | `Dir` | `folder` | `is_dir`, decided before anything else. | 12.9% | — |
| 2 | `Code` | `code` | Source a person wrote: languages, shells, templates, stylesheets, shaders. | 11.4% | 0.9% |
| 3 | `Image` | `image` | Raster, vector, camera raw, layered editor documents. | 3.5% | 0.1% |
| 4 | `Archive` | `archive` | A container of other files: archives, packages, disk and VM images. | 0.5% | 3.6% |
| 5 | `Doc` | `doc` | Something a person reads: office, PDF, plain text, prose markup, ebooks, mail. | 3.1% | 0.2% |
| 6 | `Exec` | `exec` | Machine code the OS can run. | 1.6% | 27.4% |
| 7 | `Media` | `media` | **Retired.** Never produced again; still decoded from old indexes. | — | — |
| 8 | `Audio` | `audio` | Sound without video, plus playlists. | 0.01% | 0.02% |
| 9 | `Video` | `video` | Moving pictures. | 0.00% | 0.01% |
| 10 | `Build` | `build` | Machine-produced and regenerable. Deleting it costs time, not information. | **54.2%** | **65.7%** |
| 11 | `Data` | `data` | Structured machine-readable content: serialisation, tables, schemas, databases, weights, logs. | 5.1% | 1.1% |
| 12 | `Config` | `config` | Settings that change how a program behaves; certificates and keys. | 1.4% | 0.03% |
| 13 | `Font` | `font` | Typefaces. | 0.1% | 0.04% |

Group aliases with no discriminant of their own: `media` → Audio, Video, Media;
`text` → Code, Data, Config, Doc. `kind:` accepts every token above.

## Classification order

```
1.  is_dir                                        -> Dir
2.  ext = text after the last dot
    2a. html|htm and the name looks generated (struct.*, fn.*, package-summary…) -> Build
    2b. EXT_TABLE[ext]                            -> that kind
    2c. unknown extension                         -> step 4
3.  no extension
    3a. NAME_TABLE[fold(name)]                    -> that kind   (exact; dotfiles included)
    3b. STEM_PREFIX[fold(name)]                   -> that kind   (readme*, license*, …)
4.  mode & 0o111 != 0                             -> Exec        (unix only; mode is 0 on Windows)
5.                                                -> File
```

* `is_dir` first: on macOS `.app`, `.xcodeproj` and `.framework` are directories.
* The exec bit last, and it never overrides an extension: 17,073 files here
  carry `u+x` by accident, among them 3,408 `.png`.
* Names are folded with `DefaultFolder`, not lower-cased: `LİCENSE` must match
  in a Turkish locale.
* A purely numeric "extension" (`2.2.20` → `20`) lands in `File`, which is right
  by accident; `ext_str`'s contract is shared with the `ext:` term, so it stays.

## Why these lines

* **`Build` is a format, not a provenance.** An ELF object is a kind of thing
  the way a PNG is. A `.rs` inside `target/` is a Rust file that happens to be
  generated; that is a location, and would be a separate `derived` bit — not
  built.
* **`.json`, `.xml` are `Data`, not `Code`.** With them in `Code`, `kind:code`
  was 27% of the disk and half of it not source.
* **`.html` is `Doc`, generated documentation is `Build`.** 83.7% of the HTML
  files here are rustdoc or javadoc item pages. The name rule is a heuristic
  for two toolchains and is labelled one.
* **Audio and Video are split** because every prior art splits them and "how
  much music do I have" is a question the report exists to answer.
* Rejected: `Db` (into Data), `Disk` (into Archive — the closest call), `Model`
  (`.obj` collides with object files), `Log`/`Cert`/`Key` (`ext:` answers it),
  `Temp`/`Backup`/`Link` (a state, not a type).

## Where `File` is the honest answer

Extensionless files not on a name list (Cargo fingerprints, Git objects), false
extensions (`4.1.93.final`), private formats, `trybuild` `.stderr` fixtures,
`.orig`/`.bak`. An extension goes in a table when it means one thing.

## Least certain

`Config` as a kind (fuzzy border with `Data`; first to cut) · `Font` (clean,
0.1%, no competitor has it) · disk images inside `Archive` · the generated-HTML
rule · `.mts` → Code, `bat`/`cmd` → Exec while `sh`/`ps1` → Code. The corpus is
one developer's machine; the Audio, Video, Doc and Image tables are sized on
prior art rather than on it.

Sources: freedesktop shared-mime-info, Apple UTIs, GitHub Linguist, Everything
1.5 filters, FSearch, Windows `PerceivedType` and `System.Kind`, TreeSize, GNOME
Files.
