//! The rows of a segment, in folded-extension order.
//!
//! Extension *eligibility* comes from the spelling: the suffix after the last
//! dot has to be one to twelve bytes before folding. Its comparison key comes
//! from the folded arena and may be longer, because Unicode folding can change
//! byte length. Discovering which rows own the first page used to walk every
//! row; this stores that decision once when the segment is built.
//!
//! The on-disk representation is deliberately the same grouped-row format as
//! [`crate::NameOrder`]. Equal extensions are one large tie group, often tens
//! of thousands of rows, and their row order is load-bearing: newest first,
//! then path, independently of the requested primary-key direction.

use crate::name_order::NameOrder;

/// The rows of one segment in ascending folded-extension order.
#[derive(Debug, Clone, Copy)]
pub struct ExtensionOrder<'a> {
    inner: NameOrder<'a>,
}

impl<'a> ExtensionOrder<'a> {
    /// Open the file, or refuse a malformed one.
    pub fn open(bytes: &'a [u8]) -> Option<ExtensionOrder<'a>> {
        NameOrder::open(bytes).map(|inner| ExtensionOrder { inner })
    }

    pub fn rows(&self) -> usize {
        self.inner.rows()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// The row whose extension is `i`-th in ascending order.
    pub fn at(&self, i: usize) -> Option<u32> {
        self.inner.at(i)
    }

    /// The start of the equal-extension group containing position `i`.
    pub(crate) fn group_at_or_before(&self, i: usize) -> Option<usize> {
        self.inner.group_at_or_before(i)
    }
}

/// Order rows by the complete folded extension of each eligible spelled name.
///
/// `order` is returned by the name-order build, so this second text order adds
/// no second row-list allocation to rebuild peak memory.
pub(crate) fn build(rows: usize, spelled: &[u8], folded: &[u8], order: Vec<u32>) -> Vec<u8> {
    let eligible = eligible_rows(rows, spelled);
    crate::name_order::build_keyed(rows, folded, order, extension, Some(&eligible)).0
}

/// The row's complete folded extension, with eligibility decided pre-fold.
pub(crate) fn folded_extension<'a>(spelled: &[u8], folded: &'a [u8]) -> &'a [u8] {
    if extension_is_eligible(spelled) {
        extension(folded)
    } else {
        b""
    }
}

/// The suffix after the last dot, with no folded-length restriction.
fn extension(name: &[u8]) -> &[u8] {
    match name.iter().rposition(|&b| b == b'.') {
        Some(i) if i > 0 && i + 1 < name.len() => &name[i + 1..],
        _ => b"",
    }
}

/// Whether the spelling satisfies the core extension contract.
fn extension_is_eligible(name: &[u8]) -> bool {
    name.iter()
        .rposition(|&b| b == b'.')
        .is_some_and(|i| i > 0 && (1..=12).contains(&(name.len() - i - 1)))
}

/// One eligibility bit per row, derived in a sequential pass over spellings.
fn eligible_rows(rows: usize, names: &[u8]) -> Vec<u8> {
    let mut eligible = vec![0u8; rows.div_ceil(8)];
    let mut cursor = 0usize;
    for row in 0..rows {
        let Some(end) = names
            .get(cursor..)
            .and_then(|rest| memchr::memchr(0, rest))
            .map(|end| cursor + end)
        else {
            break;
        };
        if extension_is_eligible(&names[cursor..end]) {
            eligible[row / 8] |= 1 << (row % 8);
        }
        cursor = end + 1;
    }
    eligible
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::names::NameWriter;

    fn order(names: &[&str]) -> (Vec<usize>, Vec<usize>) {
        let mut writer = NameWriter::new();
        for name in names {
            writer.push(name);
        }
        let blob = build(writer.len(), writer.spelled(), writer.folded(), Vec::new());
        let stored = ExtensionOrder::open(&blob).expect("extension order");
        let rows = (0..stored.rows())
            .map(|i| stored.at(i).expect("row") as usize)
            .collect();
        let groups = (0..stored.rows())
            .filter(|&i| stored.group_at_or_before(i) == Some(i))
            .collect();
        (rows, groups)
    }

    #[test]
    fn extensions_are_ordered_and_ties_keep_row_order() {
        let names = ["z.rs", "plain", "a.PDF", "b.rs", ".hidden", "c.txt"];
        let (rows, groups) = order(&names);
        let got: Vec<&str> = rows.into_iter().map(|row| names[row]).collect();
        assert_eq!(got, ["plain", ".hidden", "a.PDF", "z.rs", "b.rs", "c.txt"]);
        assert_eq!(groups, [0, 2, 3, 5]);
    }

    #[test]
    fn an_empty_order_is_readable() {
        let blob = build(0, b"", b"", Vec::new());
        let stored = ExtensionOrder::open(&blob).expect("empty order");
        assert!(stored.is_empty());
        assert_eq!(stored.at(0), None);
    }

    #[test]
    fn eligibility_is_decided_before_folding_changes_the_byte_length() {
        use scour_core::text::{DefaultFolder, Folder};

        // Seven dotless i characters are fourteen raw bytes, so this is not
        // an extension even though Scour's Turkish fold contracts it to seven.
        let contracted = format!("x.{}", "ı".repeat(7));
        let contracted_folded = DefaultFolder.fold(&contracted);
        assert_eq!(
            folded_extension(contracted.as_bytes(), contracted_folded.as_bytes()),
            b""
        );

        // U+023A is two bytes and lowercases to U+2C65, which is three. Six
        // fit the raw twelve-byte limit while their complete folded key is
        // eighteen bytes and must not be truncated to the raw limit.
        let growing_ext = "Ⱥ".repeat(6);
        assert_eq!(growing_ext.len(), 12);
        let growing = format!("x.{growing_ext}");
        let growing_folded = DefaultFolder.fold(&growing);
        let want = DefaultFolder.fold(&growing_ext);
        assert!(want.len() > 16, "fixture does not grow past the sort head");
        assert_eq!(
            folded_extension(growing.as_bytes(), growing_folded.as_bytes()),
            want.as_bytes()
        );
    }
}
