//! Does `DirTable::depths` agree with counting the slashes in `get`?
use scour_index_native::Live;

fn main() {
    let dir = std::env::args().nth(1).expect("index dir");
    let dir = std::path::Path::new(&dir);
    let mut best: Option<(u64, u64)> = None;
    for e in std::fs::read_dir(dir).expect("read_dir").flatten() {
        let n = e.file_name();
        let n = n.to_string_lossy();
        if !n.ends_with(".dirs") {
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
    let (number, _) = best.expect("no segment");
    let live = Live::open(dir, number, 0).expect("open");
    let seg = live.view().expect("view");
    let fast = seg.dirs.depths();
    println!("{} dizin", fast.len());
    let mut bad = 0;
    for id in 0..fast.len() as u32 {
        let slow = seg
            .dirs
            .get(id)
            .map(|p| p.bytes().filter(|&b| b == b'/').count())
            .unwrap_or(0);
        if slow != fast[id as usize] as usize {
            if bad < 5 {
                println!(
                    "  {id}: hızlı {} yavaş {slow}  {:?}",
                    fast[id as usize],
                    seg.dirs.get(id)
                );
            }
            bad += 1;
        }
    }
    println!("uyuşmayan: {bad}");
}
