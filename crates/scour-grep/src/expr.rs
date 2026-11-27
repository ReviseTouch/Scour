//! Boolean content expressions (`a;b !c`, `a | b !c`, `"exact phrase" !skip`,
//! `~similar`) evaluated in minimal passes over candidate files.

use crate::budget::Budget;
use crate::ground::Ground;
use crate::near::Forgive;
use crate::pattern::{BadPattern, Letters, Needle, Pattern, Term};
use std::sync::Arc;

use crate::scan::{Enter, File, Options, Outcome, Scanner};
use crate::texts::Texts;

/// Conjunctive content plan: every `must` pattern (each an OR of `;`/`|` terms)
/// must match the file, and `forbid` (an OR of all `!` terms) must not.
#[derive(Debug, Clone)]
pub struct Expr {
    pub must: Vec<Pattern>,
    pub forbid: Option<Pattern>,
}

impl Expr {
    /// [`Expr::forgiving`] with no mistake forgiven: `~word` is the word.
    pub fn new(needle: &Needle, letters: impl Into<Letters>) -> Result<Expr, BadPattern> {
        Self::forgiving(needle, letters, |_| 0)
    }

    /// Parse `needle`. `Needle::Text` supports `;` / `|` (OR), whitespace (AND),
    /// `!term` (exclude file), `"..."` (literal phrase), and `~word` (the word,
    /// or as many mistakes from it as `forgive` allows; `\~` or `"~"` is the
    /// tilde itself).
    pub fn forgiving(
        needle: &Needle,
        letters: impl Into<Letters>,
        forgive: Forgive,
    ) -> Result<Expr, BadPattern> {
        let letters = letters.into();
        match needle {
            Needle::Regex(_) | Needle::Bytes(_) | Needle::Wild { .. } => Ok(Expr {
                must: vec![Pattern::new(needle, letters)?],
                forbid: None,
            }),
            Needle::Text(raw) => Self::parse_text(raw, letters, forgive),
        }
    }

    /// Each piece as `~word` and `\~` read it; nothing else in it is syntax.
    pub fn from_clauses(
        must_clauses: &[Vec<String>],
        forbid_terms: &[String],
        letters: impl Into<Letters>,
        forgive: Forgive,
    ) -> Result<Expr, BadPattern> {
        let letters = letters.into();
        let read = |pieces: &[String]| -> Vec<Term> {
            pieces.iter().map(|p| term(p, false, forgive)).collect()
        };
        let must: Vec<Vec<Term>> = must_clauses.iter().map(|c| read(c)).collect();
        Self::from_terms(&must, &read(forbid_terms), letters)
    }

    fn from_terms(
        must_clauses: &[Vec<Term>],
        forbid_terms: &[Term],
        letters: Letters,
    ) -> Result<Expr, BadPattern> {
        let mut must = Vec::with_capacity(must_clauses.len());
        for clause in must_clauses {
            must.push(Pattern::any_term(clause, letters)?);
        }
        let forbid = if forbid_terms.is_empty() {
            None
        } else {
            Some(Pattern::any_term(forbid_terms, letters)?)
        };
        if must.is_empty() && forbid.is_none() {
            return Err(BadPattern::Empty);
        }
        Ok(Expr { must, forbid })
    }

    fn parse_text(input: &str, letters: Letters, forgive: Forgive) -> Result<Expr, BadPattern> {
        if input.is_empty() {
            return Err(BadPattern::Empty);
        }
        let tokens = split_clauses(input);
        let mut must_clauses: Vec<Vec<Term>> = Vec::new();
        let mut forbid_terms: Vec<Term> = Vec::new();

        for tok in tokens {
            for piece in split_inline_bangs(&tok) {
                let (neg, body) = match piece.strip_prefix('!') {
                    Some(rest) if !piece.starts_with('"') => (true, rest),
                    _ => (false, piece.as_str()),
                };
                // A bare `~` is a similar word not written yet: nothing.
                let alts: Vec<Term> = split_alts(body)
                    .into_iter()
                    .filter(|(text, quoted)| *quoted || text != "~")
                    .map(|(text, quoted)| term(&text, quoted, forgive))
                    .collect();
                if alts.is_empty() {
                    continue;
                }
                if neg {
                    forbid_terms.extend(alts);
                } else {
                    must_clauses.push(alts);
                }
            }
        }
        Self::from_terms(&must_clauses, &forbid_terms, letters)
    }

