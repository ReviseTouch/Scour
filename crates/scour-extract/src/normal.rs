//! One way of making text comparable, used where text is kept and where it
//! is asked about: composed (NFC), lower case with Turkish dotted and dotless
//! i kept apart, and a word broken at a line's end by a hyphen made whole.

use icu_normalizer::ComposingNormalizerBorrowed;

/// The comparable form of `text`.
pub fn normalize(text: &str) -> String {
    let composed = ComposingNormalizerBorrowed::new_nfc().normalize(text);
    let mut out = String::with_capacity(composed.len());
    let mut chars = composed.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '-' && joins(&mut chars) {
            continue;
        }
        push_lower(&mut out, c);
    }
    out
}

/// Turkish lower case: `I` is `ı` and `İ` is `i`, as the language writes them.
fn push_lower(out: &mut String, c: char) {
    match c {
        'I' => out.push('ı'),
        'İ' => out.push('i'),
        c => out.extend(c.to_lowercase()),
    }
}

/// After a hyphen: a line break, then a lower-case letter, is one word cut in
/// two. The break is consumed; the letter is left for the caller.
fn joins(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> bool {
    let mut ahead = chars.clone();
    let mut broke = false;
    while let Some(&c) = ahead.peek() {
        match c {
            '\r' | ' ' | '\t' => {}
            '\n' if !broke => broke = true,
            _ => break,
        }
        ahead.next();
    }
    let joined = broke && ahead.peek().is_some_and(|c| c.is_lowercase());
    if joined {
        *chars = ahead;
    }
    joined
}

/// The decomposed spelling of `c` (`ş` → `s` + U+0327), when it has one.
pub fn decomposed(c: char) -> Option<String> {
    let mut buf = [0u8; 4];
    let one = c.encode_utf8(&mut buf);
    let nfd = icu_normalizer::DecomposingNormalizerBorrowed::new_nfd().normalize(one);
    (nfd != *one).then(|| nfd.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turkish_letters_lower_as_turkish() {
        assert_eq!(normalize("İSTANBUL IĞDIR"), "istanbul ığdır");
        assert_eq!(normalize("ŞÇÖÜĞ"), "şçöüğ");
        assert_eq!(normalize("Iİıi"), "ıiıi");
    }

    #[test]
    fn decomposed_text_is_composed() {
        assert_eq!(normalize("s\u{327}u"), "şu");
        assert_eq!(normalize("O\u{308}"), "ö");
        assert_eq!(decomposed('ş').as_deref(), Some("s\u{327}"));
        assert_eq!(decomposed('a'), None);
    }

    #[test]
    fn a_word_broken_at_a_line_end_is_whole() {
        assert_eq!(normalize("mahke-\nmesi"), "mahkemesi");
        assert_eq!(normalize("mahke- \r\n  mesi"), "mahkemesi");
        // Not a break: a hyphen that stays, a capital after it, no line end.
        assert_eq!(normalize("Ali-\nVeli"), "ali-\nveli");
        assert_eq!(normalize("e-posta"), "e-posta");
    }
}
