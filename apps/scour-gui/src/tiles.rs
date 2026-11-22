//! Bounded worker pool turning cached PNGs and OS shell / Quick Look bitmaps
//! into direct RGBA pixel buffers for grid tiles, answered through [`Got::Tile`].

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use crate::link::Got;
use crate::pixels::{self, RgbaBuf};

const WORKERS: usize = 4;
const QUEUE_CAP: usize = 64;
const TILE_PX: u32 = 256;

#[derive(Debug, Clone)]
pub enum TileSource {
    CachedPng(PathBuf),
    DirectImage,
    Native,
}

#[derive(Clone)]
pub enum TileOutcome {
    Pixels(RgbaBuf),
    Nothing,
}

struct Entry {
    path: String,
    source: TileSource,
}

struct Shared {
    queue: Mutex<VecDeque<Entry>>,
    ready: Condvar,
}

impl Shared {
    fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::with_capacity(QUEUE_CAP)),
            ready: Condvar::new(),
        }
    }

    /// Enqueue items and return paths evicted before a worker started them.
    fn push(&self, items: Vec<Entry>) -> Vec<String> {
        let mut dropped = Vec::new();
        let mut q = self.queue.lock().unwrap_or_else(|p| p.into_inner());
        for item in items {
            if q.len() >= QUEUE_CAP
                && let Some(old) = q.pop_front()
            {
                dropped.push(old.path);
            }
            q.push_back(item);
        }
        self.ready.notify_all();
        dropped
    }

    fn pop(&self) -> Entry {
        let mut q = self.queue.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if let Some(item) = q.pop_front() {
                return item;
            }
            q = self.ready.wait(q).unwrap_or_else(|p| p.into_inner());
        }
    }
}

fn pool() -> &'static Arc<Shared> {
    static POOL: OnceLock<Arc<Shared>> = OnceLock::new();
    POOL.get_or_init(|| {
        let shared = Arc::new(Shared::new());
        for i in 0..WORKERS {
            let s = Arc::clone(&shared);
            let _ = std::thread::Builder::new()
                .name(format!("scour-tile-{i}"))
                .spawn(move || {
                    loop {
                        let entry = s.pop();
                        let buf = match &entry.source {
                            TileSource::CachedPng(png) => pixels::load_cached_png(png),
                            TileSource::DirectImage => {
                                let native = scour_core::path::to_path(&entry.path);
                                pixels::load_direct_image(&native, TILE_PX)
                            }
                            TileSource::Native => {
                                let native = scour_core::path::to_path(&entry.path);
                                scour_thumbs::native::picture_cached(&native, TILE_PX)
                                    .or_else(|| scour_thumbs::native::picture(&native, TILE_PX))
                                    .and_then(pixels::from_picture)
                            }
                        };
                        let outcome = match buf {
                            Some(px) => TileOutcome::Pixels(px),
                            None => TileOutcome::Nothing,
                        };
                        let path = entry.path;
                        let _ = slint::invoke_from_event_loop(move || {
                            crate::deliver(Got::Tile { path, outcome });
                        });
                    }
                });
        }
        shared
    })
}

/// Submit a batch of visible tiles for off-thread pixel decoding. Returns any
/// queued paths evicted because the viewport scrolled past [`QUEUE_CAP`].
pub fn submit(batch: Vec<(String, TileSource)>) -> Vec<String> {
    if batch.is_empty() {
        return Vec::new();
    }
    let entries = batch
        .into_iter()
        .map(|(path, source)| Entry { path, source })
        .collect();
    pool().push(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrolling_past_the_queue_cap_evicts_oldest_unstarted_tiles() {
        let shared = Shared::new();
        let items: Vec<Entry> = (0..(QUEUE_CAP + 3))
            .map(|i| Entry {
                path: format!("/p/{i}"),
                source: TileSource::Native,
            })
            .collect();
        let evicted = shared.push(items);
        assert_eq!(evicted, ["/p/0", "/p/1", "/p/2"]);
        assert_eq!(shared.pop().path, "/p/3");
    }
}
