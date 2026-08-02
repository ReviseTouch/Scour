//! The index itself.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use parking_lot::{Mutex, RwLock};
use scour_core::text::{DefaultFolder, Folder};
use scour_core::{
    ApplyReport, Change, Entry, Error, Facet, FacetBy, FacetRequest, FacetResponse, Hit, Index,
    IndexStats, Kind, MaintReport, Maintenance, Meta, Result, SearchRequest, SearchResponse,
    SortKey,
};
use tantivy::query::{EnableScoring, Query};
use tantivy::schema::Value;
use tantivy::{
    DocId, DocSet, Order, SegmentReader, TERMINATED, TantivyDocument, Term, collector::TopDocs,
    columnar::Column, doc,
};

use crate::lower::{Lowered, lower};
use crate::schema::{
    IndexOptions, Row, build_schema, eid_bytes, eid_from_bytes, eid_hash, field,
    register_tokenizers,
};

/// Bookkeeping that tantivy does not keep for us.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct Meta0 {
    opts: IndexOptions,
    /// Segments written by a rebuild, and therefore ordered newest-first.
    sorted_segments: Vec<String>,
}

/// Live state that does not survive a restart.
#[derive(Debug, Default)]
struct Pending {
    /// Digests of entries removed but not yet committed.
    hidden: HashSet<u64>,
    /// Subtrees removed but not yet committed. Checked against the stored path,
    /// so it costs nothing until a row is actually materialised.
    hidden_prefixes: Vec<String>,
    /// Documents added since the last rebuild. Every query reads all of them.
    tail: u64,
}

pub struct TantivyIndex {
    dir: PathBuf,
    index: tantivy::Index,
    reader: tantivy::IndexReader,
    /// One writer, behind one lock.
    ///
    /// Not merely for safety: on Windows two concurrent `commit()` calls race
    /// on the atomic rename of `.managed.json` and one of them fails with
    /// `PermissionDenied` (tantivy #2847). Serialising here is the workaround,
    /// and it costs nothing, because writing is a single background job by
    /// design.
    writer: Mutex<tantivy::IndexWriter<TantivyDocument>>,
    meta: RwLock<Meta0>,
    pending: RwLock<Pending>,
    opts: IndexOptions,
}

const META_FILE: &str = "scour-index.json";

// Hand-written: neither `IndexReader` nor `IndexWriter` is `Debug`, and dumping
// them would not say anything useful anyway. What a reader wants here is where
// the index is and how it was built.
impl std::fmt::Debug for TantivyIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TantivyIndex")
            .field("dir", &self.dir)
            .field("options", &self.opts)
            .field("pending", &*self.pending.read())
            .finish()
    }
}

impl TantivyIndex {
    /// Open an existing index, or create one with these options.
    ///
    /// The options of an existing index win: they describe what is on disk, and
    /// disagreeing with that silently would produce an index that answers
    /// questions it has no data for.
    pub fn open_or_create(dir: &Path, opts: IndexOptions) -> Result<Self> {
        if dir.join("meta.json").exists() {
            Self::open(dir)
        } else {
            Self::create(dir, opts)
        }
    }

    pub fn create(dir: &Path, opts: IndexOptions) -> Result<Self> {
        std::fs::create_dir_all(dir).map_err(|e| Error::io(&e, &dir.to_string_lossy()))?;
        let index = tantivy::Index::create_in_dir(dir, build_schema(&opts)).map_err(tv)?;
        register_tokenizers(&index);
        let me = Self::wrap(
            index,
            dir.to_owned(),
            Meta0 {
                opts,
                sorted_segments: Vec::new(),
            },
        )?;
        me.save_meta()?;
        Ok(me)
    }

    pub fn open(dir: &Path) -> Result<Self> {
        let index = tantivy::Index::open_in_dir(dir).map_err(tv)?;
        register_tokenizers(&index);
        let meta: Meta0 = std::fs::read_to_string(dir.join(META_FILE))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let me = Self::wrap(index, dir.to_owned(), meta)?;
        // Everything outside the recorded sorted set is tail.
        me.pending.write().tail = me.tail_docs();
        Ok(me)
    }

