//! The panel the status bar opens, as state: the last `Resources` answer, the
//! processor samples taken from two answers at a time, the one row whose
//! second step is open, what a press came to, and a restart under way. What
//! it draws is in `resource_lines` and `resource_gauges`.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use scour_i18n::Catalogue;
use scour_proto::{HeldKind, Request, Resources};
use scour_ui::resources::{self as says, Sample};
use slint::SharedString;

use crate::t;

/// Asked this often while the panel is open; nothing is asked while it is shut.
pub(crate) const EVERY: Duration = Duration::from_secs(2);
/// The sparkline's length: a minute of answers.
pub(crate) const SPARK: usize = 30;
/// How long a row says what its press came to.
const SAID_FOR: Duration = Duration::from_secs(3);
/// An ask still unanswered after this is given up, and asked again.
const GIVE_UP: Duration = Duration::from_secs(10);

/// What a press in the panel asks of the service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Press {
    Clear(HeldKind),
    Sort,
    Restart,
}

impl Press {
    pub(crate) fn request(self) -> Request {
        match self {
            Press::Clear(what) => Request::Clear { what },
            Press::Sort => Request::Maintain {
                level: scour_core::Maintenance::Rebuild,
            },
            Press::Restart => Request::Restart {},
        }
    }

    /// By the token a row carries: a kind's, `sort` or `restart`.
    pub(crate) fn named(token: &str) -> Option<Press> {
        match token {
            "sort" => Some(Press::Sort),
            "restart" => Some(Press::Restart),
            other => KINDS
                .into_iter()
                .find(|k| self::token(*k) == other)
                .map(Press::Clear),
        }
    }

    /// What the row says once the service took it.
    fn done(self) -> &'static str {
        match self {
            Press::Clear(kind) => says::thing(token(kind)).map_or("", |t| t.act.done),
            Press::Sort => says::SORT.done,
            Press::Restart => says::RESTART.done,
        }
    }
}

const KINDS: [HeldKind; 5] = [
    HeldKind::Index,
    HeldKind::Texts,
    HeldKind::Previews,
    HeldKind::Page,
    HeldKind::State,
];

/// The wire's token for a kind, which is `scour_ui`'s key for its line.
pub(crate) fn token(kind: HeldKind) -> &'static str {
    match kind {
        HeldKind::Index => "index",
        HeldKind::Texts => "texts",
        HeldKind::Previews => "previews",
        HeldKind::Page => "page",
        HeldKind::State => "state",
    }
}

/// What a press came to, said in its row for a moment.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Said {
    Done(&'static str),
    /// The service's own sentence.
    No(String),
}

/// A restart under way: the uptime of the last answer before it, and
/// whether the connection has gone since. Either tells the new service.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Restarting {
    up_for: u64,
    gone: bool,
}

#[derive(Default)]
pub(crate) struct Panel {
    pub(crate) open: bool,
    pub(crate) last: Option<Resources>,
    before: Option<Sample>,
    /// The service's share of one core, oldest first, at most [`SPARK`].
    pub(crate) spark: VecDeque<f32>,
    /// The machine's busy share between the last two answers.
    pub(crate) machine: Option<f32>,
    /// The one row whose second step is open.
    pub(crate) asking: Option<Press>,
    pub(crate) said: Option<(Press, Said, Instant)>,
    pub(crate) restarting: Option<Restarting>,
    /// When the ask now out was sent; one at a time.
    asked: Option<Instant>,
}

impl Panel {
    /// Whether to ask now: nothing is out, or what is out was given up on.
    pub(crate) fn due(&mut self, now: Instant) -> bool {
        let due = self
            .asked
            .is_none_or(|at| now.duration_since(at) >= GIVE_UP);
        if due {
            self.asked = Some(now);
        }
        due
    }

