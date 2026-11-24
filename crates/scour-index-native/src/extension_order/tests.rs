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

/// Rows as a segment's extension order has them: long climbing groups, a
/// group of one here and there, and a tail shorter than a chunk.
fn grouped(rows: usize) -> (Vec<u32>, Vec<u8>) {
    let mut state: u64 = 0x243f_6a88_85a3_08d3;
    let mut next = |n: usize| -> usize {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        ((state >> 33) as usize) % n
    };
    let mut group_of: Vec<usize> = (0..rows)
        .map(|_| if next(10) < 6 { 0 } else { next(40) })
        .collect();
    if let Some(last) = group_of.last_mut() {
        *last = 41;
    }
    let mut order: Vec<u32> = (0..rows as u32).collect();
    order.sort_by_key(|&row| (group_of[row as usize], row));
    let mut groups = vec![0u8; rows.div_ceil(8)];
    for i in 0..rows {
        if i == 0 || group_of[order[i - 1] as usize] != group_of[order[i] as usize] {
            groups[i / 8] |= 1 << (i % 8);
        }
    }
    (order, groups)
}

#[test]
fn every_position_comes_back_alone_and_in_a_stream() {
    for rows in [1usize, 2, 31, 32, 33, 64, 65, 1000, 5000] {
        let (order, groups) = grouped(rows);
        let blob = encode(&order, &groups);
        let stored = ExtensionOrder::open(&blob).expect("open");
        assert_eq!(stored.rows(), rows);
        let mut chunk = Chunk::default();
        for (i, &want) in order.iter().enumerate() {
            assert_eq!(stored.at(i), Some(want), "{rows} rows at {i}");
            assert_eq!(chunk.at(&stored, i), Some(want), "{rows} rows at {i}");
        }
        // Backwards and by jumps, as a descending stream reads its groups.
        for i in (0..rows).rev().chain((0..rows).step_by(37)) {
            assert_eq!(chunk.at(&stored, i), Some(order[i]), "{rows} rows at {i}");
        }
        assert_eq!(stored.at(rows), None);
        assert_eq!(chunk.at(&stored, rows + 5), None);
        for i in 0..rows {
            let start = (0..=i).rev().find(|&j| groups[j / 8] >> (j % 8) & 1 != 0);
            assert_eq!(stored.group_at_or_before(i), start);
            assert_eq!(stored.starts_group(i), start == Some(i));
        }
    }
}

#[test]
fn climbing_rows_are_stored_as_steps() {
    // One group of every row: a first row and thirty-one one-bit steps.
    let order: Vec<u32> = (0..3200).collect();
    let mut groups = vec![0u8; 400];
    groups[0] = 1;
    let blob = encode(&order, &groups);
    assert_eq!(blob.len(), 4 + 101 * 4 + 100 * (1 + 4 + 4) + 400);
    // Rows that fall are a row number each, twelve bits here.
    let order: Vec<u32> = (0..3200).rev().collect();
    let blob = encode(&order, &groups);
    assert_eq!(blob.len(), 4 + 101 * 4 + 100 * (1 + 48) + 400);
}

#[test]
fn a_file_cut_or_scribbled_on_is_refused_or_read_and_never_a_panic() {
    let (order, groups) = grouped(1500);
    let blob = encode(&order, &groups);
    for cut in 0..blob.len() {
        assert!(ExtensionOrder::open(&blob[..cut]).is_none(), "cut at {cut}");
    }
    let mut longer = blob.clone();
    longer.push(0);
    assert!(ExtensionOrder::open(&longer).is_none());
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    for _ in 0..300 {
        let mut bad = blob.clone();
        for _ in 0..6 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let at = 4 + (state >> 33) as usize % (bad.len() - 4);
            bad[at] = (state >> 13) as u8;
        }
        let Some(stored) = ExtensionOrder::open(&bad) else {
            continue;
        };
        let mut chunk = Chunk::default();
        for i in 0..1500 {
            assert!(chunk.at(&stored, i).is_none_or(|row| row < 1500));
            assert!(stored.group_at_or_before(i).is_none_or(|g| g <= i));
        }
    }
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
    use scour_core::text::{Folder, Unaccented};

    // Fourteen raw bytes, so not an extension, though the fold makes seven.
    let contracted = format!("x.{}", "ı".repeat(7));
    let contracted_folded = Unaccented.fold(&contracted);
    assert_eq!(
        folded_extension(contracted.as_bytes(), contracted_folded.as_bytes()),
        b""
    );

    // U+023A is two bytes and folds to three, so six fit the raw limit while
    // the key is eighteen bytes and must not be truncated to it.
    let growing_ext = "Ⱥ".repeat(6);
    assert_eq!(growing_ext.len(), 12);
    let growing = format!("x.{growing_ext}");
    let growing_folded = Unaccented.fold(&growing);
    let want = Unaccented.fold(&growing_ext);
    assert!(want.len() > 16, "fixture does not grow past the sort head");
    assert_eq!(
        folded_extension(growing.as_bytes(), growing_folded.as_bytes()),
        want.as_bytes()
    );
}
