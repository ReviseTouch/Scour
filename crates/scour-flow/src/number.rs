//! A spreadsheet cell's number as the sheet shows it: its value through the
//! cell's format (`#,##0.00`, `dd.mm.yyyy`, `hh:mm`, `0%`), punctuated the way
//! the reader's language punctuates. What a format cannot be read as, it shows
//! as Excel's General.

/// How numbers and dates are written where they are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Locale {
    pub decimal: char,
    pub group: char,
    pub months: [&'static str; 12],
    pub days: [&'static str; 7],
}

const MONTHS_EN: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const DAYS_EN: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const MONTHS_TR: [&str; 12] = [
    "Ocak", "Şubat", "Mart", "Nisan", "Mayıs", "Haziran", "Temmuz", "Ağustos", "Eylül", "Ekim",
    "Kasım", "Aralık",
];
const DAYS_TR: [&str; 7] = [
    "Pazar",
    "Pazartesi",
    "Salı",
    "Çarşamba",
    "Perşembe",
    "Cuma",
    "Cumartesi",
];

impl Locale {
    /// English names, with the given marks.
    pub fn new(decimal: char, group: char) -> Locale {
        Locale {
            decimal,
            group,
            months: MONTHS_EN,
            days: DAYS_EN,
        }
    }

    /// For a language code (`tr`, `en`, `tr-TR`): its names and its marks.
    pub fn of(language: &str) -> Locale {
        if language.to_ascii_lowercase().starts_with("tr") {
            Locale {
                decimal: ',',
                group: '.',
                months: MONTHS_TR,
                days: DAYS_TR,
            }
        } else {
            Locale::new('.', ',')
        }
    }
}

impl Default for Locale {
    fn default() -> Self {
        Locale::new('.', ',')
    }
}

/// `value` through `format`.
pub fn show(value: f64, format: &str, locale: &Locale) -> String {
    if !value.is_finite() {
        return general(value, locale);
    }
    let sections = split(format);
    // Positive; negative; zero; text — each optional past the first.
    let (section, signed) = match (sections.len(), value) {
        (n, v) if v < 0.0 && n >= 2 => (sections[1].as_str(), false),
        (n, v) if v == 0.0 && n >= 3 => (sections[2].as_str(), true),
        _ => (sections.first().map_or("", String::as_str), true),
    };
    let section = strip_brackets(section);
    let plain = section.trim();
    if plain.is_empty() || plain.eq_ignore_ascii_case("general") || plain == "@" {
        return general(value, locale);
    }
    if is_date(&section) {
        return date(value, &section, locale).unwrap_or_else(|| general(value, locale));
    }
    let v = if signed { value } else { value.abs() };
    number(v, &section, locale).unwrap_or_else(|| general(value, locale))
}

/// Excel's General: an integer as one, else up to eleven significant digits.
pub fn general(value: f64, locale: &Locale) -> String {
    if !value.is_finite() {
        return "#NUM!".into();
    }
    if value == value.trunc() && value.abs() < 1e15 {
        return format!("{}", value as i64);
    }
    if value.abs() >= 1e11 || value.abs() < 1e-9 {
        let s = format!("{value:.5E}");
        return s.replace('.', &locale.decimal.to_string());
    }
    let digits = 10 - (value.abs().log10().floor() as i32).clamp(-10, 10);
    let s = format!("{:.*}", digits.clamp(0, 10) as usize, value);
    let s = if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        s
    };
    s.replace('.', &locale.decimal.to_string())
}

/// The sections of a format, split at `;` outside quotes and escapes.
fn split(format: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut quoted = false;
    let mut chars = format.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                out.last_mut().unwrap().push(c);
            }
            '\\' if !quoted => {
                out.last_mut().unwrap().push(c);
                if let Some(n) = chars.next() {
                    out.last_mut().unwrap().push(n);
                }
            }
            ';' if !quoted => out.push(String::new()),
            c => out.last_mut().unwrap().push(c),
        }
    }
    out
}

/// A section without its `[Red]`, `[>100]` and `[$-41F]`; a currency's
/// symbol in `[$₺-41F]` is kept as text, and `[h]` stays for the clock.
fn strip_brackets(section: &str) -> String {
    let mut out = String::new();
    let mut rest = section;
    while let Some(at) = rest.find('[') {
        out.push_str(&rest[..at]);
        let Some(end) = rest[at..].find(']') else {
            rest = &rest[at..];
            break;
        };
        let inner = &rest[at + 1..at + end];
        let lower = inner.to_ascii_lowercase();
        if lower == "h"
            || lower == "hh"
            || lower == "m"
            || lower == "mm"
            || lower == "s"
            || lower == "ss"
        {
            out.push('[');
            out.push_str(inner);
            out.push(']');
        } else if let Some(cur) = inner.strip_prefix('$') {
            let symbol = cur.split('-').next().unwrap_or("");
            if !symbol.is_empty() {
                out.push('"');
                out.push_str(symbol);
                out.push('"');
            }
        }
        rest = &rest[at + end + 1..];
    }
    out.push_str(rest);
    out
}

