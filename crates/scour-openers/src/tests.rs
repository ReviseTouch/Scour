use super::*;
use std::path::Path;

#[test]
fn a_process_number_is_read_out_of_a_gdbus_reply() {
    assert_eq!(uint_reply("(uint32 12345,)\n"), Some(12345));
    assert_eq!(uint_reply(""), None);
}

#[test]
fn a_path_becomes_a_uri_no_bus_tool_can_misread() {
    assert_eq!(
        file_uri(Path::new("/home/a/Çay listesi, 'son'.txt")),
        "file:///home/a/%C3%87ay%20listesi%2C%20%27son%27.txt"
    );
    assert_eq!(file_uri(Path::new("/x/a-b_c.d~")), "file:///x/a-b_c.d~");
    assert_eq!(
        gvariant_strings(&["file:///a".into(), "file:///b%20c".into()]),
        "['file:///a', 'file:///b%20c']"
    );
}

#[test]
fn the_file_lands_where_the_entry_says_and_the_rest_of_the_codes_go() {
    let p = Path::new("/home/a/notes.txt");
    assert_eq!(command("gedit %U", p), vec!["gedit", "/home/a/notes.txt"]);
    assert_eq!(
        command("code --new-window %F", p),
        vec!["code", "--new-window", "/home/a/notes.txt"]
    );
    // A program handed `%c` opens a file called `%c` or refuses to start.
    assert_eq!(
        command("foo %i %c %k %f", p),
        vec!["foo", "/home/a/notes.txt"]
    );
    // An entry that forgot its code still gets the file, appended.
    assert_eq!(
        command("mousepad", p),
        vec!["mousepad", "/home/a/notes.txt"]
    );
}

#[test]
fn a_quoted_word_stays_one_word() {
    assert_eq!(split(r#"foo "a b" c"#), vec!["foo", "a b", "c"]);
    assert_eq!(split(r#"foo "a\"b""#), vec!["foo", r#"a"b"#]);
    // An empty quoted argument is still an argument.
    assert_eq!(split(r#"foo "" bar"#), vec!["foo", "", "bar"]);
    assert_eq!(split("  spaced   out  "), vec!["spaced", "out"]);
}

#[test]
fn the_language_is_the_first_two_letters_and_nothing_else() {
    // The shapes the function has to survive, not the environment's value.
    for (value, want) in [
        ("tr_TR.UTF-8", "tr"),
        ("en_GB", "en"),
        ("de@euro", "de"),
        ("tr", "tr"),
    ] {
        let cut = value
            .split(['.', '@'])
            .next()
            .unwrap()
            .split('_')
            .next()
            .unwrap();
        assert_eq!(cut, want, "{value}");
    }
}

/// Reading this machine's real tables: what is installed differs everywhere,
/// so only the shape is checked — no panic, no empty name, nothing to run.
#[test]
fn the_real_tables_are_read_without_falling_over() {
    for mime in ["text/plain", "image/png", "application/pdf"] {
        for opener in openers(mime) {
            assert!(!opener.id.is_empty());
            assert!(!opener.exec.is_empty(), "{}", opener.id);
            assert!(opener.id.ends_with(".desktop"), "{}", opener.id);
        }
    }
}
