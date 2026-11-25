//! ntfsraw MODE LIST — one NTFS volume's files read two ways, for measuring.
//!
//!   normal  each file opened and read through the system, in the order given
//!   raw     each file's extents asked for (an open for attributes only), then
//!           the volume itself read in place order, uncached, in large spans;
//!           files inside their MFT record taken from the record; compressed,
//!           encrypted and sparse files read the normal way
//!   mft     as raw, but every file's extents taken from the MFT, read once
//!           from start to end: no file is opened
//!   verify  raw and normal, every 4 KiB block of every file compared
//!   verify-mft  the same with mft
//!
//! Paths on LIST, one a line, all on one volume. `raw` and `verify` need an
//! elevated token: the volume is opened as a device.

#[cfg(windows)]
mod mft;
#[cfg(windows)]
mod win;

fn main() {
    #[cfg(windows)]
    win::main();
    #[cfg(not(windows))]
    eprintln!("Windows only");
}
