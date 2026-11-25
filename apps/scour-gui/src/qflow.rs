//! Where the query wraps when its characters are not one width: the rule of
//! `scour_ui::wrap` — break after a space, a word only when longer than a
//! line, the spaces of a break hanging at its end — measured in pixels, with
//! stretches that never break (a capsule drawn as one piece).

use scour_ui::wrap::Line;

/// The lines `chars` take in `room` pixels, `w[i]` the width of `chars[i]`.
/// Each `keep` range is one unbreakable unit, as wide as its characters.
pub fn lines(chars: &[char], w: &[f32], room: f32, keep: &[(usize, usize)]) -> Vec<Line> {
    let n = chars.len();
    let mut out = Vec::new();
    let mut start = 0;
    while start < n {
        let (mut end, mut acc) = (start, 0.0);
        // The last place a line may end: after a space, before a non-space.
        let mut fit = None;
        while end < n {
            let (next, unit) = match keep.iter().find(|k| k.0 == end && k.1 > end) {
                Some(&(_, e)) => (e.min(n), w[end..e.min(n)].iter().sum::<f32>()),
                None => (end + 1, w.get(end).copied().unwrap_or(0.0)),
            };
            // A line holds at least one unit, however wide.
            if end > start && acc + unit > room + 0.01 {
                break;
            }
            acc += unit;
            end = next;
            if chars[end - 1].is_whitespace() && end < n && !chars[end].is_whitespace() {
                fit = Some(end);
            }
        }
        if end < n {
            if chars[end].is_whitespace() {
                while end < n && chars[end].is_whitespace() {
                    end += 1;
                }
            } else if let Some(f) = fit {
                end = f;
            }
        }
        out.push(Line { start, end });
        start = end;
    }
    if out.is_empty() {
        out.push(Line { start: 0, end: 0 });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use scour_ui::wrap;

    fn cut(text: &str, w: &[f32], room: f32, keep: &[(usize, usize)]) -> Vec<String> {
        let c: Vec<char> = text.chars().collect();
        lines(&c, w, room, keep)
            .iter()
            .map(|l| c[l.start..l.end].iter().collect())
            .collect()
    }

    #[test]
    fn one_width_wraps_where_the_column_rule_does() {
        for (text, cols) in [
            ("rapor ext:pdf dm:7d", 12),
            ("abcde   fgh", 5),
            ("under:/home/u/Belgeler/Hukuk", 10),
            ("ab averyverylongword", 6),
            ("", 4),
        ] {
            let c: Vec<char> = text.chars().collect();
            let w = vec![10.0; c.len()];
            assert_eq!(
                lines(&c, &w, cols as f32 * 10.0, &[]),
                wrap::lines(text, cols),
                "{text}"
            );
        }
    }

    #[test]
    fn a_wide_capsule_wraps_sooner_and_a_kept_one_whole() {
        let text = "rapor under:/a|under:/b x";
        let mut w = vec![10.0; text.len()];
        // The capsule is drawn 150 px wide, all of it on its first letter.
        w[6..23].fill(0.0);
        w[6] = 150.0;
        assert_eq!(
            cut(text, &w, 200.0, &[(6, 23)]),
            ["rapor ", "under:/a|under:/b x"]
        );
        // Spaces inside a kept stretch are no place to break.
        let text = "under:/a | under:/b x";
        let w = vec![10.0; text.len()];
        assert_eq!(
            cut(text, &w, 120.0, &[(0, 19)]),
            ["under:/a | under:/b ", "x"]
        );
    }
}
