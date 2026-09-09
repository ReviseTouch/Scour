//! Ask the desktop for pictures of these files and say what happened, including
//! how many processes a batch actually started. Point `XDG_CACHE_HOME` at your
//! own directory, never the desktop's real cache.
//!
//! `XDG_CACHE_HOME=/var/tmp/probe cargo run -p scour-thumbs --example ask -- <path>...`

fn main() {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    if paths.is_empty() {
        eprintln!("usage: ask <path>...");
        std::process::exit(2);
    }

    println!("cache:        {}", scour_thumbs::cache::dir().display());
    println!(
        "thumbnailers: {} types this machine can draw",
        scour_thumbs::known::known().types()
    );
    println!();

    let wanted: Vec<scour_thumbs::Wanted> = paths
        .iter()
        .map(|path| scour_thumbs::Wanted {
            path: path.clone(),
            mtime: mtime_of(path),
        })
        .collect();

    for (n, it) in wanted.iter().enumerate() {
        println!(
            "{n}. {}  mime={}  drawable={}  cached={}  failed={}",
            it.path,
            mime_of(&it.path),
            scour_thumbs::can_make(&it.path),
            scour_thumbs::cache::existing(&it.path).is_some(),
            scour_thumbs::cache::has_failed(&it.path, it.mtime),
        );
    }

    let maker = scour_thumbs::Maker::default();
    let began = std::time::Instant::now();
    let made = maker.make(&wanted);
    let took = began.elapsed();

    println!();
    println!(
        "ran {} process(es), {} of {} ready, in {:.0} ms",
        made.ran,
        made.ready.len(),
        wanted.len().min(scour_thumbs::Maker::BATCH),
        took.as_secs_f64() * 1000.0
    );
    for path in &made.ready {
        let at = scour_thumbs::cache::existing(path);
        println!(
            "  ready  {path}\n         -> {}",
            at.map_or("(gone?)".into(), |p| p.display().to_string())
        );
    }
    for it in &wanted {
        if !made.ready.contains(&it.path) {
            println!(
                "  no     {}\n         failure noted at {}",
                it.path,
                scour_thumbs::cache::failure(&it.path).display()
            );
        }
    }
}

fn mime_of(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    scour_thumbs::known::known().mime_of(name).unwrap_or("-")
}

fn mtime_of(path: &str) -> i64 {
    let Ok(meta) = std::fs::metadata(path) else {
        return 0;
    };
    let Ok(when) = meta.modified() else {
        return 0;
    };
    when.duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
