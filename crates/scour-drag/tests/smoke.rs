//! The payload a drop target reads.

use std::path::Path;

use scour_drag::{file_uri, uri_list};

#[test]
fn a_path_becomes_a_file_uri_with_its_odd_bytes_escaped() {
    assert_eq!(file_uri(Path::new("/home/u/a.txt")), "file:///home/u/a.txt");
    assert_eq!(
        file_uri(Path::new("/home/u/My Files/#1 çay.pdf")),
        "file:///home/u/My%20Files/%231%20%C3%A7ay.pdf"
    );
}

#[test]
fn a_list_ends_every_line_with_crlf() {
    let list = uri_list(&[Path::new("/a"), Path::new("/b c")]);
    assert_eq!(list, "file:///a\r\nfile:///b%20c\r\n");
}
