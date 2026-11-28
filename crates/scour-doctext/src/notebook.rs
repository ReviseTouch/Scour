//! A Jupyter notebook (`.ipynb`) as a flow: each Markdown cell read as
//! Markdown, each code cell as code under its `In [n]`, and what it printed —
//! text, a table or a picture — under it, as the notebook was saved.

use scour_flow::Flow;
use serde_json::Value;

use crate::build::Builder;

/// The notebook in `bytes`.
pub fn flow(bytes: &[u8]) -> Result<Flow, String> {
    let nb: Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let cells = nb
        .get("cells")
        .and_then(Value::as_array)
        .ok_or("no cells")?;
    let mut b = Builder::default();
    for cell in cells {
        if b.full() {
            break;
        }
        let source = text(cell.get("source"));
        match cell.get("cell_type").and_then(Value::as_str) {
            Some("markdown") => crate::markdown::read(&source, &mut b),
            Some("code") => {
                let n = cell
                    .get("execution_count")
                    .and_then(Value::as_u64)
                    .map_or_else(|| "In [ ]".to_owned(), |n| format!("In [{n}]"));
                b.caption(&n);
                b.code_block(&source);
                for out in cell
                    .get("outputs")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    output(&mut b, out);
                }
            }
            // A raw cell is what it says, as it is.
            _ if !source.trim().is_empty() => b.code_block(&source),
            _ => {}
        }
    }
    Ok(b.finish())
}

/// What a code cell printed: a picture where there is one, else a page, else
/// its text.
fn output(b: &mut Builder, out: &Value) {
    match out.get("output_type").and_then(Value::as_str) {
        Some("stream") => quiet_code(b, &text(out.get("text"))),
        Some("error") => {
            let trace: Vec<String> = out
                .get("traceback")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(without_colour)
                .collect();
            quiet_code(b, &trace.join("\n"));
        }
        Some("execute_result" | "display_data") => {
            let Some(data) = out.get("data") else { return };
            for mime in ["image/png", "image/jpeg", "image/gif"] {
                if let Some(bytes) = data
                    .get(mime)
                    .map(|v| text(Some(v)))
                    .and_then(|t| scour_flow::decode(&t.replace(['\n', '\r'], "")))
                    && b.picture(&bytes, None)
                {
                    return;
                }
            }
            if let Some(html) = data.get("text/html") {
                crate::html::read(&text(Some(html)), b, &mut |_| None);
                b.flush();
                return;
            }
            if let Some(plain) = data.get("text/plain") {
                quiet_code(b, &text(Some(plain)));
            }
        }
        _ => {}
    }
}

/// Printed output, set apart from the code it came from.
fn quiet_code(b: &mut Builder, text: &str) {
    if text.trim().is_empty() {
        return;
    }
    b.look.quiet += 1;
    b.code_block(text);
    b.look.quiet -= 1;
}

/// A source or an output: a string, or the list of lines Jupyter writes.
fn text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(lines)) => lines.iter().filter_map(Value::as_str).collect(),
        _ => String::new(),
    }
}

/// A traceback's terminal colours taken out: `ESC[0;31m` and the like.
fn without_colour(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use scour_flow::Block;

    #[test]
    fn cells_in_order_with_what_they_printed() {
        let nb = r##"{"cells":[
            {"cell_type":"markdown","source":["# Başlık\n","bir **kalın**"]},
            {"cell_type":"code","execution_count":3,"source":"print(1)",
             "outputs":[{"output_type":"stream","text":["1\n"]},
                        {"output_type":"error","traceback":["\u001b[0;31mHata\u001b[0m"]}]},
            {"cell_type":"code","execution_count":null,"source":"x",
             "outputs":[{"output_type":"execute_result","data":{"text/plain":["42"]}}]}
        ],"nbformat":4}"##;
        let flow = flow(nb.as_bytes()).unwrap();
        let words: Vec<String> = flow
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Para(p) => Some(p.text()),
                _ => None,
            })
            .collect();
        assert_eq!(
            words,
            [
                "Başlık",
                "bir kalın",
                "In [3]",
                "print(1)",
                "1",
                "Hata",
                "In [ ]",
                "x",
                "42"
            ]
        );
    }
}
