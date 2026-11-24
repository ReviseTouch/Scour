//! The engine's answers in the wire's words.

use super::*;

/// A grep pattern as typed, in the mode asked for.
pub(super) fn needle_of(
    mode: scour_proto::GrepMode,
    pattern: String,
) -> scour_core::Result<scour_grep::Needle> {
    Ok(match mode {
        scour_proto::GrepMode::Text => scour_grep::Needle::Text(pattern),
        scour_proto::GrepMode::Regex => scour_grep::Needle::Regex(pattern),
        scour_proto::GrepMode::Hex => {
            scour_grep::Needle::hex(&pattern).ok_or_else(|| scour_core::Error::QuerySyntax {
                at: 0,
                expected: "pairs of hex digits, ?? for any byte, ^ for the start".into(),
            })?
        }
    })
}

pub(super) fn grep_answer(r: scour_engine::Grepped) -> scour_proto::GrepResponse {
    scour_proto::GrepResponse {
        files: r
            .files
            .into_iter()
            .map(|f| scour_proto::GrepFile {
                path: f.path,
                meta: f.meta,
                kind: f.kind,
                more: f.more,
                hits: f
                    .hits
                    .into_iter()
                    .map(|h| scour_proto::GrepHit {
                        line: h.line,
                        offset: h.offset,
                        text: h.text,
                        at: h.at,
                    })
                    .collect(),
            })
            .collect(),
        candidates: r.candidates,
        capped: r.capped,
        scanned: r.scanned,
        read: r.read,
        binary: r.binary,
        too_big: r.too_big,
        unreadable: r.unreadable,
        header_only: r.header_only,
        no_reader: r.no_reader,
        locked: r.locked,
        offline: r.offline,
        next: r.next,
        misread: r.misread,
        took_us: r.took_us,
    }
}

pub(super) fn duplicates_answer(r: scour_dupes::Report) -> Response {
    Response::Duplicates {
        groups: r
            .groups
            .iter()
            .map(|g| scour_proto::DupGroup {
                size: g.size,
                paths: g.paths.clone(),
                waste: g.waste(),
                certainty: g.certainty.token().to_owned(),
            })
            .collect(),
        candidates: r.candidates,
        waste: r.waste,
        proven: r.proven,
        read: r.read,
        unconfirmed: r.unconfirmed,
    }
}
