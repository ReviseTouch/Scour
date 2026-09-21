//! What came of trying.

/// The answer to "is something listening now, and did we do it?".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Something was already there. Nothing was started.
    AlreadyRunning,
    /// A service was started here and answered.
    Started,
    /// Nothing was tried: `SCOUR_NO_AUTOSTART` is set.
    Off,
    /// Nothing is listening, and this is the sentence that says why.
    Failed(String),
}

impl Outcome {
    /// Is something listening at the address now?
    pub fn running(&self) -> bool {
        matches!(self, Outcome::AlreadyRunning | Outcome::Started)
    }

    /// The sentence to show beside the old hint, if there is one.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Outcome::Failed(why) => Some(why),
            _ => None,
        }
    }
}
