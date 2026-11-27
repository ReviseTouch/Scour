//! Latest-wins off-thread preview worker: multi-page scrollable PDF/Office
//! documents, EXIF-oriented JPEG/PNGs, and Windows/macOS native bitmaps.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::pixels::{self, RgbaBuf};
use crate::ui::PeekPage;
use crate::{MainWindow, rows};

const KEEP_RADIUS: usize = 8;
const AHEAD_PAGES: usize = 2;

pub struct Request<'a> {
    pub path: &'a str,
    pub is_image: bool,
    pub px: u32,
    pub waiting_on_service: bool,
    pub reading_note: String,
    pub nothing_note: String,
}

struct DocUi {
    seq: u64,
    path: String,
    pdf_path: PathBuf,
    px: u32,
    pages: usize,
    asked: HashSet<usize>,
    model: Rc<VecModel<PeekPage>>,
}

thread_local! {
    static DOC: RefCell<Option<DocUi>> = const { RefCell::new(None) };
}

enum Task {
    Open {
        seq: u64,
        path: String,
        is_image: bool,
        px: u32,
        /// Where the first find falls, as a share of the document's text;
        /// below zero when nothing was searched for.
        at: f32,
        waiting_on_service: bool,
        reading_note: String,
        nothing_note: String,
        weak: slint::Weak<MainWindow>,
    },
    Pages {
        seq: u64,
        path: String,
        pdf_path: PathBuf,
        indices: Vec<usize>,
        px: u32,
        weak: slint::Weak<MainWindow>,
    },
}

struct Queue {
    open_slot: Mutex<Option<Task>>,
    page_slot: Mutex<Option<Task>>,
    ready: Condvar,
    seq: AtomicU64,
}

impl Queue {
    fn new() -> Self {
        Self {
            open_slot: Mutex::new(None),
            page_slot: Mutex::new(None),
            ready: Condvar::new(),
            seq: AtomicU64::new(0),
        }
    }

    fn push_open(&self, make: impl FnOnce(u64) -> Task) -> u64 {
        let seq = self.seq.fetch_add(1, Ordering::AcqRel) + 1;
        *self.page_slot.lock().unwrap_or_else(|p| p.into_inner()) = None;
        *self.open_slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(make(seq));
        self.ready.notify_one();
        seq
    }

    fn push_pages(&self, task: Task) {
        *self.page_slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(task);
        self.ready.notify_one();
    }

    fn take(&self) -> Task {
        let mut open = self.open_slot.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if let Some(t) = open.take() {
                return t;
            }
            if let Some(t) = self
                .page_slot
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take()
            {
                return t;
            }
            open = self.ready.wait(open).unwrap_or_else(|p| p.into_inner());
        }
    }

    fn is_current(&self, seq: u64) -> bool {
        self.seq.load(Ordering::Acquire) == seq
    }
}

fn page_to_buf(img: scour_preview::doc::PageImage) -> Option<RgbaBuf> {
    let expected = (img.width as usize)
        .checked_mul(img.height as usize)?
        .checked_mul(4)?;
    (img.width > 0 && img.height > 0 && img.rgba.len() == expected)
        .then(|| RgbaBuf::clone_from_slice(&img.rgba, img.width, img.height))
}

fn resolve_pixels(path: &str, is_image: bool, px: u32) -> Option<RgbaBuf> {
    if let Some(sharp) = scour_thumbs::cache::existing_for(path, px)
        && let Some(buf) = pixels::load_cached_png(&sharp)
    {
        return Some(buf);
    }
    let native = scour_core::path::to_path(path);
    if is_image && let Some(buf) = pixels::load_direct_image(&native, px) {
        return Some(buf);
    }
    if scour_thumbs::native::available()
        && let Some(buf) = scour_thumbs::native::picture(&native, px).and_then(pixels::from_picture)
    {
        return Some(buf);
    }
    scour_thumbs::cache::existing(path).and_then(|p| pixels::load_cached_png(&p))
}

