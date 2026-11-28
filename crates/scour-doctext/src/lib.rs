//! The words of documents the in-process readers cannot stream: PDF, the
//! legacy Office formats and e-mail. Bytes in, plain text out, no file opened
//! here — the caller reads the file under its own identity and bounds. And a
//! word processor's document as it flows in a preview, from the same library.

mod build;
mod csv;
mod epub;
mod flow;
mod format;
mod html;
mod mail;
mod markdown;
mod msg;
mod notebook;
mod odf;
mod rtf;
mod text;

pub use flow::flow;

/// Seconds since 1970 as `2026-11-28 13:04`, in UTC (Howard Hinnant's
/// days-to-civil); empty for none.
pub(crate) fn when(secs: i64) -> String {
    if secs <= 0 {
        return String::new();
    }
    let days = secs.div_euclid(86_400);
    let rest = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year}-{month:02}-{day:02} {:02}:{:02}",
        rest / 3600,
        rest % 3600 / 60
    )
}
pub use format::Format;
pub use text::{Text, text};
