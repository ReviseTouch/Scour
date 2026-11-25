//! The time, type and size terms in the query line drawn as their words
//! while the caret is outside them: `dm:` as `time_said`, a `kind:` or
//! `ext:` list as its values with `·` between and `+N` for the rest when
//! the line is narrow, a `size:` or `empty:` as its preset. A term the words
//! cannot say is drawn as typed.

use scour_ui::lens::{self, SIZE_PRESETS, Term};

use super::placecap::Folded;
use crate::app::App;

/// One term's words: a list's values, of which the first `shown` are drawn.
struct Said {
    term: Term,
    values: Vec<String>,
    shown: usize,
}

impl Said {
    fn label(&self) -> String {
        let mut out = String::from(if self.term.not { "!" } else { "" });
        out.push_str(&self.values[..self.shown].join(" · "));
        if self.shown < self.values.len() {
            out.push_str(&format!(" +{}", self.values.len() - self.shown));
        }
        out
    }
}

/// The query's filter terms drawn as words. `room` is the field's width:
/// while the line is wider, the longest list gives up a value at a time.
pub fn folded(app: &App, room: usize) -> Vec<Folded> {
    let caret = app.caret.min(app.query.len());
    let now = crate::v2::unix_now();
    let offset = scour_ui::format::local_offset(now);
    let mut said: Vec<Said> = lens::terms(&app.query)
        .into_iter()
        .filter(|t| !(t.start < caret && caret <= t.end))
        .filter_map(|t| {
            let values = words(app, &t, now, offset)?;
            Some(Said {
                shown: values.len(),
                values,
                term: t,
            })
        })
        .collect();
    let own = |s: &Said| app.query[s.term.start..s.term.end].chars().count();
    let rest = app.query.chars().count() - said.iter().map(own).sum::<usize>();
    loop {
        let wide = rest
            + said
                .iter()
                .map(|s| s.label().chars().count())
                .sum::<usize>();
        let longest = said
            .iter_mut()
            .filter(|s| s.shown > 1)
            .max_by_key(|s| s.label().chars().count());
        match longest {
            Some(s) if wide > room => s.shown -= 1,
            _ => break,
        }
    }
    said.into_iter()
        .map(|s| Folded {
            start: s.term.start,
            end: s.term.end,
            kind: None,
            label: s.label(),
            place: false,
            not: s.term.not,
        })
        .collect()
}

/// What a term says, a value a word; `None` for one drawn as typed.
fn words(app: &App, t: &Term, now: i64, offset: i64) -> Option<Vec<String>> {
    if lens::is_empty_term(&app.query, t) {
        return Some(vec![app.say(SIZE_PRESETS[0].label).into_owned()]);
    }
    let one = |s: String| Some(vec![s]);
    match t.field? {
        "dm" => one(app.time_words(&t.value, now, offset)?),
        "size" => {
            let i = lens::size_preset_of(&app.query, t)?;
            one(app.say(SIZE_PRESETS[i].label).into_owned())
        }
        field @ ("kind" | "ext") => list(app, field, &t.value),
        _ => None,
    }
}

/// A list's values in words. A kind it does not know leaves the term as
/// typed: a mistake stays seen.
fn list(app: &App, field: &str, value: &str) -> Option<Vec<String>> {
    let mut values = Vec::new();
    for v in value.split(';').map(str::trim).filter(|v| !v.is_empty()) {
        let key = lens::value_key(field, v);
        values.push(match field {
            "kind" => app.say(lens::kind_label(&key)?).into_owned(),
            _ => format!(".{key}"),
        });
    }
    (!values.is_empty()).then_some(values)
}
