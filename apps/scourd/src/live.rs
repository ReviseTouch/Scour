//! The engine in use, and what replaces it: the disks.
//!
//! An engine takes its sources once, so a disk chosen, left out, plugged in or
//! pulled out is a new engine over the same index. The old one stops first —
//! watches off, walk abandoned, committed — so two never write at once, and a
//! request waits behind the swap rather than meeting a stopped engine.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, RwLock};

use scour_core::Index;
use scour_engine::Engine;

use crate::disks::Plan;
use crate::handle::Kept;

pub struct Live {
    engine: RwLock<Arc<Engine>>,
    index: Arc<dyn Index>,
    /// What the engine was built from, and what every disk is said to be.
    plan: Mutex<Plan>,
    /// One swap at a time: the disk watcher and a saved answer can both ask.
    swapping: Mutex<()>,
}

/// Which sources a freshly started engine walks.
pub enum Walk {
    Nothing,
    All,
    Named(HashSet<String>),
}

impl Live {
    pub fn new(engine: Engine, index: Arc<dyn Index>, plan: Plan) -> Live {
        Live {
            engine: RwLock::new(Arc::new(engine)),
            index,
            plan: Mutex::new(plan),
            swapping: Mutex::default(),
        }
    }

    /// The engine to answer with. Held by the caller for one request: a swap
    /// waits for nobody, and a request still holding the old one finishes on it.
    pub fn engine(&self) -> Arc<Engine> {
        Arc::clone(&self.engine.read().unwrap_or_else(|p| p.into_inner()))
    }

    pub fn plan(&self) -> Plan {
        self.plan.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Disks waiting for an answer, for every status.
    pub fn asking(&self) -> u32 {
        self.plan.lock().unwrap_or_else(|p| p.into_inner()).asking()
    }

    /// Look at the disks again, after one came or went or somebody answered.
    /// A change only in what a face shows wakes the faces; a change in what is
    /// indexed replaces the engine.
    pub fn replan(&self, kept: &Kept) {
        let _one = self.swapping.lock().unwrap_or_else(|p| p.into_inner());
        let plan = kept.plan();
        let was = {
            let mut held = self.plan.lock().unwrap_or_else(|p| p.into_inner());
            if *held == plan {
                return;
            }
            std::mem::replace(&mut *held, plan.clone())
        };
        if was.sources == plan.sources {
            self.engine().nudge();
        } else {
            self.swap(kept, &plan);
        }
    }

    fn swap(&self, kept: &Kept, plan: &Plan) {
        let mut slot = self.engine.write().unwrap_or_else(|p| p.into_inner());
        let old = Arc::clone(&slot);
        // A walk cut short is started again, everywhere: which sources it had
        // reached is not known from outside. Otherwise only new sources walk.
        let walk = if old.walking() {
            Walk::All
        } else {
            let had: HashSet<String> = old.sources().into_iter().map(|s| s.name).collect();
            Walk::Named(
                plan.sources
                    .iter()
                    .map(|(s, _)| s.name.clone())
                    .filter(|n| !had.contains(n))
                    .collect(),
            )
        };
        let revision = old.status().revision + 1;
        old.shutdown();
        let new = Arc::new(crate::wire::engine(
            &kept.config,
            Arc::clone(&self.index),
            &plan.sources,
            revision,
        ));
        *slot = Arc::clone(&new);
        drop(slot);
        scour_core::note!(
            "scourd: the disks changed · {} sources now",
            new.sources().len()
        );
        start(new, walk);
    }

    /// Stop the engine in use, for the process to end.
    pub fn shutdown(&self) {
        self.engine().shutdown();
    }
}

/// Watch, then walk, on a thread of its own: the folder map a fanotify reader
/// needs takes seconds to build and no query needs it. Walking after watching,
/// because walking first leaves a window covered by neither.
pub fn start(engine: Arc<Engine>, walk: Walk) {
    std::thread::spawn(move || {
        // One call, because the order inside it is the invariant that a
        // test can hold on to. See `Engine::cover_then_walk`.
        let started = engine.cover_then_walk(matches!(walk, Walk::All));
        if let Walk::Named(names) = &walk {
            walk_named(&engine, names);
        }
        match started {
            Ok((n, skipped)) if skipped.is_empty() => {
                scour_core::note!("scourd: watching {n} source(s)");
            }
            Ok((n, skipped)) => {
                // Named, so it can be acted on, but briefly: one unreadable
                // tree produced 191 paths, and the shared prefix is the
                // useful part of them.
                scour_core::note!(
                    "scourd: watching {n} source(s); {} subtree(s) unreadable, under {}",
                    skipped.len(),
                    crate::common_prefix(&skipped)
                );
            }
            Err(e) => scour_core::note!("scourd: the first walk could not start: {e}"),
        }
    });
}

/// A whole walk of each of these sources.
fn walk_named(engine: &Engine, names: &HashSet<String>) {
    for name in names {
        if let Err(e) = engine.walk_source(name) {
            scour_core::note!("scourd: {name} could not be walked: {e}");
        }
    }
}

/// Wait for disks to come and go, for the life of the process. A burst — a
/// stick with three partitions — is let settle first, so it is one swap.
pub fn follow_disks(live: Arc<Live>, kept: Arc<Kept>) {
    std::thread::spawn(move || {
        let mut changes = scour_volumes::Changes::new();
        loop {
            if !changes.wait(std::time::Duration::from_secs(3600)) {
                continue;
            }
            while changes.wait(std::time::Duration::from_secs(1)) {}
            live.replan(&kept);
        }
    });
}