    /// Evaluate the expression across `files`, reading surviving candidates only.
    pub fn scan(
        &self,
        options: &Options,
        ground: &Ground,
        enter: Option<Enter>,
        texts: Option<Arc<dyn Texts>>,
        files: &[File],
        budget: &Budget,
    ) -> Vec<Outcome> {
        let mut outcomes = self.outcomes(options, ground, enter, texts, files, budget);
        self.mark_every(&mut outcomes);
        outcomes
    }

    /// Every word the file must hold, marked everywhere in each line shown:
    /// each word was looked for by a pass of its own, and a line holding two
    /// of them showed only the one its pass found.
    fn mark_every(&self, outcomes: &mut [Outcome]) {
        for outcome in outcomes {
            let Outcome::Found { hits, .. } = outcome else {
                continue;
            };
            for hit in hits.iter_mut() {
                if hit.text.is_empty() {
                    continue;
                }
                let mut marks: Vec<(usize, usize)> = self
                    .must
                    .iter()
                    .flat_map(|p| p.finds(crate::excerpt::Kind::Text, hit.text.as_bytes()))
                    .collect();
                marks.push((hit.at.0 as usize, (hit.at.0 + hit.at.1) as usize));
                marks.sort_unstable();
                let mut joined: Vec<(u32, u32)> = Vec::with_capacity(marks.len());
                let mut last: Option<(usize, usize)> = None;
                for (a, b) in marks {
                    match &mut last {
                        Some(l) if a <= l.1 => l.1 = l.1.max(b),
                        _ => {
                            if let Some((x, y)) = last {
                                joined.push((x as u32, (y - x) as u32));
                            }
                            last = Some((a, b));
                        }
                    }
                }
                if let Some((x, y)) = last {
                    joined.push((x as u32, (y - x) as u32));
                }
                hit.marks = joined;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn outcomes(
        &self,
        options: &Options,
        ground: &Ground,
        enter: Option<Enter>,
        texts: Option<Arc<dyn Texts>>,
        files: &[File],
        budget: &Budget,
    ) -> Vec<Outcome> {
        if self.must.len() == 1 && self.forbid.is_none() {
            return Scanner::new(self.must[0].clone(), options.clone(), ground.clone())
                .entering(enter)
                .reading_documents(texts.clone())
                .scan(files, budget);
        }

        if self.must.is_empty() {
            let Some(forbid) = &self.forbid else {
                return vec![Outcome::Clean; files.len()];
            };
            let probe = Options {
                hits_per_file: 0,
                ..options.clone()
            };
            return Scanner::new(forbid.clone(), probe, ground.clone())
                .entering(enter)
                .reading_documents(texts.clone())
                .scan(files, budget)
                .into_iter()
                .map(|o| match o {
                    Outcome::Clean => Outcome::Found {
                        hits: Vec::new(),
                        more: false,
                    },
                    Outcome::Found { .. } => Outcome::Clean,
                    other => other,
                })
                .collect();
        }

        let mut outcomes = Scanner::new(self.must[0].clone(), options.clone(), ground.clone())
            .entering(enter.clone())
            .reading_documents(texts.clone())
            .scan(files, budget);

        for pat in &self.must[1..] {
            let survivors: Vec<usize> = outcomes
                .iter()
                .enumerate()
                .filter_map(|(i, o)| matches!(o, Outcome::Found { .. }).then_some(i))
                .collect();
            if survivors.is_empty() {
                break;
            }
            let sub: Vec<File> = survivors.iter().map(|&i| files[i].clone()).collect();
            let next_out = Scanner::new(pat.clone(), options.clone(), ground.clone())
                .entering(enter.clone())
                .reading_documents(texts.clone())
                .scan(&sub, budget);
            for (idx, sub_res) in survivors.into_iter().zip(next_out) {
                match sub_res {
                    Outcome::Found {
                        hits: more_hits,
                        more: more_flag,
                    } => {
                        if let Outcome::Found { hits, more } = &mut outcomes[idx] {
                            hits.extend(more_hits);
                            hits.sort_by_key(|h| (h.line, h.offset));
                            hits.dedup_by(|a, b| a.line.is_some() && a.line == b.line);
                            if options.hits_per_file > 0 && hits.len() > options.hits_per_file {
                                hits.truncate(options.hits_per_file);
                                *more = true;
                            } else {
                                *more |= more_flag;
                            }
                        }
                    }
                    Outcome::NotReached => outcomes[idx] = Outcome::NotReached,
                    _ => outcomes[idx] = Outcome::Clean,
                }
            }
        }

        if let Some(forbid) = &self.forbid {
            let survivors: Vec<usize> = outcomes
                .iter()
                .enumerate()
                .filter_map(|(i, o)| matches!(o, Outcome::Found { .. }).then_some(i))
                .collect();
            if !survivors.is_empty() {
                let sub: Vec<File> = survivors.iter().map(|&i| files[i].clone()).collect();
                let probe = Options {
                    hits_per_file: 0,
                    ..options.clone()
                };
                let rej = Scanner::new(forbid.clone(), probe, ground.clone())
                    .entering(enter)
                    .reading_documents(texts.clone())
                    .scan(&sub, budget);
                for (idx, sub_res) in survivors.into_iter().zip(rej) {
                    match sub_res {
                        Outcome::Found { .. } => outcomes[idx] = Outcome::Clean,
                        Outcome::NotReached => outcomes[idx] = Outcome::NotReached,
                        _ => {}
                    }
                }
            }
        }
        outcomes
    }
}

/// Tokenise on unquoted whitespace, keeping `(a | b)` and `a ; b` / `a | b` glued.
fn split_clauses(input: &str) -> Vec<String> {
    let mut raw: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut paren_depth = 0usize;
    for c in input.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                cur.push('"');
            }
            '(' if !in_quotes && cur.is_empty() => {
                paren_depth += 1;
            }
            ')' if !in_quotes && paren_depth > 0 => {
                paren_depth -= 1;
            }
            c if c.is_whitespace() && !in_quotes && paren_depth == 0 => {
                if !cur.is_empty() {
                    raw.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        raw.push(cur);
    }

    let mut out: Vec<String> = Vec::with_capacity(raw.len());
    let mut pending_bang = false;
    let mut glue_or = false;
    for mut t in raw {
        let quoted = t.starts_with('"');
        if !quoted && (t == "|" || t == ";") {
            glue_or = true;
            continue;
        }
        if !quoted && t == "!" {
            pending_bang = true;
            continue;
        }
        if !quoted && (t.starts_with('|') || t.starts_with(';')) && !out.is_empty() {
            glue_or = true;
            t.remove(0);
        }
        let trail = !quoted && t.len() > 1 && (t.ends_with('|') || t.ends_with(';'));
        if trail {
            t.pop();
        }
        if t.is_empty() {
            glue_or |= trail;
            continue;
        }
        if std::mem::take(&mut pending_bang) && !t.starts_with('!') {
            t.insert(0, '!');
        }
        if std::mem::take(&mut glue_or)
            && let Some(prev) = out.last_mut()
        {
            prev.push(';');
            prev.push_str(&t);
        } else {
            out.push(t);
        }
        glue_or = trail;
    }
    out
}

/// Split `a!b` into `["a", "!b"]` outside quotes while keeping `hello!` whole.
fn split_inline_bangs(tok: &str) -> Vec<String> {
    if tok.starts_with('"') {
        return vec![tok.to_owned()];
    }
    let mut out = Vec::new();
    let mut from = 0usize;
    let mut in_quotes = false;
    for (i, c) in tok.char_indices() {
        if c == '"' {
            in_quotes = !in_quotes;
            continue;
        }
        if !in_quotes && c == '!' && i > 0 && i + 1 < tok.len() {
            let prev = tok[..i].chars().next_back();
            if prev != Some(';') && prev != Some('|') {
                out.push(tok[from..i].to_owned());
                from = i;
            }
        }
    }
    out.push(tok[from..].to_owned());
    out
}

/// Split OR alternatives on unquoted `;` or `|`, unquoting each piece and
/// trimming the blanks outside quotes at its ends; with each, whether its
/// first character was quoted.
fn split_alts(body: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut cur = String::new();
    // How much of `cur` stays when the piece ends: up to its last character
    // that is neither an unquoted blank nor nothing.
    let mut keep = 0usize;
    let mut in_quotes = false;
    let mut quoted = None;
    for c in body.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                keep = cur.len();
            }
            ';' | '|' if !in_quotes => {
                cur.truncate(keep);
                if !cur.is_empty() || body.contains('"') {
                    let v = std::mem::take(&mut cur);
                    if !v.is_empty() {
                        out.push((v, quoted.unwrap_or(false)));
                    }
                } else {
                    cur.clear();
                }
                (keep, quoted) = (0, None);
            }
            c if c.is_whitespace() && !in_quotes => {
                if !cur.is_empty() {
                    cur.push(c);
                }
            }
            c => {
                quoted.get_or_insert(in_quotes);
                cur.push(c);
                keep = cur.len();
            }
        }
    }
    cur.truncate(keep);
    if !cur.is_empty() {
        out.push((cur, quoted.unwrap_or(false)));
    }
    out
}