    /// An answer. True when it is the first from a service that restarted.
    pub(crate) fn took(&mut self, r: Resources) -> bool {
        self.asked = None;
        let now = Sample {
            uptime_ms: r.uptime_ms,
            service_ms: r.cpu.service_ms,
            busy: r.cpu.busy,
            total: r.cpu.total,
        };
        if let Some(before) = self.before
            && let Some(s) = says::shares(&before, &now)
        {
            if self.spark.len() == SPARK {
                self.spark.pop_front();
            }
            self.spark.push_back(s.service);
            self.machine = (r.cpu.total > 0).then_some(s.machine);
        }
        self.before = Some(now);
        let back = self
            .restarting
            .is_some_and(|at| at.gone || r.uptime_ms < at.up_for);
        if back {
            self.restarting = None;
        }
        self.last = Some(r);
        back
    }

    /// The ask out was refused: the next tick asks again.
    pub(crate) fn unanswered(&mut self) {
        self.asked = None;
    }

    /// The connection went: nothing is out any more, and a restart has begun.
    pub(crate) fn lost(&mut self) {
        self.asked = None;
        if let Some(at) = self.restarting.as_mut() {
            at.gone = true;
        }
    }

    /// Closed: the asks, the words and the line start again next time.
    pub(crate) fn shut(&mut self) {
        self.open = false;
        self.asking = None;
        self.said = None;
        self.spark.clear();
        self.before = None;
        self.machine = None;
        self.asked = None;
    }

    /// A row's button (`ask`), its second step (`go`), or `no`. True when
    /// the service is to be asked now.
    pub(crate) fn step(&mut self, press: Press, step: &str) -> bool {
        if self.refuses(press) {
            self.asking = None;
            return false;
        }
        match step {
            "ask" => {
                self.asking = (self.asking != Some(press)).then_some(press);
                self.said = None;
                false
            }
            "go" => {
                self.asking = None;
                self.said = None;
                if press == Press::Restart {
                    self.restarting = Some(Restarting {
                        up_for: self.last.as_ref().map_or(0, |r| r.uptime_ms),
                        gone: false,
                    });
                }
                true
            }
            _ => {
                self.asking = None;
                false
            }
        }
    }

    /// The service's answer to a press.
    pub(crate) fn answered(&mut self, press: Press, outcome: Result<(), String>, now: Instant) {
        match outcome {
            // `restarting` says it until the service is back.
            Ok(()) if press == Press::Restart => {}
            Ok(()) => self.said = Some((press, Said::Done(press.done()), now)),
            Err(why) => {
                if press == Press::Restart {
                    self.restarting = None;
                }
                self.said = Some((press, Said::No(why), now));
            }
        }
    }

    /// Whether what a row said has been said long enough, and is gone.
    pub(crate) fn expire(&mut self, now: Instant) -> bool {
        let over = self
            .said
            .as_ref()
            .is_some_and(|(_, _, at)| now.duration_since(*at) >= SAID_FOR);
        if over {
            self.said = None;
        }
        over
    }

    /// Whether the last answer says the service would refuse this now: the
    /// page's browser holds its profile.
    pub(crate) fn refuses(&self, press: Press) -> bool {
        let Press::Clear(kind) = press else {
            return false;
        };
        self.last.as_ref().is_some_and(|r| {
            r.files
                .iter()
                .any(|h| h.kind == kind && says::refused(&h.busy).is_some())
        })
    }

    /// The folder the last answer names for a kind.
    pub(crate) fn path_of(&self, kind: HeldKind) -> Option<String> {
        self.last
            .as_ref()?
            .files
            .iter()
            .find(|h| h.kind == kind)
            .map(|h| h.path.clone())
            .filter(|p| !p.is_empty())
    }

    /// What a press said, if it is this one's: the words, and whether a no.
    pub(crate) fn said_by(&self, cat: &Catalogue, press: Press) -> (SharedString, bool) {
        match &self.said {
            Some((p, Said::Done(id), _)) if *p == press => (t(cat, id), false),
            Some((p, Said::No(why), _)) if *p == press => (why.as_str().into(), true),
            _ => (SharedString::new(), false),
        }
    }
}

#[cfg(test)]
#[path = "resources_tests.rs"]
mod tests;