fn run_open(
    q: &Queue,
    seq: u64,
    path: String,
    is_image: bool,
    px: u32,
    at: f32,
    waiting: bool,
    reading: String,
    nothing: String,
    weak: slint::Weak<MainWindow>,
) {
    let native = scour_core::path::to_path(&path);
    if scour_preview::doc::is_paged(&native)
        && let Some(doc) = scour_preview::doc::prepare(&native)
    {
        if !q.is_current(seq) {
            return;
        }
        // The two pages the view opens on: where the find is, if there is one.
        let pages = doc.info.pages.max(1);
        let start = first_page(at, pages);
        let last = (start + 1).min(pages);
        let mut initial = Vec::with_capacity(2);
        for p in start..=last {
            if !q.is_current(seq) {
                return;
            }
            if let Some(buf) =
                scour_preview::doc::render_page(&doc.pdf_path, p, px).and_then(page_to_buf)
            {
                initial.push(((p - 1) as usize, buf));
            }
        }
        if !initial.is_empty() {
            let _ = slint::invoke_from_event_loop(move || {
                install_doc(&weak, seq, path, doc.pdf_path, doc.info, px, initial);
            });
            return;
        }
    }
    if !q.is_current(seq) {
        return;
    }
    let got = resolve_pixels(&path, is_image, px);
    // Its own pixels, under it: from the header, which is all that is read.
    let said = is_image
        .then(|| image::image_dimensions(&native).ok())
        .flatten()
        .map(|(wd, ht)| scour_ui::preview::picture_said(wd, ht, scour_ui::path::leaf(&path)));
    if !q.is_current(seq) {
        return;
    }
    let _ = slint::invoke_from_event_loop(move || {
        let Some(w) = weak.upgrade() else { return };
        if w.get_peek_path().as_str() != path {
            return;
        }
        match got {
            Some(buf) => {
                w.set_peek_shot(slint::Image::from_rgba8(buf));
                w.set_peek_has_shot(true);
                w.set_peek_note(slint::SharedString::new());
                w.set_peek_shot_said(said.unwrap_or_default().into());
            }
            None if w.get_peek_text().is_empty() => {
                w.set_peek_note((if waiting { reading } else { nothing }).into());
            }
            None => {}
        }
    });
}

fn install_doc(
    weak: &slint::Weak<MainWindow>,
    seq: u64,
    path: String,
    pdf_path: PathBuf,
    info: scour_preview::doc::DocInfo,
    px: u32,
    initial: Vec<(usize, RgbaBuf)>,
) {
    let Some(w) = weak.upgrade() else { return };
    if w.get_peek_path().as_str() != path {
        return;
    }
    let total = info.pages as usize;
    let mut items: Vec<PeekPage> = (0..total)
        .map(|i| PeekPage {
            number: (i + 1) as i32,
            ready: false,
            shot: rows::blank(),
        })
        .collect();
    let mut asked = HashSet::new();
    for (idx, buf) in initial {
        if let Some(slot) = items.get_mut(idx) {
            slot.ready = true;
            slot.shot = slint::Image::from_rgba8(buf);
            asked.insert(idx);
        }
    }
    let model = Rc::new(VecModel::from(items));
    w.set_peek_aspect(info.aspect);
    w.set_peek_pages(ModelRc::from(Rc::clone(&model)));
    w.set_peek_note(slint::SharedString::new());
    DOC.with(|cell| {
        *cell.borrow_mut() = Some(DocUi {
            seq,
            path,
            pdf_path,
            px,
            pages: total,
            asked,
            model,
        });
    });
}

fn worker() -> &'static Arc<Queue> {
    static WORKER: OnceLock<Arc<Queue>> = OnceLock::new();
    WORKER.get_or_init(|| {
        let queue = Arc::new(Queue::new());
        let q = Arc::clone(&queue);
        let _ = std::thread::Builder::new()
            .name("scour-peek".into())
            .spawn(move || {
                loop {
                    match q.take() {
                        Task::Open {
                            seq,
                            path,
                            is_image,
                            px,
                            at,
                            waiting_on_service,
                            reading_note,
                            nothing_note,
                            weak,
                        } => {
                            if q.is_current(seq) {
                                run_open(
                                    &q,
                                    seq,
                                    path,
                                    is_image,
                                    px,
                                    at,
                                    waiting_on_service,
                                    reading_note,
                                    nothing_note,
                                    weak,
                                );
                            }
                        }
                        Task::Pages {
                            seq,
                            path,
                            pdf_path,
                            indices,
                            px,
                            weak,
                        } => {
                            for idx in indices {
                                if !q.is_current(seq) {
                                    break;
                                }
                                let Some(buf) = scour_preview::doc::render_page(
                                    &pdf_path,
                                    (idx + 1) as u32,
                                    px,
                                )
                                .and_then(page_to_buf) else {
                                    continue;
                                };
                                if !q.is_current(seq) {
                                    break;
                                }
                                let p = path.clone();
                                let w = weak.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    land_page(&w, seq, &p, idx, buf);
                                });
                            }
                        }
                    }
                }
            });
        queue
    })
}

fn land_page(weak: &slint::Weak<MainWindow>, seq: u64, path: &str, idx: usize, buf: RgbaBuf) {
    let Some(w) = weak.upgrade() else { return };
    if w.get_peek_path().as_str() != path {
        return;
    }
    DOC.with(|cell| {
        let Some(doc) = cell.borrow().as_ref().cloned_model(seq) else {
            return;
        };
        doc.set_row_data(
            idx,
            PeekPage {
                number: (idx + 1) as i32,
                ready: true,
                shot: slint::Image::from_rgba8(buf),
            },
        );
    });
}

