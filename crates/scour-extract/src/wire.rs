//! What a reader and a text-extracting process say to each other over a
//! pipe: a document's bytes one way, its text the other. Framed by length,
//! so a crashed peer is an end of stream, never a misread message.

use std::io::{self, Read, Write};

/// What became of one document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Words(String),
    /// It opened and holds no text.
    NoText,
    /// It would not open; the reason, for a log.
    Failed(String),
}

/// Larger frames are refused: a peer that says more is not one of ours.
const MOST: u64 = 512 << 20;

/// `format` names the reader; `cap` bounds the text sent back.
pub fn write_request(w: &mut impl Write, format: &str, cap: u64, bytes: &[u8]) -> io::Result<()> {
    frame(w, format.as_bytes())?;
    w.write_all(&cap.to_le_bytes())?;
    frame(w, bytes)?;
    w.flush()
}

/// None at the end of the stream: the other side is done.
pub fn read_request(r: &mut impl Read) -> io::Result<Option<(String, u64, Vec<u8>)>> {
    let Some(format) = unframe(r)? else {
        return Ok(None);
    };
    let cap = u64::from_le_bytes(exactly(r)?);
    let bytes = unframe(r)?.ok_or_else(cut_short)?;
    Ok(Some((
        String::from_utf8_lossy(&format).into_owned(),
        cap,
        bytes,
    )))
}

pub fn write_answer(w: &mut impl Write, answer: &Answer) -> io::Result<()> {
    let (tag, body): (u8, &[u8]) = match answer {
        Answer::Words(text) => (0, text.as_bytes()),
        Answer::NoText => (1, b""),
        Answer::Failed(why) => (2, why.as_bytes()),
    };
    w.write_all(&[tag])?;
    frame(w, body)?;
    w.flush()
}

pub fn read_answer(r: &mut impl Read) -> io::Result<Answer> {
    let [tag] = exactly::<1>(r)?;
    let body = unframe(r)?.ok_or_else(cut_short)?;
    let text = String::from_utf8_lossy(&body).into_owned();
    Ok(match tag {
        0 => Answer::Words(text),
        1 => Answer::NoText,
        _ => Answer::Failed(text),
    })
}

fn frame(w: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    w.write_all(&(bytes.len() as u64).to_le_bytes())?;
    w.write_all(bytes)
}

fn unframe(r: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 8];
    match r.read_exact(&mut len) {
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        other => other?,
    }
    let len = u64::from_le_bytes(len);
    if len > MOST {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body)?;
    Ok(Some(body))
}

fn exactly<const N: usize>(r: &mut impl Read) -> io::Result<[u8; N]> {
    let mut buf = [0u8; N];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

fn cut_short() -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, "message cut short")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_and_answers_survive_the_pipe() {
        let mut pipe = Vec::new();
        write_request(&mut pipe, "pdf", 99, b"%PDF").unwrap();
        let mut r = pipe.as_slice();
        assert_eq!(
            read_request(&mut r).unwrap(),
            Some(("pdf".into(), 99, b"%PDF".to_vec()))
        );
        assert_eq!(read_request(&mut r).unwrap(), None);
        for answer in [
            Answer::Words("şey".into()),
            Answer::NoText,
            Answer::Failed("why".into()),
        ] {
            let mut pipe = Vec::new();
            write_answer(&mut pipe, &answer).unwrap();
            assert_eq!(read_answer(&mut pipe.as_slice()).unwrap(), answer);
        }
    }

    #[test]
    fn a_peer_that_dies_mid_message_is_an_error() {
        let mut pipe = Vec::new();
        write_answer(&mut pipe, &Answer::Words("long text".into())).unwrap();
        pipe.truncate(pipe.len() - 3);
        assert!(read_answer(&mut pipe.as_slice()).is_err());
        let huge = (MOST + 1).to_le_bytes();
        assert!(read_request(&mut huge.as_slice()).is_err());
    }
}
