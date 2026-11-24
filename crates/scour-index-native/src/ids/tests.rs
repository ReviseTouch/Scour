use super::*;

const S: SourceId = SourceId(0);

fn path(n: u64) -> String {
    format!("/home/u/dosya-{n}.txt")
}

#[test]
fn every_path_finds_its_own_row() {
    let mut w = IdWriter::new();
    for n in 0..1000u64 {
        w.push(S, &path(n), n as u32);
    }
    let bytes = w.finish();
    let map = IdMap::open(&bytes).expect("open");
    assert_eq!(map.len(), 1000);
    for n in 0..1000u64 {
        let rows: Vec<u32> = map.candidates(S, &path(n)).collect();
        assert!(rows.contains(&(n as u32)), "row for {n} not among {rows:?}");
    }
}

#[test]
fn an_absent_path_returns_almost_nothing() {
    // A collision is allowed; being handed every row is not.
    let mut w = IdWriter::new();
    for n in 0..1000u64 {
        w.push(S, &path(n), n as u32);
    }
    let bytes = w.finish();
    let map = IdMap::open(&bytes).expect("open");
    let mut worst = 0;
    for n in 5000..6000u64 {
        worst = worst.max(map.candidates(S, &path(n)).count());
    }
    assert!(worst <= 2, "a probe returned {worst} candidates");
}

#[test]
fn the_digest_is_pinned_to_these_numbers() {
    // The number is the format: change it and no index on disk finds
    // anything, with no error anywhere.
    assert_eq!(digest(SourceId(0), "/a/b"), 0xe8e9_2dc1_109d_665b);
    assert_ne!(
        digest(SourceId(0), "/a/b"),
        digest(SourceId(1), "/a/b"),
        "the source is part of the identity"
    );
    assert_ne!(
        digest(SourceId(0), "/a/b"),
        digest(SourceId(0), "/a/B"),
        "case belongs to the path, whatever the filesystem thinks of it"
    );
}

#[test]
fn two_writers_over_the_same_input_agree_byte_for_byte() {
    let mut a = IdWriter::new();
    let mut b = IdWriter::new();
    for n in 0..100u64 {
        a.push(S, &path(n), n as u32);
    }
    for n in (0..100u64).rev() {
        b.push(S, &path(n), n as u32);
    }
    assert_eq!(a.finish(), b.finish());
}

#[test]
fn a_table_of_any_size_says_what_a_sorted_list_would() {
    // Sizes on both sides of every bucket count, sources so hashes collide
    // on nothing but luck, and every reader against the plain sorted pairs.
    for n in [0usize, 1, 2, 31, 32, 33, 63, 64, 65, 500, 1023, 1024, 4097] {
        let mut w = IdWriter::new();
        let mut pairs: Vec<(u32, u32)> = Vec::new();
        for row in 0..n {
            // Every third path twice, so some hashes hold two rows.
            let p = path((row - row % 3 / 2) as u64);
            w.push(S, &p, row as u32);
            pairs.push((IdMap::key_of(S, &p), row as u32));
        }
        pairs.sort_unstable();
        let bytes = w.finish();
        let map = IdMap::open(&bytes).expect("open");
        assert_eq!(map.len(), n);
        assert_eq!(map.iter().collect::<Vec<_>>(), pairs, "{n} entries");
        for &(h, _) in &pairs {
            let want: Vec<u32> = pairs.iter().filter(|p| p.0 == h).map(|p| p.1).collect();
            assert_eq!(map.rows_for(h).collect::<Vec<_>>(), want, "{n} entries");
        }
        for h in [0, 1, u32::MAX, 0x8000_0000, 0x7fff_ffff] {
            let want: Vec<u32> = pairs.iter().filter(|p| p.0 == h).map(|p| p.1).collect();
            assert_eq!(map.rows_for(h).collect::<Vec<_>>(), want);
        }
        // A join against keys that hold every other hash, some absent ones
        // and a repeat.
        let mut keys: Vec<u32> = pairs.iter().step_by(2).map(|p| p.0).collect();
        keys.extend([0, 7, u32::MAX]);
        keys.extend(pairs.first().map(|p| p.0));
        keys.sort_unstable();
        // And against many times as many keys, where the table is walked.
        let mut many: Vec<u32> = (0..n as u32 * 5 + 9)
            .map(|i| i.wrapping_mul(0x9e37_79b9))
            .collect();
        many.extend(keys.iter().copied());
        many.sort_unstable();
        for keys in [keys, many] {
            let mut got = Vec::new();
            map.join(keys.len(), |j| keys[j], |row, j| got.push((row, j)));
            got.sort_unstable();
            let mut want = Vec::new();
            for &(h, row) in &pairs {
                for (j, &k) in keys.iter().enumerate() {
                    if k == h {
                        want.push((row, j));
                    }
                }
            }
            want.sort_unstable();
            assert_eq!(got, want, "{n} entries, {} keys", keys.len());
        }
    }
}

#[test]
fn a_table_cut_or_scribbled_on_is_refused_or_read_and_never_a_panic() {
    let mut w = IdWriter::new();
    for n in 0..700u64 {
        w.push(S, &path(n), n as u32);
    }
    let bytes = w.finish();
    for cut in 0..bytes.len() {
        assert!(IdMap::open(&bytes[..cut]).is_none(), "cut at {cut}");
    }
    let mut longer = bytes.clone();
    longer.push(0);
    assert!(IdMap::open(&longer).is_none());
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    for _ in 0..300 {
        let mut bad = bytes.clone();
        for _ in 0..6 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let at = 4 + (state >> 33) as usize % (bad.len() - 4);
            bad[at] = (state >> 13) as u8;
        }
        let map = IdMap::open(&bad).expect("the length is still right");
        assert!(map.iter().count() <= 700);
        for n in 0..700u64 {
            assert!(map.candidates(S, &path(n)).count() <= 700);
        }
        map.join(3, |j| [0, 9, u32::MAX][j], |_, _| {});
    }
}

#[test]
fn five_million_rows_are_under_five_bytes_each() {
    // 2^18 buckets and 14 + 23 bits an entry; nothing is built to say so.
    let len = 5_056_158usize;
    let bits = bucket_bits(len);
    assert_eq!(bits, 18);
    let bytes = 4
        + ((1usize << bits) + 1) * 4
        + packed::len(len, 32 - bits + packed::width_for(len)).expect("len");
    assert!(bytes < len * 5, "{bytes}");
}

#[test]
fn an_empty_table_is_readable() {
    let bytes = IdWriter::new().finish();
    let map = IdMap::open(&bytes).expect("open");
    assert!(map.is_empty());
    assert_eq!(map.candidates(S, "/a").count(), 0);
    assert!(IdMap::open(&[]).is_none());
}
