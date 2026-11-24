//! The file that names an index's segments and its format.

use serde::{Deserialize, Serialize};

pub(super) const META_FILE: &str = "native-index.json";

/// Bumped when the files change shape **or when a stored value changes what it
/// means**. 2 added the trigram filter; 3 the per-block minimum and maximum; 4 a
/// distance byte a directory; 5 the file kinds, where nothing changed shape at
/// all and every older row still decodes, into the wrong answer; 6 the folded
/// name arena, without which a search has nothing to walk; 7 took the block from
/// 128 rows to 32, and every offset in every file is relative to it; 8 made a
/// row's identity its path — three identity columns went and the lookup table is
/// keyed on the path; 9 added the link count, without which a disk-usage report
/// counts a hard-linked file once per name; 10 keeps a name's spelling as its
/// difference from the fold, and the spelled arena went; 11 packed the row
/// orders and the path lookup table to the bits a row number needs; 12 keeps
/// the extension order as the steps between its rows; 13 folds a name without
/// its accents, so the arena, the trigrams and the name order all changed value.
pub(super) const FORMAT: u32 = 13;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(super) struct SegRef {
    pub(super) number: u64,
    pub(super) generation: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Meta {
    pub(super) format: u32,
    pub(super) generation: u64,
    pub(super) next_segment: u64,
    pub(super) segments: Vec<SegRef>,
}

impl Default for Meta {
    fn default() -> Self {
        Meta {
            format: FORMAT,
            generation: 0,
            next_segment: 1,
            segments: Vec::new(),
        }
    }
}
