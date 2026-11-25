//! Reads documents for the service, one at a time, for as long as it is fed:
//! a request on standard input, its answer on standard output. A document
//! that panics a library is answered `Failed`; one that kills the process
//! is the service's to notice, by the pipe closing.

use std::io::{BufReader, BufWriter};

use scour_doctext::{Format, Text};
use scour_extract::wire::{self, Answer};

fn main() {
    let (stdin, stdout) = (std::io::stdin(), std::io::stdout());
    let mut input = BufReader::new(stdin.lock());
    let mut output = BufWriter::new(stdout.lock());
    // A library's own panic message would go to the service's log as noise.
    std::panic::set_hook(Box::new(|_| {}));
    while let Ok(Some((format, cap, bytes))) = wire::read_request(&mut input) {
        let answer = answer(&format, cap, &bytes);
        if wire::write_answer(&mut output, &answer).is_err() {
            break;
        }
    }
}

fn answer(format: &str, cap: u64, bytes: &[u8]) -> Answer {
    let Some(format) = Format::named(format) else {
        return Answer::Failed(format!("no reader for {format}"));
    };
    let cap = usize::try_from(cap).unwrap_or(usize::MAX);
    match std::panic::catch_unwind(|| scour_doctext::text(bytes, format, cap)) {
        Ok(Text::Words(words)) => Answer::Words(words),
        Ok(Text::None) => Answer::NoText,
        Ok(Text::Failed(why)) => Answer::Failed(why),
        Err(_) => Answer::Failed("the reader panicked".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_format_and_a_broken_file_are_answered() {
        assert!(matches!(answer("xyz", 10, b""), Answer::Failed(_)));
        assert!(matches!(answer("pdf", 10, b"%PDF-junk"), Answer::Failed(_)));
    }
}
