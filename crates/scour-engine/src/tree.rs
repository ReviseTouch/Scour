//! Listing a directory from the index.
//!
//! Answered from the index rather than the filesystem, and that is the whole
//! point. A directory holding a million files takes as long to list here as one
//! holding ten, because the index already knows which entries name it as their
//! parent — and it knows how many entries each *child* directory holds without
//! opening any of them.
//!
//! This is the operation an assistant exploring a filesystem actually performs,
//! and it is bounded on purpose: `limit` per level rather than in total, with
//! `truncated` saying plainly when something was left out. A listing that
//! silently omits half a directory is worse than one that admits it.

use scour_core::{Ast, Group, Index, Kind, Match, Page, Result, SearchRequest, SortKey, TreeNode};

pub fn build(index: &dyn Index, path: &str, depth: u32, limit: u32) -> Result<TreeNode> {
    let path = normalise(path);
    let mut root = match stat_from_index(index, &path)? {
        Some(node) => node,
        // Not indexed: still a usable answer, because its children may well be.
        None => TreeNode {
            name: leaf(&path).to_owned(),
            path: path.clone(),
            is_dir: true,
            kind: Kind::Dir,
            size: 0,
            mtime: 0,
            children: 0,
            nodes: Vec::new(),
            truncated: false,
        },
    };
    fill(index, &mut root, depth, limit)?;
    Ok(root)
}

fn fill(index: &dyn Index, node: &mut TreeNode, depth: u32, limit: u32) -> Result<()> {
    if depth == 0 || !node.is_dir {
        return Ok(());
    }
    let res = index.search(&SearchRequest {
        query: one(Match::ParentIs(node.path.clone())),
        // Directories first, then by name: the order someone reads a listing
        // in, and stable enough to page through.
        sort: SortKey::Name,
        descending: false,
        page: Page {
            offset: 0,
            limit,
            count_cap: limit.saturating_add(1),
        },
    })?;
    node.children = res.total;
    node.truncated = (res.hits.len() as u64) < res.total;
    let mut nodes: Vec<TreeNode> = res
        .hits
        .into_iter()
        .map(|h| TreeNode {
            name: h.name().to_owned(),
            is_dir: h.is_dir,
            kind: h.kind,
            size: h.meta.size,
            mtime: h.meta.mtime,
            children: 0,
            nodes: Vec::new(),
            truncated: false,
            path: h.path,
        })
        .collect();
    nodes.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
    for child in &mut nodes {
        fill(index, child, depth - 1, limit)?;
    }
    node.nodes = nodes;
    Ok(())
}

/// The entry for this exact path, if the index holds it.
fn stat_from_index(index: &dyn Index, path: &str) -> Result<Option<TreeNode>> {
    let parent = parent_of(path);
    let res = index.search(&SearchRequest {
        query: one(Match::ParentIs(parent.to_owned())),
        sort: SortKey::Name,
        descending: false,
        page: Page {
            offset: 0,
            limit: 10_000,
            count_cap: 10_000,
        },
    })?;
    Ok(res
        .hits
        .into_iter()
        .find(|h| h.path == path)
        .map(|h| TreeNode {
            name: h.name().to_owned(),
            is_dir: h.is_dir,
            kind: h.kind,
            size: h.meta.size,
            mtime: h.meta.mtime,
            children: 0,
            nodes: Vec::new(),
            truncated: false,
            path: h.path,
        }))
}

fn one(m: Match) -> Ast {
    Ast {
        groups: vec![Group {
            alts: vec![(false, m)],
        }],
    }
}

fn normalise(p: &str) -> String {
    let t = p.replace('\\', "/");
    let t = t.trim_end_matches('/');
    if t.is_empty() {
        "/".to_owned()
    } else {
        t.to_owned()
    }
}

fn parent_of(p: &str) -> &str {
    match p.rfind('/') {
        Some(0) => "/",
        Some(i) => &p[..i],
        None => "",
    }
}

fn leaf(p: &str) -> &str {
    match p.rfind('/') {
        Some(i) if i + 1 < p.len() => &p[i + 1..],
        _ => p,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_helpers() {
        assert_eq!(normalise("/a/b/"), "/a/b");
        assert_eq!(normalise("/"), "/");
        assert_eq!(normalise(""), "/");
        assert_eq!(parent_of("/a/b"), "/a");
        assert_eq!(parent_of("/a"), "/");
        assert_eq!(leaf("/a/b"), "b");
        assert_eq!(leaf("/"), "/");
    }
}
