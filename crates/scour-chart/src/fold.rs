//! The few worth a row of their own, and the rest as one line.

/// What was folded away: how many, and what they add up to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rest {
    pub count: usize,
    pub value: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Folded<T> {
    /// The largest `keep`, largest first.
    pub kept: Vec<T>,
    /// The others, when there were any.
    pub rest: Option<Rest>,
}

/// Keep the `keep` largest by `value`, fold the others into one [`Rest`]. Stable:
/// two equal values keep the order they came in.
pub fn fold<T>(mut items: Vec<T>, value: impl Fn(&T) -> u64, keep: usize) -> Folded<T> {
    items.sort_by_key(|x| std::cmp::Reverse(value(x)));
    if items.len() <= keep {
        return Folded {
            kept: items,
            rest: None,
        };
    }
    let others = items.split_off(keep);
    Folded {
        kept: items,
        rest: Some(Rest {
            count: others.len(),
            value: others.iter().map(&value).sum(),
        }),
    }
}