trait ClonedModel {
    fn cloned_model(&self, seq: u64) -> Option<Rc<VecModel<PeekPage>>>;
}

impl ClonedModel for Option<&DocUi> {
    fn cloned_model(&self, seq: u64) -> Option<Rc<VecModel<PeekPage>>> {
        let d = (*self)?;
        (d.seq == seq).then(|| Rc::clone(&d.model))
    }
}

/// Clear any active multi-page document session on the UI thread.
pub fn clear(w: &MainWindow) {
    crate::flowing::clear(w);
    DOC.with(|cell| *cell.borrow_mut() = None);
    if w.get_peek_pages().row_count() > 0 {
        w.set_peek_pages(ModelRc::default());
    }
}

/// Render newly visible pages and release distant page bitmaps on scroll.
pub fn scrolled(w: &MainWindow, center: usize) {
    let task = DOC.with(|cell| {
        let mut slot = cell.borrow_mut();
        let doc = slot.as_mut()?;
        let mut release = Vec::new();
        for &idx in &doc.asked {
            if idx.abs_diff(center) > KEEP_RADIUS {
                release.push(idx);
            }
        }
        for idx in release {
            doc.asked.remove(&idx);
            doc.model.set_row_data(
                idx,
                PeekPage {
                    number: (idx + 1) as i32,
                    ready: false,
                    shot: rows::blank(),
                },
            );
        }
        let end = (center + AHEAD_PAGES).min(doc.pages.saturating_sub(1));
        let mut want = Vec::new();
        for idx in center..=end {
            if doc.asked.insert(idx) {
                want.push(idx);
            }
        }
        (!want.is_empty()).then(|| Task::Pages {
            seq: doc.seq,
            path: doc.path.clone(),
            pdf_path: doc.pdf_path.clone(),
            indices: want,
            px: doc.px,
            weak: w.as_weak(),
        })
    });
    if let Some(t) = task {
        worker().push_pages(t);
    }
}

/// The page, counted from one, that holds `at` (a share of the text), or the
/// first when there is no find.
fn first_page(at: f32, pages: u32) -> u32 {
    if at < 0.0 {
        return 1;
    }
    ((at * pages as f32) as u32 + 1).clamp(1, pages)
}

/// Resolve the visual preview of `req.path` off the drawing thread.
pub fn request(w: &MainWindow, req: Request<'_>) {
    let path = req.path.to_owned();
    let is_image = req.is_image;
    let px = req.px;
    let at = w.get_peek_doc_at();
    let waiting_on_service = req.waiting_on_service;
    let reading_note = req.reading_note;
    let nothing_note = req.nothing_note;
    let weak = w.as_weak();
    worker().push_open(|seq| Task::Open {
        seq,
        path,
        is_image,
        px,
        at,
        waiting_on_service,
        reading_note,
        nothing_note,
        weak,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_page_is_the_one_the_find_falls_on() {
        assert_eq!(first_page(-1.0, 5), 1);
        assert_eq!(first_page(0.0, 5), 1);
        assert_eq!(first_page(0.53, 5), 3);
        assert_eq!(first_page(0.999, 5), 5);
        assert_eq!(first_page(1.0, 5), 5);
        assert_eq!(first_page(0.5, 1), 1);
    }

    #[test]
    fn a_newer_open_supersedes_pending_open_and_pages() {
        let q = Queue::new();
        let s1 = q.push_open(|seq| Task::Open {
            seq,
            path: "/a.pdf".into(),
            is_image: false,
            px: 480,
            at: -1.0,
            waiting_on_service: false,
            reading_note: String::new(),
            nothing_note: String::new(),
            weak: slint::Weak::default(),
        });
        q.push_pages(Task::Pages {
            seq: s1,
            path: "/a.pdf".into(),
            pdf_path: PathBuf::from("/a.pdf"),
            indices: vec![2, 3],
            px: 480,
            weak: slint::Weak::default(),
        });
        let s2 = q.push_open(|seq| Task::Open {
            seq,
            path: "/b.docx".into(),
            is_image: false,
            px: 480,
            at: -1.0,
            waiting_on_service: false,
            reading_note: String::new(),
            nothing_note: String::new(),
            weak: slint::Weak::default(),
        });
        assert!(!q.is_current(s1));
        assert!(q.is_current(s2));
        match q.take() {
            Task::Open { seq, path, .. } => {
                assert_eq!(seq, s2);
                assert_eq!(path, "/b.docx");
            }
            Task::Pages { .. } => panic!("stale pages should have been cleared"),
        }
    }
}
