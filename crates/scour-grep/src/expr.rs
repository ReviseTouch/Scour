//! Boolean content expressions (`a;b !c`, `a | b !c`, `"exact phrase" !skip`)
//! evaluated in minimal passes over candidate files.

use crate::budget::Budget;
use crate::ground::Ground;
use crate::pattern::{BadPattern, Needle, Pattern};
use crate::scan::{Enter, File, Options, Outcome, Scanner};

/// Conjunctive content plan: every `must` pattern (each an OR of `;`/`|` terms)
/// must match the file, and `forbid` (an OR of all `!` terms) must not.
#[derive(Debug, Clone)]
pub struct Expr {
    pub must: Vec<Pattern>,
    pub forbid: Option<Pattern>,
}

impl Expr {
    /// Parse `needle`. `Needle::Text` supports `;` / `|` (OR), whitespace (AND),
    /// `!term` (exclude file), and `"..."` (literal phrase).
    pub fn new(needle: &Needle, exact_case: bool) -> Result<Expr, BadPattern> {
        match needle {
            Needle::Regex(_) | Needle::Bytes(_) => Ok(Expr {
                must: vec![Pattern::new(needle, exact_case)?],
                forbid: None,
            }),
            Needle::Text(raw) => Self::parse_text(raw, exact_case),
        }
    }

    pub fn from_clauses(
        must_clauses: &[Vec<String>],
        forbid_terms: &[String],
        exact_case: bool,
    ) -> Result<Expr, BadPattern> {
        let mut must = Vec::with_capacity(must_clauses.len());
        for clause in must_clauses {
            must.push(Pattern::any_text(clause, exact_case)?);
        }
        let forbid = if forbid_terms.is_empty() {
            None
        } else {
            Some(Pattern::any_text(forbid_terms, exact_case)?)
        };
        if must.is_empty() && forbid.is_none() {
            return Err(BadPattern::Empty);
        }
        Ok(Expr { must, forbid })
    }

    fn parse_text(input: &str, exact_case: bool) -> Result<Expr, BadPattern> {
        if input.is_empty() {
            return Err(BadPattern::Empty);
        }
        let tokens = split_clauses(input);
        let mut must_clauses: Vec<Vec<String>> = Vec::new();
        let mut forbid_terms: Vec<String> = Vec::new();

        for tok in tokens {
            for piece in split_inline_bangs(&tok) {
                let (neg, body) = match piece.strip_prefix('!') {
                    Some(rest) if !piece.starts_with('"') => (true, rest),
                    _ => (false, piece.as_str()),
                };
                let alts = split_alts(body);
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
        Self::from_clauses(&must_clauses, &forbid_terms, exact_case)
    }

    /// Evaluate the expression across `files`, reading surviving candidates only.
    pub fn scan(
        &self,
        options: &Options,
        ground: &Ground,
        enter: Option<Enter>,
        files: &[File],
        budget: &Budget,
    ) -> Vec<Outcome> {
        if self.must.len() == 1 && self.forbid.is_none() {
            return Scanner::new(self.must[0].clone(), options.clone(), ground.clone())
                .entering(enter)
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

/// Split OR alternatives on unquoted `;` or `|`, unquoting each piece.
fn split_alts(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    for c in body.chars() {
        match c {
            '"' => in_quotes = !in_quotes,
            ';' | '|' if !in_quotes => {
                let s = cur.trim();
                if !s.is_empty() || body.contains('"') {
                    let v = std::mem::take(&mut cur);
                    if !v.is_empty() {
                        out.push(v);
                    }
                } else {
                    cur.clear();
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
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
