//! The panel a status bar opens: what the service holds on this machine, said
//! the same way in every face. Things are named by the wire's tokens
//! (`HeldKind` in snake case, `Running::kind`); this crate takes no
//! dependency, so a face hands over plain numbers. Every word is a msgid.

/// What a press asks of the service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Do {
    /// `Clear { what: index }`: the index emptied and walked again.
    Reset,
    /// `Maintain { level: rebuild }`: the unsorted rows put in order.
    Sort,
    /// `Clear { what: texts }`.
    ForgetTexts,
    /// `Clear { what: previews }`.
    ClearPreviews,
    /// `Clear { what: page }`.
    ClearPage,
    /// The folder shown in the file manager. Nothing is asked of the service.
    Open,
    /// `Restart`.
    Restart,
}

/// A button. One that cannot be taken back asks first, in the row itself:
/// `ask` says what will happen, `go` does it; `done` is said afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Act {
    pub what: Do,
    pub button: &'static str,
    /// Empty for a press that needs no second step.
    pub ask: &'static str,
    pub go: &'static str,
    pub done: &'static str,
    /// Drawn in the exclusion red: it deletes something.
    pub danger: bool,
}

/// One thing the service keeps on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thing {
    /// The wire's `HeldKind`.
    pub token: &'static str,
    pub label: &'static str,
    /// What is in it, with `{n}` (items), `{parts}` and `{limit}` filled in
    /// by the face; for the previews and the page, plain words.
    pub note: &'static str,
    pub act: Act,
}

/// In the order the panel lists them, which is the order the service sends.
pub const THINGS: [Thing; 5] = [
    Thing {
        token: "index",
        label: "Index",
        note: "{n} entries · parts: {parts}",
        act: Act {
            what: Do::Reset,
            button: "Start over…",
            ask: "The index is emptied and every source is walked again. That takes a few minutes, and searches find less until it is done. Settings, rules and tabs stay.",
            go: "Empty and walk again",
            done: "The index was emptied; the walk has started.",
            danger: true,
        },
    },
    Thing {
        token: "texts",
        label: "Document texts",
        note: "{n} documents · up to {limit}",
        act: Act {
            what: Do::ForgetTexts,
            button: "Empty…",
            ask: "Text read out of PDF and Office documents is deleted. Content search is slower until it has read them again.",
            go: "Empty it",
            done: "Document texts emptied.",
            danger: true,
        },
    },
    Thing {
        token: "previews",
        label: "Previews",
        note: "documents turned into PDF",
        act: Act {
            what: Do::ClearPreviews,
            button: "Empty…",
            ask: "Previews are deleted; each is made again the first time its document is looked at.",
            go: "Empty it",
            done: "Previews emptied.",
            danger: true,
        },
    },
    Thing {
        token: "page",
        label: "Page window's profile",
        note: "its browser's data",
        act: Act {
            what: Do::ClearPage,
            button: "Empty…",
            ask: "The page window's own browser data is deleted: its cache and its window size.",
            go: "Empty it",
            done: "Profile emptied.",
            danger: true,
        },
    },
    Thing {
        token: "state",
        label: "Settings and logs",
        note: "kept",
        act: Act {
            what: Do::Open,
            button: "Open folder",
            ask: "",
            go: "",
            done: "",
            danger: false,
        },
    },
];

/// The index's unsorted rows, under its line: putting them in order makes the
/// index smaller, and measured no faster to search (2026-09-23).
pub const UNSORTED: &str = "Unsorted rows";
pub const SORT: Act = Act {
    what: Do::Sort,
    button: "Put in order…",
    ask: "{n} rows are put in order. It takes about a minute and makes the index smaller; searches are not measurably faster for it.",
    go: "Put in order",
    done: "Putting the rows in order.",
    danger: false,
};

/// One step: nothing is lost, and the service is back in seconds.
pub const RESTART: Act = Act {
    what: Do::Restart,
    button: "Restart the service",
    ask: "",
    go: "",
    done: "Restarting…",
    danger: false,
};

/// The line for a wire token, if this face knows it.
pub fn thing(token: &str) -> Option<&'static Thing> {
    THINGS.iter().find(|t| t.token == token)
}

/// What the row says instead of offering its button, for the wire's `busy`:
/// `open`, the page's browser holds its profile. `kept` is the settings,
/// which only ever offer to be shown.
pub fn refused(busy: &str) -> Option<&'static str> {
    match busy {
        "open" => Some("Close the page window first: its browser is using this profile."),
        _ => None,
    }
}

/// Scour's programs, by the wire's `Running::kind`, in the panel's order.
pub const RUNNING: [(&str, &str); 5] = [
    ("window", "Window"),
    ("page", "Page bridge"),
    ("terminal", "Terminal"),
    ("mcp", "MCP servers"),
    ("extractor", "Document reader"),
];

/// Under the MCP line: why there are so many.
pub const MCP_NOTE: &str = "Every open Claude session starts one of its own.";
/// After it: how long the oldest has run, `{u}` said as [`uptime`] says it.
pub const OLDEST: &str = "The oldest has been running for {u}.";

