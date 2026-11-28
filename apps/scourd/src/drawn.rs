//! Drawings made lately, kept: the arrows come back to a file, and a row
//! drawn ahead of the hand (`Request::Flow` from a window warming the rows
//! beside the one it shows) is drawn once. Keyed by the file's path, size and
//! change time, so a file written since is drawn again; bounded by count and
//! by weight, the oldest let go first.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use scour_flow::{Block, Flow};

/// Drawings kept at most, and their weight in bytes.
const MOST: usize = 24;
const MOST_BYTES: usize = 96 << 20;
/// How long an ask waits for the same drawing being made on another
/// connection before making its own.
const WAIT: Duration = Duration::from_secs(10);

/// A file as it was when drawn.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) struct Key {
    pub path: String,
    pub size: i64,
    pub mtime: i64,
}

#[derive(Default)]
struct Held {
    kept: VecDeque<(Key, Arc<Flow>, usize)>,
    bytes: usize,
    /// Being drawn now, on some connection.
    making: Vec<Key>,
}

fn held() -> &'static (Mutex<Held>, Condvar) {
    static HELD: OnceLock<(Mutex<Held>, Condvar)> = OnceLock::new();
    HELD.get_or_init(|| (Mutex::new(Held::default()), Condvar::new()))
}

/// The drawing of `key`: kept, or made by `make` and kept — once, however
/// many ask at the same moment.
pub(crate) fn drawn(key: Key, make: impl FnOnce() -> Result<Flow, String>) -> Result<Flow, String> {
    let (lock, ready) = held();
    {
        let mut h = lock.lock().unwrap_or_else(|p| p.into_inner());
        let deadline = std::time::Instant::now() + WAIT;
        while h.making.contains(&key) {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                break;
            }
            h = ready
                .wait_timeout(h, left)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
        if let Some(i) = h.kept.iter().position(|(k, _, _)| *k == key) {
            // The most recent last: what is let go is what was looked at least lately.
            let entry = h.kept.remove(i).expect("found above");
            let flow = Flow::clone(&entry.1);
            h.kept.push_back(entry);
            return Ok(flow);
        }
        h.making.push(key.clone());
    }
    let made = make();
    let mut h = lock.lock().unwrap_or_else(|p| p.into_inner());
    h.making.retain(|k| *k != key);
    if let Ok(flow) = &made {
        let weight = weight(flow);
        if weight <= MOST_BYTES / 4 {
            h.bytes += weight;
            h.kept.push_back((key, Arc::new(flow.clone()), weight));
            while h.kept.len() > MOST || h.bytes > MOST_BYTES {
                let Some((_, _, w)) = h.kept.pop_front() else {
                    break;
                };
                h.bytes -= w;
            }
        }
    }
    ready.notify_all();
    made
}

/// What a drawing weighs, near enough: its words and its pictures.
fn weight(flow: &Flow) -> usize {
    fn block(b: &Block) -> usize {
        match b {
            Block::Para(p) => 64 + p.runs.iter().map(|r| r.text.len() + 32).sum::<usize>(),
            Block::Table(t) => t
                .rows
                .iter()
                .flat_map(|r| &r.cells)
                .flat_map(|c| &c.paras)
                .map(|p| 64 + p.text().len())
                .sum(),
            Block::Picture(p) => p.data.len() + 64,
            Block::Sheet(s) => s
                .rows
                .iter()
                .flat_map(|r| &r.cells)
                .map(|c| c.text.len() + 48)
                .sum(),
            Block::Slide(s) => s.blocks.iter().map(block).sum::<usize>() + 64,
            Block::Break => 16,
        }
    }
    flow.blocks.iter().map(block).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn key(path: &str, mtime: i64) -> Key {
        Key {
            path: path.into(),
            size: 1,
            mtime,
        }
    }

    #[test]
    fn a_drawing_is_made_once_and_again_when_the_file_changes() {
        let made = AtomicUsize::new(0);
        let make = || {
            made.fetch_add(1, Ordering::SeqCst);
            Ok(Flow::default())
        };
        let path = format!("/drawn-test-{}", std::process::id());
        drawn(key(&path, 1), make).unwrap();
        drawn(key(&path, 1), make).unwrap();
        assert_eq!(made.load(Ordering::SeqCst), 1);
        drawn(key(&path, 2), make).unwrap();
        assert_eq!(made.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn asked_at_once_it_is_still_made_once() {
        let made = Arc::new(AtomicUsize::new(0));
        let path = format!("/drawn-race-{}", std::process::id());
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let (made, path) = (Arc::clone(&made), path.clone());
                std::thread::spawn(move || {
                    drawn(key(&path, 1), || {
                        made.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(50));
                        Ok(Flow::default())
                    })
                })
            })
            .collect();
        for t in threads {
            assert!(t.join().unwrap().is_ok());
        }
        assert_eq!(made.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_failure_is_not_kept() {
        let made = AtomicUsize::new(0);
        let path = format!("/drawn-fail-{}", std::process::id());
        for _ in 0..2 {
            let _ = drawn(key(&path, 1), || {
                made.fetch_add(1, Ordering::SeqCst);
                Err("no".to_owned())
            });
        }
        assert_eq!(made.load(Ordering::SeqCst), 2);
    }
}
