//! Print what `scour_volumes::look` finds on this machine.

fn main() {
    for v in scour_volumes::look() {
        println!(
            "{:<10} {:<28} {:<8} {:>9} MB  {:<14} {}",
            format!("{:?}", v.kind),
            v.roots.join(" "),
            v.fs,
            v.size >> 20,
            v.label,
            v.id
        );
    }
}
