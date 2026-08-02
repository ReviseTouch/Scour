//! Translation.
//!
//! gettext catalogues, compiled into the binary, keyed by the **English text
//! itself** rather than by an invented identifier. That choice is what makes
//! the whole scheme cheap to live with:
//!
//! * A string with no translation degrades to correct English, not to a bare
//!   key like `status.entries.label` leaking into someone's terminal.
//! * A translator reads `"The index is empty"` and not `msg_idx_empty_3`.
//! * The user interface, when it arrives, uses Slint's own `@tr()`, which is
//!   also gettext and also keyed by the source string — so the *same* `.po`
//!   files serve both sides. One catalogue, not two that drift.
//!
//! Compiled in rather than loaded from disk because a search tool that cannot
//! find its own translation files is a worse bug than an untranslated string,
//! and because Windows and Android have no gettext runtime to speak of.
//!
//! What is *not* translated: anything a machine reads. Error codes, the query
//! language's field names, the MCP tool descriptions. A model asking for
//! `ext:rs` must get the same answer in every locale, and the query language
//! is part of the contract rather than part of the presentation.

mod catalogue;
mod po;

pub use catalogue::{Catalogue, LANGUAGES, system_language};