/// Whether a section writes a date or a time: a letter of one outside quotes.
fn is_date(section: &str) -> bool {
    let mut quoted = false;
    let mut escaped = false;
    for c in section.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '"' => quoted = !quoted,
            '\\' => escaped = true,
            'y' | 'Y' | 'd' | 'D' | 'h' | 'H' | 's' | 'S' | 'm' | 'M' if !quoted => return true,
            _ => {}
        }
    }
    false
}

#[derive(Debug, PartialEq)]
enum Tok {
    Lit(String),
    Year(usize),
    Month(usize),
    Minute(usize),
    Day(usize),
    Hour(usize, bool),
    Second(usize),
    AmPm(bool),
}

fn date(value: f64, section: &str, locale: &Locale) -> Option<String> {
    let mut toks = Vec::new();
    let mut chars: Vec<char> = section.chars().collect();
    chars.push('\0');
    let mut i = 0;
    let lit = |toks: &mut Vec<Tok>, s: &str| match toks.last_mut() {
        Some(Tok::Lit(l)) => l.push_str(s),
        _ => toks.push(Tok::Lit(s.to_owned())),
    };
    while chars[i] != '\0' {
        let c = chars[i];
        let run = |c: char| {
            let mut n = 0;
            while chars[i + n].eq_ignore_ascii_case(&c) {
                n += 1;
            }
            n
        };
        match c {
            '"' => {
                let mut s = String::new();
                i += 1;
                while chars[i] != '"' && chars[i] != '\0' {
                    s.push(chars[i]);
                    i += 1;
                }
                lit(&mut toks, &s);
                i += 1;
            }
            '\\' => {
                lit(&mut toks, &chars[i + 1].to_string());
                i += 2;
            }
            '_' | '*' => i += 2,
            '[' => {
                let n = chars[i + 1..].iter().position(|&c| c == ']').unwrap_or(0);
                let inner: String = chars[i + 1..i + 1 + n].iter().collect();
                if inner.eq_ignore_ascii_case("h") || inner.eq_ignore_ascii_case("hh") {
                    toks.push(Tok::Hour(inner.len(), true));
                }
                i += n + 2;
            }
            'y' | 'Y' => {
                let n = run(c);
                toks.push(Tok::Year(n));
                i += n;
            }
            'd' | 'D' => {
                let n = run(c);
                toks.push(Tok::Day(n));
                i += n;
            }
            'h' | 'H' => {
                let n = run(c);
                toks.push(Tok::Hour(n, false));
                i += n;
            }
            's' | 'S' => {
                let n = run(c);
                toks.push(Tok::Second(n));
                i += n;
            }
            'm' | 'M' => {
                let n = run(c);
                toks.push(Tok::Month(n));
                i += n;
            }
            'a' | 'A'
                if section[byte_at(&chars, i)..]
                    .to_ascii_uppercase()
                    .starts_with("AM/PM") =>
            {
                toks.push(Tok::AmPm(true));
                i += 5;
            }
            'a' | 'A'
                if section[byte_at(&chars, i)..]
                    .to_ascii_uppercase()
                    .starts_with("A/P") =>
            {
                toks.push(Tok::AmPm(false));
                i += 3;
            }
            '0' if matches!(toks.last(), Some(Tok::Lit(l)) if l.ends_with('.')) => {
                // Fractions of a second: dropped, and the point before them.
                if let Some(Tok::Lit(l)) = toks.last_mut() {
                    l.pop();
                }
                while chars[i] == '0' {
                    i += 1;
                }
            }
            c => {
                lit(&mut toks, &c.to_string());
                i += 1;
            }
        }
    }
    // An `m` right after an hour, or right before a second, is minutes.
    let len = toks.len();
    for k in 0..len {
        if let Tok::Month(n) = toks[k] {
            let after_hour = toks[..k]
                .iter()
                .rev()
                .find(|t| !matches!(t, Tok::Lit(_)))
                .is_some_and(|t| matches!(t, Tok::Hour(..)));
            let before_second = toks[k + 1..]
                .iter()
                .find(|t| !matches!(t, Tok::Lit(_)))
                .is_some_and(|t| matches!(t, Tok::Second(_)));
            if (after_hour || before_second) && n <= 2 {
                toks[k] = Tok::Minute(n);
            }
        }
    }
    let twelve = toks.iter().any(|t| matches!(t, Tok::AmPm(_)));
    // Excel's day zero is 1899-12-30 for every serial past its 1900 leap day.
    let days = value.floor() as i64;
    let mut secs = ((value - value.floor()) * 86_400.0).round() as i64;
    let days = days + secs / 86_400;
    secs %= 86_400;
    let (y, m, d) = civil(days - 25_569);
    let weekday = (days + 6).rem_euclid(7) as usize;
    let (hh, mi, ss) = (secs / 3600, secs / 60 % 60, secs % 60);
    let mut out = String::new();
    for t in &toks {
        match *t {
            Tok::Lit(ref s) => out.push_str(s),
            Tok::Year(n) if n <= 2 => out.push_str(&format!("{:02}", y.rem_euclid(100))),
            Tok::Year(_) => out.push_str(&y.to_string()),
            Tok::Month(1) => out.push_str(&m.to_string()),
            Tok::Month(2) => out.push_str(&format!("{m:02}")),
            Tok::Month(3) => out.extend(locale.months[m as usize - 1].chars().take(3)),
            Tok::Month(5) => out.extend(locale.months[m as usize - 1].chars().take(1)),
            Tok::Month(_) => out.push_str(locale.months[m as usize - 1]),
            Tok::Minute(1) => out.push_str(&mi.to_string()),
            Tok::Minute(_) => out.push_str(&format!("{mi:02}")),
            Tok::Day(1) => out.push_str(&d.to_string()),
            Tok::Day(2) => out.push_str(&format!("{d:02}")),
            Tok::Day(3) => out.extend(locale.days[weekday].chars().take(3)),
            Tok::Day(_) => out.push_str(locale.days[weekday]),
            Tok::Hour(_, true) => out.push_str(&(days * 24 + hh).to_string()),
            Tok::Hour(n, false) => {
                let h = if twelve { (hh + 11) % 12 + 1 } else { hh };
                if n >= 2 {
                    out.push_str(&format!("{h:02}"));
                } else {
                    out.push_str(&h.to_string());
                }
            }
            Tok::Second(1) => out.push_str(&ss.to_string()),
            Tok::Second(_) => out.push_str(&format!("{ss:02}")),
            Tok::AmPm(full) => out.push_str(match (hh < 12, full) {
                (true, true) => "AM",
                (false, true) => "PM",
                (true, false) => "A",
                (false, false) => "P",
            }),
        }
    }
    Some(out)
}