    fn wrap(index: tantivy::Index, dir: PathBuf, meta: Meta0) -> Result<Self> {
        let opts = meta.opts;
        let writer = index
            .writer_with_num_threads(1, opts.writer_heap_mb * 1024 * 1024)
            .map_err(tv)?;
        // Automatic merges concatenate segments in arrival order, which would
        // quietly destroy the newest-first ordering the fast path depends on —
        // and nothing would report an error; searches would simply return the
        // wrong page. Compaction is `Maintenance::Rebuild`.
        writer.set_merge_policy(Box::new(tantivy::merge_policy::NoMergePolicy));
        let reader = index.reader().map_err(tv)?;
        Ok(Self {
            dir,
            index,
            reader,
            writer: Mutex::new(writer),
            meta: RwLock::new(meta),
            pending: RwLock::new(Pending::default()),
            opts,
        })
    }

    fn save_meta(&self) -> Result<()> {
        let json = serde_json::to_string_pretty(&*self.meta.read()).map_err(|e| Error::Io {
            detail: e.to_string(),
        })?;
        std::fs::write(self.dir.join(META_FILE), json).map_err(|e| Error::io(&e, META_FILE))
    }

    pub fn options(&self) -> IndexOptions {
        self.opts
    }

    fn field(&self, name: &str) -> Result<tantivy::schema::Field> {
        self.index
            .schema()
            .get_field(name)
            .map_err(|_| Error::IndexCorrupt {
                detail: format!("missing field {name}"),
            })
    }

    /// Documents living in segments that no rebuild produced.
    fn tail_docs(&self) -> u64 {
        let sorted = &self.meta.read().sorted_segments;
        self.reader
            .searcher()
            .segment_readers()
            .iter()
            .filter(|s| !sorted.contains(&s.segment_id().uuid_string()))
            .map(|s| s.num_docs() as u64)
            .sum()
    }

    fn add(&self, w: &mut tantivy::IndexWriter<TantivyDocument>, e: &Entry) -> Result<()> {
        let r = Row::build(e);
        let s = self.index.schema();
        let g = |n: &str| s.get_field(n).expect("schema built by this crate");

        let mut d = doc!(
            g(field::EID)        => r.eid.clone(),
            g(field::EID_HASH)   => r.eid_hash,
            g(field::NAME_NORM)  => r.name_norm.as_str(),
            g(field::PATH_EXACT) => e.path.as_str(),
            g(field::NAME)       => r.name.as_str(),
            g(field::PATH)       => r.path.as_str(),
            g(field::NAME_SORT)  => r.name_norm.as_str(),
            g(field::PARENT_SORT)=> r.parent.as_str(),
            g(field::EXT)        => r.ext.as_str(),
            g(field::MTIME)      => e.meta.mtime,
            g(field::CTIME)      => e.meta.ctime,
            g(field::ATIME)      => e.meta.atime,
            g(field::SIZE)       => e.meta.size,
            g(field::DISK)       => e.meta.disk,
            g(field::KIND)       => r.kind.as_u8() as i64,
            g(field::IS_DIR)     => i64::from(e.is_dir),
            g(field::MODE)       => e.meta.mode,
            g(field::UID)        => e.meta.uid,
            g(field::GID)        => e.meta.gid,
            g(field::ITEMS)      => e.meta.items,
        );
        if self.opts.index_paths {
            d.add_text(g(field::PATH_NORM), &r.path_norm);
        }
        // Every ancestor as its own token: deleting a subtree is then one term
        // rather than a walk. Measured at 378,100 documents marked in 1.3 µs.
        let dirs = g(field::DIRS);
        for a in Row::ancestors(&e.path) {
            d.add_text(dirs, a);
        }
        w.add_document(d).map_err(tv)?;
        Ok(())
    }

    /// Rewrite the whole index newest-first, folding in everything added since
    /// the last rebuild.
    ///
    /// Self-contained: the stored path and name plus the fast columns are
    /// enough to reconstruct every entry, so this needs no help from the source
    /// that produced them.
    fn rebuild(&self) -> Result<()> {
        let searcher = self.reader.searcher();
        // Only the sort key and the address are collected here. Materialising
        // rows first would cost an order of magnitude more memory for no reason.
        let mut order: Vec<(i64, u32, DocId)> = Vec::with_capacity(searcher.num_docs() as usize);
        for (ord, seg) in searcher.segment_readers().iter().enumerate() {
            let mtime: Column<i64> = seg.fast_fields().i64(field::MTIME).map_err(tv)?;
            let alive = seg.alive_bitset();
            for doc in 0..seg.max_doc() {
                if alive.is_some_and(|b| !b.is_alive(doc)) {
                    continue;
                }
                order.push((mtime.first(doc).unwrap_or(0), ord as u32, doc));
            }
        }
        order.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(b.2.cmp(&a.2)));

