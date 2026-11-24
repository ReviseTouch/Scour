//! `content:` and grep: the files a query names, read on a thread of their own.

use super::*;
use scour_core::Result;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// How a query's words are read, which a person chooses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Reading {
    /// A letter and the same letter with a mark are different letters.
    pub exact_accents: bool,
    pub typos: Typos,
    /// A misspelling forgiven only past a word's first four letters: a longer
    /// word then never finds more than a shorter one.
    pub strict_typos: bool,
}

impl Reading {
    pub(super) fn bits(self) -> u8 {
        u8::from(self.exact_accents) | (self.typos as u8) << 1 | u8::from(self.strict_typos) << 3
    }

    pub(super) fn of(bits: u8) -> Reading {
        Reading {
            exact_accents: bits & 1 != 0,
            typos: match bits >> 1 & 3 {
                0 => Typos::Off,
                1 => Typos::Suggest,
                _ => Typos::Always,
            },
            strict_typos: bits & 8 != 0,
        }
    }
}

impl Engine {
    /// Read every query's words this way from now on, and tell whoever is
    /// waiting that a search run again could answer differently.
    pub fn set_reading(&self, reading: Reading) {
        if self.shared.read_as.swap(reading.bits(), Ordering::Relaxed) != reading.bits() {
            *self.shared.similar.lock() = None;
            self.shared.touched_now();
        }
    }

    /// The identity every thread that reads file contents takes first: a
    /// service reads as the person it serves. See [`scour_grep::Enter`].
    pub fn read_files_as(&self, enter: scour_grep::Enter) {
        *self.shared.enter.write() = Some(enter);
    }

    /// The reading of a `content:` query goes on from where it stopped, or
    /// begins again: every face waiting is told to ask once more.
    pub fn reread(&self, from_start: bool) {
        if self.shared.contents.again(from_start) {
            self.shared.touched();
        }
    }

    /// What a reading of file contents borrows from here.
    pub(super) fn lent(&self) -> crate::content::Lent {
        let shared = Arc::clone(&self.shared);
        crate::content::Lent {
            index: Arc::clone(&self.shared.index),
            roots: self.roots(),
            enter: self.shared.enter.read().clone(),
            revision: self.shared.revision.load(Ordering::Acquire),
            touched: Arc::new(move || shared.touched()),
        }
    }

    fn roots(&self) -> Vec<String> {
        self.shared
            .sources
            .iter()
            .flat_map(|source| source.describe().roots)
            .collect()
    }

    /// Which of the files a query names hold a pattern, read now and newest
    /// first. `enter` is what each reading thread does first: see
    /// [`scour_grep::Enter`].
    pub fn grep(
        &self,
        query: &str,
        ask: &crate::GrepAsk,
        enter: Option<scour_grep::Enter>,
    ) -> Result<crate::Grepped> {
        self.shared.reading();
        let (index, roots) = (self.shared.index.as_ref(), self.roots());
        // What reads the rest of the query reads this one too, `content:` aside.
        let ast = crate::content::without(self.shared.ast(query));
        crate::grep::run(index, &roots, ast, CANDIDATES, ask, enter)
    }
}
