//! What a frontend remembers between runs, kept in the service — the only thing all
//! the frontends talk to. One typed field per thing a *person* decides, so two of
//! them cannot invent two names for one idea; [`Settings::view`] is free-form and
//! namespaced for what only one window can mean. Every field is `#[serde(default)]`,
//! and a write is a [`Change`]: absent means "no opinion", never "clear it".

mod change;
mod settings;
mod tabs;
pub use change::*;
pub use settings::*;
pub use tabs::*;

#[cfg(test)]
mod tests;
