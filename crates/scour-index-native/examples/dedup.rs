use scour_index_native::Live;
fn main() {
    let dir = std::env::args().nth(1).expect("dir");
    let dir = std::path::Path::new(&dir);
    let mut best: Option<(u64, u64)> = None;
    for e in std::fs::read_dir(dir).expect("rd").flatten() {
        let n = e.file_name();
        let n = n.to_string_lossy();
        if !n.ends_with(".names") {
            continue;
        }
        let Some(num) = n
            .strip_prefix("seg-")
            .and_then(|r| r.split('.').next())
            .and_then(|x| x.parse::<u64>().ok())
        else {
            continue;
        };
        let len = e.metadata().map(|m| m.len()).unwrap_or(0);
        if best.is_none_or(|(_, b)| len > b) {
            best = Some((num, len));
        }
    }
    let (num, _) = best.expect("none");
    let live = Live::open(dir, num, 0).expect("open");
    let seg = live.view().expect("view");
    let rows = seg.rows();
    let (mut same, mut diff, mut same_bytes, mut diff_bytes) = (0usize, 0usize, 0usize, 0usize);
    for r in 0..rows {
        let raw = seg.names.get(r).unwrap_or("");
        let fold = seg.folded.get(r).unwrap_or("");
        if raw == fold {
            same += 1;
            same_bytes += raw.len() + 1;
        } else {
            diff += 1;
            diff_bytes += raw.len() + 1;
        }
    }
    let total = same_bytes + diff_bytes;
    println!("  {rows} satır");
    println!(
        "  katlanmışı kendisiyle AYNI : {same:>9}  (%{:.1})  {:.1} MB",
        same as f64 * 100.0 / rows as f64,
        same_bytes as f64 / 1048576.0
    );
    println!(
        "  FARKLI                     : {diff:>9}  (%{:.1})  {:.1} MB",
        diff as f64 * 100.0 / rows as f64,
        diff_bytes as f64 / 1048576.0
    );
    println!();
    println!(
        "  bugün iki arena            : {:.1} MB",
        total as f64 * 2.0 / 1048576.0
    );
    println!(
        "  tek arena + istisnalar     : {:.1} MB",
        (total + diff_bytes) as f64 / 1048576.0
    );
    println!(
        "  kazanç                     : {:.1} MB",
        (total - diff_bytes) as f64 / 1048576.0
    );
}