fn byte_at(chars: &[char], i: usize) -> usize {
    chars[..i].iter().map(|c| c.len_utf8()).sum()
}

/// Days since 1970-01-01 as a civil date (Howard Hinnant's algorithm).
fn civil(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// A number section: text around one pattern of `0`, `#`, `?`, `.`, `,`.
fn number(value: f64, section: &str, locale: &Locale) -> Option<String> {
    let mut prefix = String::new();
    let mut suffix = String::new();
    let mut pattern = String::new();
    let mut percent = 0;
    let mut chars = section.chars().peekable();
    let mut seen_digit = false;
    let mut scientific = false;
    while let Some(c) = chars.next() {
        // Text before the pattern is its prefix; once a digit is seen, its suffix.
        let after = seen_digit || !pattern.is_empty();
        let open = suffix.is_empty();
        let mut text = String::new();
        match c {
            '"' => {
                for q in chars.by_ref() {
                    if q == '"' {
                        break;
                    }
                    text.push(q);
                }
            }
            '\\' => {
                if let Some(n) = chars.next() {
                    text.push(n);
                }
            }
            '_' => {
                chars.next();
                text.push(' ');
            }
            '*' => {
                chars.next();
            }
            '%' => {
                percent += 1;
                text.push('%');
            }
            'E' | 'e' if seen_digit => {
                scientific = true;
                // The exponent's own pattern is not shaped further.
                while chars.peek().is_some_and(|c| "+-0#".contains(*c)) {
                    chars.next();
                }
            }
            '0' | '#' | '?' | '.' | ',' if open => {
                if c != ',' || seen_digit {
                    pattern.push(c);
                }
                if matches!(c, '0' | '#' | '?') {
                    seen_digit = true;
                }
            }
            '/' => return None,
            c => text.push(c),
        }
        if after {
            suffix.push_str(&text);
        } else {
            prefix.push_str(&text);
        }
    }
    if !seen_digit {
        // Text with no digits at all: the text itself, as Excel shows it.
        return Some(format!("{prefix}{suffix}"));
    }
    let v = value * 100f64.powi(percent);
    let (int_pat, frac_pat) = match pattern.split_once('.') {
        Some((i, f)) => (i.to_owned(), f.replace(',', "")),
        None => (pattern.clone(), String::new()),
    };
    let group = int_pat.contains(',');
    let int_min = int_pat.chars().filter(|&c| c == '0').count();
    let frac_max = frac_pat
        .chars()
        .filter(|c| matches!(c, '0' | '#' | '?'))
        .count();
    let frac_min = frac_pat.chars().filter(|&c| c == '0').count();
    if scientific {
        let s = format!("{:.*E}", frac_max, v.abs());
        let (m, e) = s.split_once('E')?;
        let e: i32 = e.parse().ok()?;
        let sign = if v < 0.0 { "-" } else { "" };
        let m = m.replace('.', &locale.decimal.to_string());
        return Some(format!(
            "{prefix}{sign}{m}E{}{:02}{suffix}",
            if e < 0 { '-' } else { '+' },
            e.abs()
        ));
    }
    let rounded = format!("{:.*}", frac_max, v.abs());
    let (int_digits, frac_digits) = match rounded.split_once('.') {
        Some((i, f)) => (i.to_owned(), f.to_owned()),
        None => (rounded.clone(), String::new()),
    };
    let mut frac = frac_digits;
    while frac.len() > frac_min && frac.ends_with('0') {
        frac.pop();
    }
    let int_digits = if int_digits == "0" && int_min == 0 {
        String::new()
    } else {
        format!("{:0>width$}", int_digits, width = int_min)
    };
    let int_text = if group {
        grouped(&int_digits, locale.group)
    } else {
        int_digits
    };
    let mut out = prefix;
    if v < 0.0 && rounded.chars().any(|c| matches!(c, '1'..='9')) {
        out.push('-');
    }
    out.push_str(&int_text);
    if !frac.is_empty() {
        out.push(locale.decimal);
        out.push_str(&frac);
    }
    out.push_str(&suffix);
    Some(out)
}

fn grouped(digits: &str, mark: char) -> String {
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(mark);
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tr(v: f64, f: &str) -> String {
        show(v, f, &Locale::of("tr"))
    }
    fn en(v: f64, f: &str) -> String {
        show(v, f, &Locale::of("en"))
    }

    #[test]
    fn dates_and_times_from_their_serials() {
        assert_eq!(tr(46273.0, "dd\\.mm\\.yyyy"), "08.09.2026");
        assert_eq!(tr(0.392_361_111_111_111_1, "hh:mm"), "09:25");
        assert_eq!(en(46273.0, "yyyy-mm-dd"), "2026-09-08");
        assert_eq!(en(46273.0, "d mmm yy"), "8 Sep 26");
        assert_eq!(tr(46273.0, "d mmmm yyyy dddd"), "8 Eylül 2026 Salı");
        assert_eq!(en(46273.75, "m/d/yyyy h:mm AM/PM"), "9/8/2026 6:00 PM");
        assert_eq!(en(1.5, "[h]:mm:ss"), "36:00:00");
        assert_eq!(en(0.5, "mm:ss.0"), "00:00");
    }

    #[test]
    fn numbers_by_their_pattern_and_the_reader_s_marks() {
        assert_eq!(tr(1234567.891, "#,##0.00"), "1.234.567,89");
        assert_eq!(en(1234567.891, "#,##0.00"), "1,234,567.89");
        assert_eq!(en(0.256, "0%"), "26%");
        assert_eq!(en(0.256, "0.0%"), "25.6%");
        assert_eq!(en(7.0, "000"), "007");
        assert_eq!(en(3.10, "0.0#"), "3.1");
        assert_eq!(en(-12.5, "#,##0.00;(#,##0.00)"), "(12.50)");
        assert_eq!(en(-12.5, "0.0"), "-12.5");
        assert_eq!(en(0.0, "0.00;-0.00;\"–\""), "–");
        assert_eq!(tr(1500.0, "#,##0.00\\ \"₺\""), "1.500,00 ₺");
        assert_eq!(tr(1500.0, "[$₺-41F] #,##0"), "₺ 1.500");
        assert_eq!(en(12345.0, "0.00E+00"), "1.23E+04");
    }

    #[test]
    fn general_is_what_excel_shows_with_no_format() {
        assert_eq!(en(42.0, "General"), "42");
        assert_eq!(tr(0.1 + 0.2, "General"), "0,3");
        assert_eq!(en(1.0 / 3.0, ""), "0.3333333333");
        assert_eq!(en(42.0, "@"), "42");
        // A format it cannot read is not a reason to show nothing.
        assert_eq!(en(0.75, "# ?/?"), "0.75");
    }
}
