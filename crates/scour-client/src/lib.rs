//! Talking to the service the way every face does: a pool of held
//! connections, one connection opened lazily and a service started when none
//! answers, and the requests whose wording is a decision rather than a shape.

mod asks;
mod lazy;
mod pool;

pub use asks::*;
pub use lazy::*;
pub use pool::*;