        let mut w = self.writer.lock();
        w.delete_all_documents().map_err(tv)?;
        for (_, ord, doc) in &order {
            if let Some(e) = self.entry_at(&searcher, *ord, *doc) {
                self.add(&mut w, &e)?;
            }
        }
        w.commit().map_err(tv)?;
        drop(w);
        self.reader.reload().map_err(tv)?;

        // Every segment that exists now came out of this rebuild.
        {
            let mut meta = self.meta.write();
            meta.sorted_segments = self
                .reader
                .searcher()
                .segment_readers()
                .iter()
                .map(|s| s.segment_id().uuid_string())
                .collect();
        }
        self.save_meta()?;
        let mut p = self.pending.write();
        p.hidden.clear();
        p.hidden_prefixes.clear();
        p.tail = 0;
        Ok(())
    }

    /// Reconstruct a full entry from what is stored.
    fn entry_at(&self, searcher: &tantivy::Searcher, seg: u32, doc: DocId) -> Option<Entry> {
        let d: TantivyDocument = searcher.doc(tantivy::DocAddress::new(seg, doc)).ok()?;
        let s = self.index.schema();
        let text = |n: &str| {
            s.get_field(n)
                .ok()
                .and_then(|f| d.get_first(f))
                .and_then(|v| v.as_str())
                .unwrap_or("")
        };
        let bytes = |n: &str| {
            s.get_field(n)
                .ok()
                .and_then(|f| d.get_first(f))
                .and_then(|v| v.as_bytes())
        };
        let id = eid_from_bytes(bytes(field::EID)?)?;
        let sr = searcher.segment_reader(seg);
        let ff = sr.fast_fields();
        let i = |n: &str| {
            ff.i64(n)
                .ok()
                .and_then(|c: Column<i64>| c.first(doc))
                .unwrap_or(0)
        };
        let is_dir = i(field::IS_DIR) != 0;
        Some(Entry {
            id,
            path: text(field::PATH).to_owned(),
            is_dir,
            meta: Meta {
                size: i(field::SIZE),
                mtime: i(field::MTIME),
                ctime: i(field::CTIME),
                atime: i(field::ATIME),
                mode: i(field::MODE),
                uid: i(field::UID),
                gid: i(field::GID),
                disk: i(field::DISK),
                items: i(field::ITEMS),
            },
        })
    }
}

/// The column handles of one segment, opened once and reused for every row.
///
/// Opening them per row is the single easiest way to make this look slow:
/// 200 rows times five columns is a thousand opens, which measured 13 ms —
/// sixty times the actual work.
struct SegCols {
    eid_hash: Column<u64>,
    mtime: Column<i64>,
}

impl SegCols {
    fn open(seg: &SegmentReader) -> Result<Self> {
        let ff = seg.fast_fields();
        Ok(Self {
            eid_hash: ff.u64(field::EID_HASH).map_err(tv)?,
            mtime: ff.i64(field::MTIME).map_err(tv)?,
        })
    }
}

impl Index for TantivyIndex {
    fn apply(&self, changes: &mut dyn Iterator<Item = Change>) -> Result<ApplyReport> {
        let mut w = self.writer.lock();
        let mut p = self.pending.write();
        let mut report = ApplyReport::default();
        let f_eid = self.field(field::EID)?;
        let f_dirs = self.field(field::DIRS)?;
        let f_path = self.field(field::PATH_EXACT)?;

        for c in changes {
            match c {
                Change::Upsert(e) => {
                    w.delete_term(Term::from_field_bytes(f_eid, &eid_bytes(&e.id)));
                    // A file that was hidden and has come back must stop being
                    // hidden, or the row the user just created stays invisible.
                    p.hidden.remove(&eid_hash(&e.id));
                    self.add(&mut w, &e)?;
                    p.tail += 1;
                    report.upserted += 1;
                }
                Change::Remove(id) => {
                    p.hidden.insert(eid_hash(&id));
                    w.delete_term(Term::from_field_bytes(f_eid, &eid_bytes(&id)));
                    report.removed += 1;
                }
                Change::RemoveSubtree { path } => {
                    // Two terms, not one. The ancestor token covers the
                    // *contents*; the directory's own record lists its parents,
                    // not itself, so it has to be addressed by path as well.
                    w.delete_term(Term::from_field_text(f_dirs, &path));
                    w.delete_term(Term::from_field_text(f_path, &path));
                    p.hidden_prefixes.push(path);
                    report.subtrees_removed += 1;
                }
                // Reconciling a rescan is the engine's job: it walks the
                // subtree again and sends what it finds. Nothing to do here.
                Change::Rescan { .. } => {}
            }
        }
        Ok(report)
    }

