//! Member (symbol) hashing: the `tnt2` member hash scheme.
//!
//! ```text
//! symbol_key  = <symbol-kind> ":" <display-name> ":" <ordinal>
//! symbol_hash = "tnt2:member:<hex>"
//!             = BLAKE3("treenotes-hash-v2|member\0" || symbol_key || "\0" || normalised-body)
//! ```
//!
//! The body is the declaration's own source text with insignificant whitespace collapsed. It is
//! deliberately conservative: comments are **not** stripped, so editing a doc comment re-stales the
//! member (a false-stale is safer than a false-fresh), and string bodies keep their whitespace, so
//! `"a   b"` and `"a b"` stay different declarations.

/// Version/domain tag that prefixes every member hash produced by this crate.
pub const MEMBER_SCHEME: &str = "tnt2";

const PREFIX_MEMBER: &[u8] = b"treenotes-hash-v2|member\0";

/// Hash one declaration: its symbol key plus its own normalised source text.
pub fn symbol_hash(symbol_key: &str, body: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(PREFIX_MEMBER);
    hasher.update(symbol_key.as_bytes());
    hasher.update(b"\0");
    hasher.update(normalise(body).as_bytes());
    format!("{MEMBER_SCHEME}:member:{}", hasher.finalize().to_hex())
}

/// Collapse insignificant whitespace: runs of space, tab, CR and LF outside string literals become
/// one space, and leading and trailing whitespace is dropped.
///
/// String literals are recognised for `"`, `'` and `` ` `` with backslash escapes; their contents
/// are copied verbatim. A mis-detected string only ever *keeps* whitespace (the conservative
/// direction), so this scanner can never claim two different string bodies are the same code.
pub fn normalise(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    let mut pending_space = false;
    let mut delimiter: Option<char> = None;
    while let Some(current) = chars.next() {
        match delimiter {
            Some(quote) => {
                out.push(current);
                if current == '\\' {
                    if let Some(escaped) = chars.next() {
                        out.push(escaped);
                    }
                } else if current == quote {
                    delimiter = None;
                }
            }
            None => {
                if current.is_whitespace() {
                    pending_space = true;
                    continue;
                }
                if pending_space {
                    if !out.is_empty() {
                        out.push(' ');
                    }
                    pending_space = false;
                }
                if current == '"' || current == '\'' || current == '`' {
                    delimiter = Some(current);
                }
                out.push(current);
            }
        }
    }
    out
}
