//! How a disk is spoken of, by the names the wire gives its kind and state:
//! one wording for every face, and English msgids for `scour-i18n`.

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
        let kinds = ["system", "fixed", "removable", "network"];
        let mut seen: Vec<&str> = kinds.iter().map(|k| kind(k)).collect();
        seen.dedup();
        assert_eq!(seen.len(), kinds.len());
        let states = [
            "auto",
            "chosen",
            "configured",
            "off",
            "asking",
            "later",
            "manual",
        ];
        let mut seen: Vec<&str> = states.iter().map(|s| state(s)).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), states.len());
    }
}