    fn commit(&self) -> Result<()> {
        self.writer.lock().commit().map_err(tv)?;
        self.reader.reload().map_err(tv)?;
        let mut p = self.pending.write();
        p.hidden.clear();
        p.hidden_prefixes.clear();
        Ok(())
    }

    fn search(&self, req: &SearchRequest) -> Result<SearchResponse> {
        let started = Instant::now();
        let searcher = self.reader.searcher();
        let plan = lower(&req.query, &self.index.schema(), &self.opts)?;
        let pending = self.pending.read();

        let cols: Vec<SegCols> = searcher
            .segment_readers()
            .iter()
            .map(SegCols::open)
            .collect::<Result<_>>()?;
        let f_path = self.field(field::PATH)?;
        let f_name = self.field(field::NAME)?;
        let f_eid = self.field(field::EID)?;

        // Materialise one row. Numbers come from the columns, text from the
        // document store — 0.32 µs a row against 14.33 µs for a string column.
        let read_hit = |seg: u32, doc: DocId| -> Option<Hit> {
            let d: TantivyDocument = searcher.doc(tantivy::DocAddress::new(seg, doc)).ok()?;
            let path = d
                .get_first(f_path)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned();
            if pending.hidden_prefixes.iter().any(|p| under(&path, p)) {
                return None;
            }
            let name = d
                .get_first(f_name)
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if !plan.is_exact() && !plan.accepts(&DefaultFolder.fold(name)) {
                return None;
            }
            let id = eid_from_bytes(d.get_first(f_eid).and_then(|v| v.as_bytes())?)?;
            // Removed a moment ago and not yet committed. Checked here rather
            // than only in the walk below, so that every route into a result
            // set honours it — the ordinary sorted path goes through tantivy's
            // collector and never sees the walk at all.
            if pending.hidden.contains(&eid_hash(&id)) {
                return None;
            }
            let sr = searcher.segment_reader(seg);
            let ff = sr.fast_fields();
            let i = |n: &str| {
                ff.i64(n)
                    .ok()
                    .and_then(|c: Column<i64>| c.first(doc))
                    .unwrap_or(0)
            };
            Some(Hit {
                id,
                is_dir: i(field::IS_DIR) != 0,
                kind: Kind::from_u8(i(field::KIND) as u8).unwrap_or(Kind::File),
                meta: Meta {
                    size: i(field::SIZE),
                    mtime: i(field::MTIME),
                    ctime: i(field::CTIME),
                    atime: i(field::ATIME),
                    mode: i(field::MODE),
                    uid: i(field::UID),
                    gid: i(field::GID),
                    disk: i(field::DISK),
                    items: i(field::ITEMS),
                },
                path,
            })
        };

        let limit = req.page.limit as usize;
        let offset = req.page.offset as usize;
        let cap = req.page.count_cap.max(req.page.limit) as usize;
        let want = limit + offset;

        // The fast path applies to exactly one view: newest first, over an
        // index whose every segment a rebuild produced. Everything else is
        // correct by the ordinary route and pays for it.
        let sorted: HashSet<String> = self.meta.read().sorted_segments.iter().cloned().collect();
        let all_sorted = searcher
            .segment_readers()
            .iter()
            .all(|s| sorted.contains(&s.segment_id().uuid_string()));
        let fast = req.sort == SortKey::Modified && req.descending && all_sorted;

        if !fast {
            let total = self.count_upto(&plan, &searcher, &read_hit, cap)?;
            let hits = self.top_k(&searcher, &plan, req, &read_hit, want)?;
            return Ok(SearchResponse {
                hits: hits.into_iter().skip(offset).take(limit).collect(),
                total: total.min(cap) as u64,
                capped: total >= cap,
                took_us: started.elapsed().as_micros() as u64,
                fast_path: false,
            });
        }

        // One walk produces both the page and the count.
        //
        // Only the sort key and the address are collected: reading a numeric
        // column is ~0.1 µs while materialising a row costs far more, so the
        // survivors are materialised and nothing else is.
        let weight = plan
            .query
            .weight(EnableScoring::disabled_from_searcher(&searcher))
            .map_err(tv)?;
        let mut cands: Vec<(i64, u32, DocId)> = Vec::with_capacity(want * 2);
        let mut counted = 0usize;
        for (ord, seg) in searcher.segment_readers().iter().enumerate() {
            let c = &cols[ord];
            let seg_sorted = sorted.contains(&seg.segment_id().uuid_string());
            // Driving the DocSet by hand skips the layer that filters deleted
            // documents: `Weight::scorer` yields every match, alive or not, and
            // it is the collector that consults the alive bitset. Bypassing the
            // collector makes that filtering ours — and forgetting it means
            // deleted files keep appearing while the index insists they are
            // gone.
            let alive = seg.alive_bitset();
            let mut scorer = weight.scorer(seg, 1.0).map_err(tv)?;
            let mut doc = scorer.doc();
            let mut taken = 0usize;
            // The timestamp of the page's last candidate. Everything sharing it
            // is still in contention once the tie-break is applied.
            let mut boundary: Option<i64> = None;
            while doc != TERMINATED {
                if alive.is_some_and(|b| !b.is_alive(doc)) {
                    doc = scorer.advance();
                    continue;
                }
                let h = c.eid_hash.first(doc).unwrap_or(0);
                if pending.hidden.contains(&h) {
                    doc = scorer.advance();
                    continue;
                }
                counted += 1;
                let mt = c.mtime.first(doc).unwrap_or(0);
                if !seg_sorted || taken < want {
                    // An ordered segment's first `want` matches are its newest
                    // `want`. An unsorted one has no such guarantee: cutting it
                    // at page size would keep the matches indexed *earliest*,
                    // which are the oldest — so a file created a second ago
                    // would silently never reach the top of the list.
                    cands.push((mt, ord as u32, doc));
                    taken += 1;
                    if seg_sorted && taken == want {
                        boundary = Some(mt);
                    }
                } else if boundary == Some(mt) {
                    // Still inside the tie group at the page boundary. With 115
                    // files per timestamp on a real filesystem, a page is
                    // routinely one single timestamp, and stopping mid-group
                    // would return an arbitrary subset of it.
                    cands.push((mt, ord as u32, doc));
                }
                if seg_sorted && boundary.is_some_and(|b| mt < b) && counted >= cap {
                    break;
                }
                doc = scorer.advance();
            }
        }

        // Narrow to the rows that can still make the page before paying to
        // materialise any of them, keeping whole tie groups intact.
        cands.sort_unstable_by_key(|c| std::cmp::Reverse(c.0));
        if cands.len() > want + TIE_SLACK {
            let bound = cands[want + TIE_SLACK - 1].0;
            let keep = cands
                .iter()
                .position(|c| c.0 < bound)
                .unwrap_or(cands.len());
            cands.truncate(keep.max(want));
        }
        let mut hits: Vec<Hit> = cands
            .into_iter()
            .filter_map(|(_, seg, doc)| read_hit(seg, doc))
            .collect();
        // Timestamps tie constantly, and tantivy's internal document order is
        // not an order a reader can predict. Break ties on the path so the same
        // query always produces the same page.
        sort_hits(&mut hits, SortKey::Modified, true);
        let hits: Vec<Hit> = hits.into_iter().skip(offset).take(limit).collect();

        Ok(SearchResponse {
            hits,
            total: counted.min(cap) as u64,
            capped: counted >= cap,
            took_us: started.elapsed().as_micros() as u64,
            fast_path: true,
        })
    }