/// One alternative as typed: `~word` is forgiven what `forgive` says a word
/// that long is, `\~` is a tilde, and a quoted tilde is only a tilde.
fn term(text: &str, quoted: bool, forgive: Forgive) -> Term {
    if quoted {
        return Term::Plain(text.to_owned());
    }
    if let Some(tilde) = text.strip_prefix('\\').filter(|t| t.starts_with('~')) {
        return Term::Plain(tilde.to_owned());
    }
    match text.strip_prefix('~').filter(|w| !w.is_empty()) {
        Some(word) => match forgive(word) {
            0 => Term::Plain(word.to_owned()),
            edits => Term::Near(word.to_owned(), edits),
        },
        None => Term::Plain(text.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::Skip;
    use surefoot::Medium;

    #[test]
    fn parses_or_and_not_and_phrases() {
        let dir = tempfile::tempdir().unwrap();
        let put = |name: &str, txt: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, txt.as_bytes()).unwrap();
            File {
                path,
                size: Some(txt.len() as u64),
                class: None,
            }
        };
        let files = [
            put("a.txt", "Ankara Hukuk Bürosu\nİş kazası tazminatı\n"),
            put(
                "b.txt",
                "İstanbul Ticaret\nİş kazası ve feragat protokolü\n",
            ),
            put("c.txt", "İstanbul Asliye\nİş kazası dilekçesi\n"),
            put("d.txt", "İzmir İcra\nKira sözleşmesi\n"),
        ];
        let ground = Ground::of([(dir.path().to_path_buf(), None, Medium::Solid)]);
        let expr = Expr::new(
            &Needle::Text("ankara;istanbul \"iş kazası\" !feragat".into()),
            false,
        )
        .unwrap();
        let got = expr.scan(
            &Options::default(),
            &ground,
            None,
            None,
            &files,
            &Budget::unlimited(),
        );

        // `a.txt`: has Ankara + "iş kazası", no "feragat" -> Found!
        assert!(matches!(got[0], Outcome::Found { .. }));
        // `b.txt`: has İstanbul + "iş kazası", BUT contains "feragat" -> excluded!
        assert_eq!(got[1], Outcome::Clean);
        // `c.txt`: has İstanbul + "iş kazası", no "feragat" -> Found!
        assert!(matches!(got[2], Outcome::Found { .. }));
        // `d.txt`: neither Ankara nor İstanbul -> Clean!
        assert_eq!(got[3], Outcome::Clean);

        // Negation-only `!feragat` returns every text file without `feragat`.
        let only_not = Expr::new(&Needle::Text("!feragat".into()), false).unwrap();
        let neg_got = only_not.scan(
            &Options::default(),
            &ground,
            None,
            None,
            &files,
            &Budget::unlimited(),
        );
        assert!(matches!(neg_got[0], Outcome::Found { .. }));
        assert_eq!(neg_got[1], Outcome::Clean);
        assert!(matches!(neg_got[2], Outcome::Found { .. }));
        assert!(matches!(neg_got[3], Outcome::Found { .. }));
        let _ = Skip::Binary;
    }
}
