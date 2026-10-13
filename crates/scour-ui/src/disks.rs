//! How a disk is spoken of, by the names the wire gives its kind and state:
//! one wording for every face, and English msgids for `scour-i18n`.

/// Every kind the wire names, for a face that builds its table up front.
pub const KINDS: [&str; 4] = ["system", "fixed", "removable", "network"];

/// Every state the wire names.
pub const STATES: [&str; 7] = [
    "auto",
    "chosen",
    "configured",
    "off",
    "asking",
    "later",
    "manual",
];

/// What kind of disk it is.
pub fn kind(token: &str) -> &'static str {
    match token {
        "system" => "system disk",
        "removable" => "plugged in",
        "network" => "on the network",
        _ => "built in",
    }
}

/// Whether it is indexed, and who decided.
pub fn state(token: &str) -> &'static str {
    match token {
        "auto" => "indexed",
        "chosen" => "indexed, chosen",
        "configured" => "set in config.toml",
        "off" => "left out",
        "asking" => "waiting for an answer",
        "later" => "not now",
        _ => "not indexed",
    }
}

/// The question a newly found disk is asked with; `{name}` is its name.
pub fn question(kind: &str) -> &'static str {
    match kind {
        "network" => "{name} is on the network. Index it too?",
        _ => "{name} was plugged in. Index it too?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_name_the_wire_uses_has_its_own_words() {
        let mut seen: Vec<&str> = KINDS.iter().map(|k| kind(k)).collect();
        seen.dedup();
        assert_eq!(seen.len(), KINDS.len());
        let mut seen: Vec<&str> = STATES.iter().map(|s| state(s)).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), STATES.len());
    }
}
