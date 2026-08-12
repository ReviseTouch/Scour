//! What a second pass over the query text costs, against a parse.
fn main() {
    let queries = [
        "",
        "rapor",
        "ext:rs engine",
        "ext:rs;toml under:/home/hasan dm:7d size:>1mb !target",
        "kind:zurna dm:yarin size:>abc ext:",
    ];
    for q in queries {
        let n = 20_000;
        let t = std::time::Instant::now();
        for _ in 0..n {
            std::hint::black_box(scour_query::parse(std::hint::black_box(q)));
        }
        let parse = t.elapsed().as_nanos() as f64 / n as f64;
        let t = std::time::Instant::now();
        for _ in 0..n {
            std::hint::black_box(scour_query::spans(std::hint::black_box(q)));
        }
        let spans = t.elapsed().as_nanos() as f64 / n as f64;
        println!("{parse:8.0} ns parse  {spans:8.0} ns spans   {q:?}");
    }
}
