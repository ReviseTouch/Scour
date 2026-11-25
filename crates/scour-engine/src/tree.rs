//! Listing a directory from the index, not the filesystem: a directory holding
//! a million entries lists as fast as one holding ten, and how much each child
//! directory holds is known without opening it.
//!
//! Bounded per level by `limit`, not in total; `truncated` says what was left out.
//! A folder above the sources — `/` over `/home/u` and `/mnt/data` — is no row,
//! so it is listed from the roots below it, and only where those hold rows.

use scour_core::{Ast, Group, Index, Kind, Match, Page, Result, SearchRequest, SortKey, TreeNode};

pub fn build(
    index: &dyn Index,
    roots: &[String],
    path: &str,
    depth: u32,
    limit: u32,
) -> Result<TreeNode> {
    let roots: Vec<String> = roots.iter().map(|r| normalise(r)).collect();
    let path = normalise(path);
    // Not indexed: still a usable answer, because its children may well be.
    let mut root = stat_from_index(index, &path)?.unwrap_or_else(|| folder(&path));
    fill(index, &roots, &mut root, depth, limit)?;
    Ok(root)
}

fn fill(
    index: &dyn Index,
    roots: &[String],
    node: &mut TreeNode,
    depth: u32,
    limit: u32,
) -> Result<()> {
    if !node.is_dir {
        return Ok(());
    }
    if depth == 0 {
        // Count without listing: `children == 0` at the depth limit would read as empty.
        node.children = count_children(index, roots, &node.path)?;
        node.truncated = node.children > 0;
        return Ok(());
    }
    // A source rooted at `/` holds a row `/`, whose parent is itself.
    let own = u32::from(node.path == "/");
    let res = index.search(&SearchRequest {
        query: one(Match::ParentIs(node.path.clone())),
        // By name; directories are lifted above files after the page comes back.
        sort: SortKey::Name,
        descending: false,
        page: Page {
            offset: 0,
            limit: limit.saturating_add(own),
            count_cap: limit.saturating_add(1 + own),
        },
    })?;
    let itself = res.hits.iter().any(|h| h.path == node.path);
    let mut total = res.total.saturating_sub(u64::from(itself));
    let mut nodes: Vec<TreeNode> = res
        .hits
        .into_iter()
        .filter(|h| h.path != node.path)
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
    for path in between(roots, &node.path) {
        if nodes.iter().any(|n| n.path == path) {
            continue;
        }
        let mut child = folder(&path);
        child.children = count_children(index, roots, &path)?;
        if child.children > 0 {
            total += 1;
            nodes.push(child);
        }
    }
    nodes.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
    nodes.truncate(limit as usize);
    node.children = total;
    node.truncated = (nodes.len() as u64) < total;
    for child in &mut nodes {
        fill(index, roots, child, depth - 1, limit)?;
    }
    node.nodes = nodes;
    Ok(())
}

/// How many entries are directly inside, without materialising any of them.
fn count_children(index: &dyn Index, roots: &[String], path: &str) -> Result<u64> {
    let total = index
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
        .total;
    let itself = path == "/" && stat_from_index(index, path)?.is_some();
    let mut n = total.saturating_sub(u64::from(itself));
    for folder in between(roots, path) {
        if count_children(index, roots, &folder)? > 0 {
            n += 1;
        }
    }
    Ok(n)
}

/// The folders directly in `path` that hold no row but lead down to a root:
/// `/home` in `/`, for a root `/home/u`. Never one inside a root.
fn between(roots: &[String], path: &str) -> Vec<String> {
    let base = path.trim_end_matches('/');
    let mut out: Vec<String> = Vec::new();
    for root in roots {
        let Some(rest) = root.strip_prefix(base).and_then(|r| r.strip_prefix('/')) else {
            continue;
        };
        // A root directly in `path` is a row of it.
        let Some((first, _)) = rest.split_once('/') else {
            continue;
        };
        let child = format!("{base}/{first}");
        if first.is_empty() || out.contains(&child) || roots.iter().any(|r| within(&child, r)) {
            continue;
        }
        out.push(child);
    }
    out
}

/// Is `path` this root or inside it?
fn within(path: &str, root: &str) -> bool {
    root == "/"
        || path == root
        || (path.starts_with(root) && path.as_bytes().get(root.len()) == Some(&b'/'))
}

/// A folder the index has no row for.
fn folder(path: &str) -> TreeNode {
    TreeNode {
        name: leaf(path).to_owned(),
        path: path.to_owned(),
        is_dir: true,
        kind: Kind::Dir,
        size: 0,
        mtime: 0,
        children: 0,
        nodes: Vec::new(),
        truncated: false,
    }
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
            // `leaf`, not the name: the row `/` has an empty one.
            name: leaf(&h.path).to_owned(),
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
        ..Ast::default()
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
