//! Dollar islands are protected before the existing Markdown parser sees their
//! underscores/backslashes. This is delimiter admission, not a TeX parser.
//! Fenced and inline code remain the Markdown parser's literal content.
#[derive(Debug)]
pub(super) struct Island { pub source: String, pub display: bool }

pub(super) struct Protected {
    pub source: String,
    pub prefix: String,
    pub islands: Vec<Island>,
}

pub(super) fn protect(source: &str) -> Result<Protected, super::MathDocumentError> {
    let mut prefix = String::from("FMNMDMATH");
    while source.contains(&prefix) { prefix.push('X'); }
    let bytes = source.as_bytes();
    let mut out = String::new();
    let mut islands = Vec::new();
    let (mut i, mut copied) = (0, 0);
    while i < bytes.len() {
        if bytes[i] == b'\\' { i = (i + 2).min(bytes.len()); continue; }
        // Exact-length backtick spans, plus tilde fences. The outer block
        // parser already excludes top-level code; this handles nested code.
        if bytes[i] == b'`' || (bytes[i] == b'~' && bytes.get(i..i+3) == Some(b"~~~")) {
            let start = i;
            let delimiter = bytes[i];
            while bytes.get(i) == Some(&delimiter) { i += 1; }
            let n = i - start;
            let mut closed = false;
            while i < bytes.len() {
                if bytes[i] == delimiter {
                    let end = i;
                    while bytes.get(i) == Some(&delimiter) { i += 1; }
                    if i - end == n { closed = true; break; }
                } else { i += 1; }
            }
            // An unmatched code delimiter is literal Markdown, not a code
            // span swallowing every subsequent mathematical island.
            if !closed { i = start + n; }
            continue;
        }
        if bytes[i] != b'$' { i += 1; continue; }
        let n = if bytes.get(i + 1) == Some(&b'$') { 2 } else { 1 };
        let start = i;
        let mut end = i + n;
        let mut found = None;
        while end < bytes.len() {
            if bytes[end] == b'\\' { end = (end + 2).min(bytes.len()); continue; }
            if n == 1 && bytes[end] == b'\n' { break; }
            if bytes[end] == b'$' {
                let count = if bytes.get(end + 1) == Some(&b'$') { 2 } else { 1 };
                if count == n { found = Some(end); break; }
                end += count;
            } else { end += 1; }
        }
        let Some(end) = found else { i += n; continue; };
        if islands.len() >= 256 || end - start > 8192 {
            return Err(super::MathDocumentError::Limit("Markdown math islands exceed 256 formulas or 8192 bytes per formula"));
        }
        out.push_str(&source[copied..start]);
        out.push_str(&format!("{prefix}{}Q", islands.len()));
        islands.push(Island { source: source[start+n..end].to_owned(), display: n == 2 });
        i = end + n;
        copied = i;
    }
    out.push_str(&source[copied..]);
    Ok(Protected { source: out, prefix, islands })
}
