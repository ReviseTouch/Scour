//! What this machine offers for a kind of file, including whether the starred
//! choice is the one `xdg-mime query default` names.
//!
//!     cargo run -p scour-openers --example probe
//!     cargo run -p scour-openers --example probe -- text/markdown image/png

fn main() {
    let asked: Vec<String> = std::env::args().skip(1).collect();
    let types: Vec<&str> = if asked.is_empty() {
        vec![
            "text/plain",
            "text/markdown",
            "application/pdf",
            "image/png",
        ]
    } else {
        asked.iter().map(String::as_str).collect()
    };
    for mime in types {
        let list = scour_openers::openers(mime);
        println!("{mime} → {}", list.len());
        for o in &list {
            println!(
                "   {} {}  ({})",
                if o.preferred { "★" } else { " " },
                o.name,
                o.id
            );
        }
    }
}