    fn facets(&self, req: &FacetRequest) -> Result<FacetResponse> {
        let started = Instant::now();
        let searcher = self.reader.searcher();
        let plan = lower(&req.query, &self.index.schema(), &self.opts)?;
        let facets = match &req.by {
            FacetBy::Kind => self
                .term_counts(&searcher, &plan, field::KIND, 16)?
                .into_iter()
                .map(|(k, count)| Facet {
                    key: k
                        .parse::<u8>()
                        .ok()
                        .and_then(Kind::from_u8)
                        .map_or(k, |kind| kind.msgid().to_owned()),
                    count,
                })
                .collect(),
            FacetBy::Ext { top } => self
                .term_counts(&searcher, &plan, field::EXT, *top as usize)?
                .into_iter()
                .map(|(key, count)| Facet { key, count })
                .collect(),
            FacetBy::Dir { path, top } => {
                self.child_counts(&searcher, &plan, path, *top as usize)?
            }
        };
        Ok(FacetResponse {
            facets,
            took_us: started.elapsed().as_micros() as u64,
        })
    }

    fn stats(&self) -> Result<IndexStats> {
        let searcher = self.reader.searcher();
        let dirs = self
            .term_counts(
                &searcher,
                &lower(&Default::default(), &self.index.schema(), &self.opts)?,
                field::IS_DIR,
                4,
            )
            .unwrap_or_default()
            .into_iter()
            .find(|(k, _)| k == "1")
            .map(|(_, n)| n)
            .unwrap_or(0);
        let p = self.pending.read();
        Ok(IndexStats {
            entries: searcher.num_docs(),
            dirs,
            bytes_on_disk: dir_size(&self.dir),
            segments: searcher.segment_readers().len() as u32,
            unsorted_entries: self.tail_docs().max(p.tail),
            pending_removals: p.hidden.len() as u64,
            has_content: self.opts.index_content,
        })
    }

