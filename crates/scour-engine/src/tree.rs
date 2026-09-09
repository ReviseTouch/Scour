//! Listing a directory from the index, not the filesystem: a directory holding
//! a million entries lists as fast as one holding ten, and how much each child
//! directory holds is known without opening it.
//!
//! Bounded per level by `limit`, not in total; `truncated` says what was left out.

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
    if !node.is_dir {
        return Ok(());
    }
    if depth == 0 {
        // Count without listing: `children == 0` at the depth limit would read as empty.
        node.children = count_children(index, &node.path)?;
        node.truncated = node.children > 0;
        return Ok(());
    }
    let res = index.search(&SearchRequest {
        query: one(Match::ParentIs(node.path.clone())),
        // By name; directories are lifted above files after the page comes back.
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

/// How many entries are directly inside, without materialising any of them.
fn count_children(index: &dyn Index, path: &str) -> Result<u64> {
    Ok(index
        .search(&SearchRequest {
            query: one(Match::ParentIs(path.to_owned())),
            sort: SortKey::Name,
            descending: false,
            page: Page {
                offset: 0,
                limit: 0,
                count_cap: 1_000_000,
            },
        })?
        .total)
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
