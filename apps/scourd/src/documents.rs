//! The documents `scour-grep` cannot read itself — PDF, legacy Office,
//! e-mail — read by `scour-extractor` processes started here, and their text
//! kept in `scour-textcache` so a second search opens none of them.

pub mod ahead;
mod crypt;
mod pool;

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use parking_lot::RwLock;
use scour_extract::wire::Answer;
use scour_grep::{Doc, DocText, Texts};
use scour_textcache::{Kept, Key, Stats, Store};

use pool::Pool;

/// Kept when nobody said: a gigabyte of compressed text is a few hundred
/// thousand documents.
const DEFAULT_MB: u32 = 1024;
/// How often the cache is written out and its dead text weighed.
const TIDY_EVERY: Duration = Duration::from_secs(300);

static SHARED: OnceLock<Arc<Documents>> = OnceLock::new();

/// A failure is tried again when Scour itself changes, readers with it.
fn readers_version() -> u32 {
    env!("CARGO_PKG_VERSION")
        .bytes()
        .fold(2_166_136_261u32, |h, b| {
            (h ^ u32::from(b)).wrapping_mul(16_777_619)
        })
}

pub struct Documents {
    pool: Pool,
    keep: RwLock<Option<Keep>>,
}

struct Keep {
    store: Store,
    dir: PathBuf,
}

/// What the cache is and holds, for the `texts` request.
pub struct Held {
    pub on: bool,
    pub dir: String,
    pub max_mb: u32,
    pub stats: Stats,
}

/// The extractor beside this program, if it was shipped.
fn extractor() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let path = exe
        .parent()?
        .join(format!("scour-extractor{}", std::env::consts::EXE_SUFFIX));
    path.is_file().then_some(path)
}

/// Started once; None when no extractor is installed, and documents are then
/// counted as having no reader rather than as broken.
pub fn start(settings: &scour_settings::Settings, state_dir: &Path) -> Option<Arc<Documents>> {
    let exe = extractor()?;
    let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
    let documents = Arc::new(Documents {
        pool: Pool::new(exe, (cores / 4).clamp(1, 4)),
        keep: RwLock::new(None),
    });
    documents.configure(settings, state_dir);
    let tidy = Arc::downgrade(&documents);
    let _ = std::thread::Builder::new()
        .name("scourd-texts".into())
        .spawn(move || {
            loop {
                std::thread::sleep(TIDY_EVERY);
                let Some(d) = tidy.upgrade() else { break };
                d.tidy();
            }
        });
    let _ = SHARED.set(Arc::clone(&documents));
    Some(documents)
}

pub fn shared() -> Option<&'static Arc<Documents>> {
    SHARED.get()
}

impl Documents {
    /// The cache as the settings say: off, or in a place, at a size.
    pub fn configure(&self, settings: &scour_settings::Settings, state_dir: &Path) {
        let mut keep = self.keep.write();
        if settings.texts_off {
            *keep = None;
            return;
        }
        let dir = if settings.texts_dir.is_empty() {
            state_dir.join("texts")
        } else {
            PathBuf::from(&settings.texts_dir)
        };
        let mb = if settings.texts_max_mb == 0 {
            DEFAULT_MB
        } else {
            settings.texts_max_mb
        };
        if let Some(k) = keep.as_ref()
            && k.dir == dir
        {
            // Same place: only the size may have changed, and it is read at open.
            drop(keep);
            self.reopen(dir, mb);
            return;
        }
        drop(keep);
        self.reopen(dir, mb);
    }

    fn reopen(&self, dir: PathBuf, mb: u32) {
        let mut keep = self.keep.write();
        *keep = None;
        match Store::open(&dir, u64::from(mb) << 20) {
            Ok(store) => *keep = Some(Keep { store, dir }),
            Err(e) => scour_core::note!("scourd: documents' text will not be kept: {e}"),
        }
    }

    pub fn held(&self, settings: &scour_settings::Settings) -> Held {
        let keep = self.keep.read();
        Held {
            on: keep.is_some(),
            dir: keep
                .as_ref()
                .map(|k| k.dir.to_string_lossy().into_owned())
                .unwrap_or_default(),
            max_mb: if settings.texts_max_mb == 0 {
                DEFAULT_MB
            } else {
                settings.texts_max_mb
            },
            stats: keep.as_ref().map(|k| k.store.stats()).unwrap_or_default(),
        }
    }

    /// Forget every kept text.
    pub fn forget(&self) -> std::io::Result<()> {
        match self.keep.read().as_ref() {
            Some(k) => k.store.clear(),
            None => Ok(()),
        }
    }

    fn tidy(&self) {
        if let Some(k) = self.keep.read().as_ref() {
            let _ = k.store.flush();
            match k.store.compact() {
                Ok(true) => {
                    scour_core::note!("scourd: documents' text written again without the dead")
                }
                Ok(false) => {}
                Err(e) => scour_core::note!("scourd: documents' text could not be compacted: {e}"),
            }
        }
    }
}

fn key_of(doc: &Doc<'_>) -> Key {
    Key {
        path: doc.path.to_string_lossy().into_owned(),
        size: doc.size,
        mtime: doc.mtime,
    }
}

impl Texts for Documents {
    fn kept(&self, doc: &Doc<'_>) -> Option<DocText> {
        let keep = self.keep.read();
        match keep.as_ref()?.store.get(&key_of(doc), readers_version())? {
            Kept::Words(words) => Some(DocText::Words(words)),
            Kept::NoText => Some(DocText::NoText),
            Kept::Failed { .. } => Some(DocText::Broken),
        }
    }

    fn read(&self, doc: &Doc<'_>, bytes: &[u8]) -> DocText {
        let (text, kept) = match self.pool.extract(doc.extension, bytes) {
            Answer::Words(words) => {
                let words = scour_extract::composed(&words);
                (DocText::Words(words.clone()), Kept::Words(words))
            }
            Answer::NoText => (DocText::NoText, Kept::NoText),
            Answer::Failed(_) => (
                DocText::Broken,
                Kept::Failed {
                    version: readers_version(),
                },
            ),
        };
        if let Some(k) = self.keep.read().as_ref()
            && crypt::may_keep(doc.path, &k.dir)
            && let Err(e) = k.store.put(&key_of(doc), &kept)
        {
            scour_core::note!("scourd: a document's text could not be kept: {e}");
        }
        text
    }
}