    fn maintain(&self, level: Maintenance) -> Result<MaintReport> {
        let started = Instant::now();
        let before = dir_size(&self.dir);
        match level {
            Maintenance::Flush => self.commit()?,
            Maintenance::Compact => {
                self.commit()?;
                self.writer
                    .lock()
                    .garbage_collect_files()
                    .wait()
                    .map_err(tv)?;
            }
            Maintenance::Rebuild => self.rebuild()?,
        }
        Ok(MaintReport {
            level,
            bytes_before: before,
            bytes_after: dir_size(&self.dir),
            took_ms: started.elapsed().as_millis() as u64,
        })
    }
}

impl TantivyIndex {
    /// Count matches, stopping at `cap`.
    ///
    /// An earlier version of this used tantivy's `Count` collector and then
    /// applied `.min(cap)` to the answer, which capped the *number* and not the
    /// *work* — it still visited every hit, and made the fast path six hundred
    /// times slower than it should have been. The walk below actually stops.
    fn count_upto(
        &self,
        plan: &Lowered,
        searcher: &tantivy::Searcher,
        read_hit: &dyn Fn(u32, DocId) -> Option<Hit>,
        cap: usize,
    ) -> Result<usize> {
        let weight = plan
            .query
            .weight(EnableScoring::disabled_from_searcher(searcher))
            .map_err(tv)?;
        let pending = self.pending.read();
        let mut n = 0usize;
        for (ord, seg) in searcher.segment_readers().iter().enumerate() {
            let hashes: Column<u64> = seg.fast_fields().u64(field::EID_HASH).map_err(tv)?;
            let alive = seg.alive_bitset();
            let mut scorer = weight.scorer(seg, 1.0).map_err(tv)?;
            let mut doc = scorer.doc();
            while doc != TERMINATED {
                if alive.is_some_and(|b| !b.is_alive(doc)) {
                    doc = scorer.advance();
                    continue;
                }
                if pending.hidden.contains(&hashes.first(doc).unwrap_or(0)) {
                    doc = scorer.advance();
                    continue;
                }
                // A wildcard the postings could not express, or a subtree
                // removed since the last commit, has to be checked against the
                // row itself.
                if (!plan.is_exact() || !pending.hidden_prefixes.is_empty())
                    && read_hit(ord as u32, doc).is_none()
                {
                    doc = scorer.advance();
                    continue;
                }
                n += 1;
                if n >= cap {
                    return Ok(n);
                }
                doc = scorer.advance();
            }
        }
        Ok(n)
    }

