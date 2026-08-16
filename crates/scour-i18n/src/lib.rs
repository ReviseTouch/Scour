//! Translation.
//!
//! gettext catalogues, compiled into the binary, keyed by the **English text
//! itself** rather than by an invented identifier. That choice is what makes
//! the whole scheme cheap to live with:
//!
//! * A string with no translation degrades to correct English, not to a bare
//!   key like `status.entries.label` leaking into someone's terminal.
//! * A translator reads `"The index is empty"` and not `msg_idx_empty_3`.
//! * Every frontend asks the same question of the same catalogue, so there is
//!   one set of `.po` files rather than two that drift.
//!
//! **The window asks this crate, not the toolkit.** This said the interface
//! would use Slint's own `@tr()` — also gettext, also keyed by the source
//! string — and that is not what was built. `@tr()` needs slint's `gettext`
//! feature, which is libintl on every platform, and a `slint::init_translations!`
//! pointing at compiled `.mo` files on disk; `apps/scour-gui` pins slint with
//! default features and its `build.rs` is a bare `slint_build::compile`, so an
//! `@tr()` there would hand back the English and nothing would say why. The
//! window looks each string up here instead and sets it as a property — which
//! is what keeps the promise below, because the catalogue is in the binary and
//! a window with no `.mo` files beside it is still translated.
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

pub use catalogue::{Catalogue, LANGUAGES, choose, system_language};
