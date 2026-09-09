//! What Scour can do, and which of its four faces can do it: every feature and
//! where each face stands with it, so a face that is behind says so in code.
//!
//! Data, not enforcement — no test can tell whether a window draws a rail. The
//! tests keep the table honest: a state per face, and a note on every gap.

/// One of the four ways to use Scour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Face {
    /// The browser page, served by `scour-web`.
    Page,
    /// The Slint window, `scour-gui`.
    Window,
    /// The terminal, `scour-tui`.
    Terminal,
    /// The command line, `scour`.
    Line,
}

impl Face {
    pub const ALL: [Face; 4] = [Face::Page, Face::Window, Face::Terminal, Face::Line];

    pub fn name(self) -> &'static str {
        match self {
            Face::Page => "page",
            Face::Window => "window",
            Face::Terminal => "terminal",
            Face::Line => "command line",
        }
    }
}

/// How far a face has got with a feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// It is there.
    Has,
    /// Part of it is there, and the note says which part is not.
    Part(&'static str),
    /// It is not there, and the note says why not — or "not yet".
    Not(&'static str),
    /// It cannot be there; the note says why. A thumbnail in a command line.
    Never(&'static str),
}

/// A thing Scour does, and where each face stands with it.
#[derive(Debug, Clone, Copy)]
pub struct Feature {
    /// A short, stable name. Used in prose and in the table.
    pub id: &'static str,
    /// What it is, in a line.
    pub what: &'static str,
    /// One state per face, in the order of [`Face::ALL`].
    pub faces: [State; 4],
}

use State::{Has, Never, Not, Part};

/// Everything, and everywhere it stands.
pub const FEATURES: &[Feature] = &[
    Feature {
        id: "search",
        what: "type and the list narrows, a page at a time",
        faces: [Has, Has, Has, Has],
    },
    Feature {
        id: "query-colours",
        what: "the query read back in colour, term by term",
        faces: [Has, Has, Has, Has],
    },
    Feature {
        id: "paging",
        what: "a window onto millions of rows, pages kept and fetched ahead",
        faces: [Has, Has, Has, Never("a command line prints once and exits")],
    },
    Feature {
        id: "columns",
        what: "name, kind, where, changed, size — sortable",
        faces: [
            Has,
            Has,
            Part("no kind column: the colour and the rail carry it"),
            Has,
        ],
    },
    Feature {
        id: "column-widths",
        what: "columns dragged wider, and remembered",
        faces: [
            Has,
            Has,
            Not("constraints rather than a hand"),
            Never("no columns to drag"),
        ],
    },
    Feature {
        id: "rail-kinds",
        what: "how many of each kind match, with a bar apiece",
        faces: [Has, Has, Has, Has],
    },
    Feature {
        id: "rail-places",
        what: "this desktop's own folders, as a filter",
        faces: [Has, Has, Has, Not("`under:` is typed instead")],
    },
    Feature {
        id: "rail-sizes",
        what: "three size bands, as a filter",
        faces: [Has, Has, Has, Not("`size:` is typed instead")],
    },
    Feature {
        id: "time-strip",
        what: "twenty-four bars of when things changed; press one to narrow",
        faces: [Has, Has, Has, Not("`dm:` is typed instead")],
    },
    Feature {
        id: "selection",
        what: "pick rows, see what they come to, act on them",
        faces: [Has, Has, Has, Never("nothing to select in a printed list")],
    },
    Feature {
        id: "open",
        what: "open a row, or the folder holding it",
        faces: [Has, Has, Has, Has],
    },
    Feature {
        id: "report",
        what: "what the index holds: the largest, the duplicates, what a folder weighs",
        faces: [Has, Has, Has, Has],
    },
    Feature {
        id: "rules",
        what: "what the walk skips, in three groups, switchable",
        faces: [Has, Has, Has, Has],
    },
    Feature {
        id: "language",
        what: "Turkish or English, remembered",
        faces: [Has, Has, Has, Has],
    },
    Feature {
        id: "csv",
        what: "the whole result as a spreadsheet",
        faces: [Has, Has, Has, Has],
    },
    Feature {
        id: "faces",
        what: "switch to another face, and remember which one opens",
        faces: [Has, Has, Has, Has],
    },
    Feature {
        id: "live",
        what: "the list keeps up with the index as files change",
        faces: [Has, Has, Has, Never("printed once")],
    },
    Feature {
        id: "theme",
        what: "the shared palette, light and dark",
        faces: [Has, Has, Part("true colour when the terminal has it"), Has],
    },
    Feature {
        id: "icons",
        what: "a glyph per kind",
        faces: [
            Has,
            Has,
            Part("only when the terminal measures one column"),
            Not("not yet"),
        ],
    },
    Feature {
        id: "thumbnails",
        what: "pictures of pictures",
        faces: [
            Has,
            Has,
            Not("would need kitty or sixel"),
            Never("no pixels"),
        ],
    },
    Feature {
        id: "preview",
        what: "the head of a file, without opening it",
        faces: [Has, Has, Has, Has],
    },
    Feature {
        id: "keyboard",
        what: "everything reachable without a pointer",
        faces: [Has, Has, Has, Has],
    },
    Feature {
        id: "pointer",
        what: "everything reachable with one, and answering to it",
        faces: [Has, Has, Has, Never("no pointer")],
    },
];

/// Start a face so that it outlives the one starting it: a desktop closing a
/// window signals the whole process group, so `process_group(0)` puts the child
/// in its own. The child's own children inherit that group.
pub fn detach(command: &mut std::process::Command) {
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
}

/// The state of one feature in one face.
pub fn state(id: &str, face: Face) -> Option<State> {
    let at = Face::ALL.iter().position(|f| *f == face)?;
    FEATURES.iter().find(|f| f.id == id).map(|f| f.faces[at])
}

/// The table, as text — for a `--features` flag, a help panel, or a README.
pub fn table() -> String {
    let mut out = String::new();
    out.push_str(&format!("{:<16}", "feature"));
    for face in Face::ALL {
        out.push_str(&format!("{:<14}", face.name()));
    }
    out.push('\n');
    for feature in FEATURES {
        out.push_str(&format!("{:<16}", feature.id));
        for state in feature.faces {
            let said = match state {
                Has => "yes",
                Part(_) => "part",
                Not(_) => "no",
                Never(_) => "—",
            };
            out.push_str(&format!("{said:<14}"));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_feature_says_something_about_every_face() {
        // The table is fixed-width by construction; this is about the notes.
        for feature in FEATURES {
            for (at, state) in feature.faces.iter().enumerate() {
                let face = Face::ALL[at];
                match state {
                    Part(why) | Not(why) | Never(why) => assert!(
                        !why.trim().is_empty(),
                        "{} in the {} says nothing about what is missing",
                        feature.id,
                        face.name()
                    ),
                    Has => {}
                }
            }
        }
    }

    #[test]
    fn the_names_are_short_stable_and_unique() {
        let mut seen: Vec<&str> = Vec::new();
        for feature in FEATURES {
            assert!(
                !feature.id.is_empty() && feature.id.len() <= 16,
                "{} is not a short name",
                feature.id
            );
            assert!(
                !seen.contains(&feature.id),
                "{} is in the table twice",
                feature.id
            );
            assert!(
                !feature.what.is_empty(),
                "{} does not say what it is",
                feature.id
            );
            seen.push(feature.id);
        }
    }

    #[test]
    fn a_feature_can_be_asked_about() {
        assert_eq!(state("csv", Face::Terminal), Some(Has));
        assert!(matches!(state("thumbnails", Face::Line), Some(Never(_))));
        assert_eq!(state("nothing-like-this", Face::Page), None);
    }
}