    /// The ordinary route: a top-K heap over every match.
    fn top_k(
        &self,
        searcher: &tantivy::Searcher,
        plan: &Lowered,
        req: &SearchRequest,
        read_hit: &dyn Fn(u32, DocId) -> Option<Hit>,
        want: usize,
    ) -> Result<Vec<Hit>> {
        let (col, is_text) = crate::schema::sort_column(req.sort);
        let order = if req.descending {
            Order::Desc
        } else {
            Order::Asc
        };

        // Over-fetch, then check whether the window cut a tie group in half.
        //
        // Tantivy orders by one column and breaks ties by internal document
        // order, which is not stable for a reader. Re-sorting fixes the order
        // of what was fetched, but cannot recover a row that was never fetched
        // — and on a real filesystem a page is often a single timestamp shared
        // by a hundred files. So the window grows until the last row it
        // contains no longer shares its sort value with the row at the page
        // boundary, or until it is clearly not worth continuing.
        let mut over = want + TIE_SLACK;
        loop {
            let addrs: Vec<tantivy::DocAddress> = if is_text {
                searcher
                    .search(
                        &plan.query,
                        &TopDocs::with_limit(over).order_by_string_fast_field(col, order),
                    )
                    .map_err(tv)?
                    .into_iter()
                    .map(|(_, a)| a)
                    .collect()
            } else {
                searcher
                    .search(
                        &plan.query,
                        &TopDocs::with_limit(over).order_by_fast_field::<i64>(col, order),
                    )
                    .map_err(tv)?
                    .into_iter()
                    .map(|(_, a)| a)
                    .collect()
            };
            let exhausted = addrs.len() < over;
            let mut hits: Vec<Hit> = addrs
                .into_iter()
                .filter_map(|a| read_hit(a.segment_ord, a.doc_id))
                .collect();
            sort_hits(&mut hits, req.sort, req.descending);

            let cut = !exhausted
                && hits.len() > want
                && same_sort_key(&hits[want - 1], hits.last().expect("non-empty"), req.sort);
            if !cut || over >= MAX_TIE_WINDOW {
                return Ok(hits);
            }
            over *= 4;
        }
    }

    /// Counts per distinct value of a fast column, over the matching set.
    fn term_counts(
        &self,
        searcher: &tantivy::Searcher,
        plan: &Lowered,
        column: &str,
        top: usize,
    ) -> Result<Vec<(String, u64)>> {
        use tantivy::aggregation::agg_req::Aggregations;
        use tantivy::aggregation::{
            AggContextParams, AggregationCollector, AggregationLimitsGuard,
        };

        let agg: Aggregations = serde_json::from_value(serde_json::json!({
            "g": { "terms": { "field": column, "size": top.max(1) } }
        }))
        .map_err(|e| Error::Io {
            detail: e.to_string(),
        })?;
        let collector = AggregationCollector::from_aggs(
            agg,
            AggContextParams::new(
                AggregationLimitsGuard::default(),
                self.index.tokenizers().clone(),
            ),
        );
        let res = searcher.search(&plan.query, &collector).map_err(tv)?;
        let v = serde_json::to_value(res).map_err(|e| Error::Io {
            detail: e.to_string(),
        })?;
        let mut out = Vec::new();
        if let Some(buckets) = v["g"]["buckets"].as_array() {
            for b in buckets {
                let count = b["doc_count"].as_u64().unwrap_or(0);
                let key = match &b["key"] {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n
                        .as_f64()
                        .map(|f| (f as i64).to_string())
                        .unwrap_or_default(),
                    _ => continue,
                };
                out.push((key, count));
            }
        }
        out.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        Ok(out)
    }

    /// How many matches sit under each immediate child of a directory.
    ///
    /// There is no column for "the next path component after this prefix", so
    /// this reads paths — bounded by the same cap everything else is, and
    /// reported honestly through the facet counts.
    fn child_counts(
        &self,
        searcher: &tantivy::Searcher,
        plan: &Lowered,
        parent: &str,
        top: usize,
    ) -> Result<Vec<Facet>> {
        let prefix = parent.trim_end_matches('/');
        let f_path = self.field(field::PATH)?;
        let weight = plan
            .query
            .weight(EnableScoring::disabled_from_searcher(searcher))
            .map_err(tv)?;
        let mut counts: HashMap<String, u64> = HashMap::new();
        let mut seen = 0usize;
        const SCAN_CAP: usize = 200_000;

        'outer: for (ord, seg) in searcher.segment_readers().iter().enumerate() {
            let alive = seg.alive_bitset();
            let mut scorer = weight.scorer(seg, 1.0).map_err(tv)?;
            let mut doc = scorer.doc();
            while doc != TERMINATED {
                if alive.is_some_and(|b| !b.is_alive(doc)) {
                    doc = scorer.advance();
                    continue;
                }
                let d: std::result::Result<TantivyDocument, _> =
                    searcher.doc(tantivy::DocAddress::new(ord as u32, doc));
                if let Ok(d) = d
                    && let Some(p) = d.get_first(f_path).and_then(|v| v.as_str())
                    && let Some(rest) = p.strip_prefix(prefix).and_then(|r| r.strip_prefix('/'))
                {
                    let child = rest.split('/').next().unwrap_or(rest);
                    *counts.entry(child.to_owned()).or_default() += 1;
                }
                seen += 1;
                if seen >= SCAN_CAP {
                    break 'outer;
                }
                doc = scorer.advance();
            }
        }
        let mut out: Vec<Facet> = counts
            .into_iter()
            .map(|(key, count)| Facet { key, count })
            .collect();
        out.sort_unstable_by(|a, b| b.count.cmp(&a.count).then(a.key.cmp(&b.key)));
        out.truncate(top.max(1));
        Ok(out)
    }
}