/// The panel's words that are not a row's.
pub const TITLE: &str = "Scour on this machine";
pub const ON_DISK: &str = "On disk";
pub const MEMORY: &str = "Memory";
pub const PROCESSOR: &str = "Processor";
pub const PROGRAMS: &str = "Running";
pub const SERVICE: &str = "Service";
pub const PEAK: &str = "at most {n} since it started";
pub const MACHINE_MEMORY: &str = "{used} of {total} in use";
pub const OTHERS: &str = "other programs";
pub const CORE: &str = "{n}% of one core";
pub const MACHINE_CPU: &str = "machine {n}% busy · {t} threads · load {l}";
pub const FOOT: &str = "Service running · version {v} · up {u}";
pub const OPEN_HINT: &str = "What Scour keeps on this machine";
pub const UNREADABLE: &str = "this system does not say";

/// One answer's running totals, for [`shares`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Sample {
    pub uptime_ms: u64,
    pub service_ms: u64,
    pub busy: u64,
    pub total: u64,
}

/// Between two answers: the service's processor time as a share of one core,
/// and the machine's busy share, in percent. None when no time passed or the
/// service started again in between (its uptime went back).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shares {
    pub service: f32,
    pub machine: f32,
}

pub fn shares(before: &Sample, now: &Sample) -> Option<Shares> {
    let wall = now.uptime_ms.checked_sub(before.uptime_ms)?;
    if wall == 0 {
        return None;
    }
    let used = now.service_ms.saturating_sub(before.service_ms);
    let total = now.total.saturating_sub(before.total);
    let busy = now.busy.saturating_sub(before.busy);
    Some(Shares {
        service: used as f32 * 100.0 / wall as f32,
        machine: if total == 0 {
            0.0
        } else {
            busy as f32 * 100.0 / total as f32
        },
    })
}

/// How long the service has run, as a msgid and its two numbers `{a}`, `{b}`:
/// the largest unit and the next, as a person says it.
pub fn uptime(ms: u64) -> (&'static str, u64, u64) {
    let s = ms / 1000;
    match s {
        0..60 => ("{a} s", s, 0),
        60..3600 => ("{a} min {b} s", s / 60, s % 60),
        3600..86_400 => ("{a} h {b} min", s / 3600, s % 3600 / 60),
        _ => ("{a} d {b} h", s / 86_400, s % 86_400 / 3600),
    }
}

/// Each size's share of the largest, for the bars under the rows: the largest
/// fills its bar, and nothing that is something is drawn as nothing.
pub fn bars(bytes: &[u64]) -> Vec<f32> {
    let max = bytes.iter().copied().max().unwrap_or(0);
    bytes
        .iter()
        .map(|&b| match (b, max) {
            (0, _) | (_, 0) => 0.0,
            _ => (b as f32 / max as f32).max(0.01),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_thing_the_service_sends_has_a_line_and_only_the_settings_cannot_be_emptied() {
        for token in ["index", "texts", "previews", "page", "state"] {
            let t = thing(token).expect(token);
            assert_eq!(t.act.danger, token != "state", "{token}");
            assert_eq!(t.act.ask.is_empty(), token == "state", "{token}");
        }
        assert!(thing("nonsense").is_none());
        assert!(refused("open").is_some());
        assert_eq!(refused(""), None);
        assert_eq!(refused("kept"), None);
        assert!(RESTART.ask.is_empty() && !SORT.danger);
    }

    #[test]
    fn shares_are_of_one_core_and_of_the_machine() {
        let a = Sample {
            uptime_ms: 10_000,
            service_ms: 1_000,
            busy: 100,
            total: 1_000,
        };
        let b = Sample {
            uptime_ms: 12_000,
            service_ms: 1_500,
            busy: 150,
            total: 1_400,
        };
        let s = shares(&a, &b).expect("two seconds apart");
        assert!((s.service - 25.0).abs() < 0.01, "{s:?}");
        assert!((s.machine - 12.5).abs() < 0.01, "{s:?}");
        // The same answer twice, and a service that started again in between.
        assert_eq!(shares(&a, &a), None);
        assert_eq!(shares(&b, &a), None);
    }

    #[test]
    fn uptime_is_said_in_its_two_largest_units() {
        assert_eq!(uptime(42_000), ("{a} s", 42, 0));
        assert_eq!(uptime(82_000), ("{a} min {b} s", 1, 22));
        assert_eq!(uptime(4_380_000), ("{a} h {b} min", 1, 13));
        assert_eq!(uptime(97_200_000), ("{a} d {b} h", 1, 3));
    }

    #[test]
    fn bars_are_shares_of_the_largest_and_never_vanish() {
        assert_eq!(bars(&[545, 0, 296]), vec![1.0, 0.0, 296.0 / 545.0]);
        assert_eq!(bars(&[1 << 30, 1]), vec![1.0, 0.01]);
        assert!(bars(&[]).is_empty());
        assert_eq!(bars(&[0, 0]), vec![0.0, 0.0]);
    }
}