/// How far past the page to look for rows that tie with the last one.
///
/// The real index measured 115 files per distinct timestamp — package installs
/// and archive extractions stamp thousands at the same instant — so a page is
/// routinely one tie group and a window equal to the page size would return an
/// arbitrary slice of it.
const TIE_SLACK: usize = 1_024;

/// Where growing the window to contain a tie group stops being worth it.
const MAX_TIE_WINDOW: usize = 262_144;

/// Do two rows tie on the sort column, before the path breaks the tie?
fn same_sort_key(a: &Hit, b: &Hit, key: SortKey) -> bool {
    match key {
        SortKey::Name => DefaultFolder.fold(a.name()) == DefaultFolder.fold(b.name()),
        SortKey::Path => a.path == b.path,
        SortKey::Ext => scour_core::ext_of(a.name()) == scour_core::ext_of(b.name()),
        SortKey::Size => a.meta.size == b.meta.size,
        SortKey::Modified => a.meta.mtime == b.meta.mtime,
        SortKey::Created => a.meta.ctime == b.meta.ctime,
        SortKey::Accessed => a.meta.atime == b.meta.atime,
        SortKey::Kind => a.kind == b.kind,
        SortKey::Items => a.meta.items == b.meta.items,
        SortKey::Mode => a.meta.mode == b.meta.mode,
        SortKey::Uid => a.meta.uid == b.meta.uid,
        SortKey::Gid => a.meta.gid == b.meta.gid,
        SortKey::Disk => a.meta.disk == b.meta.disk,
    }
}

/// Deterministic ordering, with an explicit tie-break on the path.
fn sort_hits(hits: &mut [Hit], key: SortKey, desc: bool) {
    hits.sort_unstable_by(|a, b| {
        let o = match key {
            SortKey::Name => DefaultFolder
                .fold(a.name())
                .cmp(&DefaultFolder.fold(b.name())),
            SortKey::Path => a.path.cmp(&b.path),
            SortKey::Ext => scour_core::ext_of(a.name()).cmp(&scour_core::ext_of(b.name())),
            SortKey::Size => a.meta.size.cmp(&b.meta.size),
            SortKey::Modified => a.meta.mtime.cmp(&b.meta.mtime),
            SortKey::Created => a.meta.ctime.cmp(&b.meta.ctime),
            SortKey::Accessed => a.meta.atime.cmp(&b.meta.atime),
            SortKey::Kind => a.kind.cmp(&b.kind),
            SortKey::Items => a.meta.items.cmp(&b.meta.items),
            SortKey::Mode => a.meta.mode.cmp(&b.meta.mode),
            SortKey::Uid => a.meta.uid.cmp(&b.meta.uid),
            SortKey::Gid => a.meta.gid.cmp(&b.meta.gid),
            SortKey::Disk => a.meta.disk.cmp(&b.meta.disk),
        };
        let o = if desc { o.reverse() } else { o };
        o.then_with(|| a.path.cmp(&b.path))
    });
}

/// Is `path` inside `prefix`, or the prefix itself?
fn under(path: &str, prefix: &str) -> bool {
    let p = prefix.trim_end_matches('/');
    path == p || (path.starts_with(p) && path.as_bytes().get(p.len()) == Some(&b'/'))
}

fn dir_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| e.metadata().ok())
                .filter(|m| m.is_file())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

fn tv(e: impl std::fmt::Display) -> Error {
    Error::IndexCorrupt {
        detail: e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtree_membership_is_by_component_not_by_prefix() {
        assert!(under("/a/b/c", "/a"));
        assert!(under("/a", "/a"));
        assert!(under("/a/b", "/a/"));
        // The bug this exists to prevent: /abc is not inside /a.
        assert!(!under("/abc", "/a"));
        assert!(!under("/b", "/a"));
    }
}
