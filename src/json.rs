//! Small JSON encoder. All external strings pass through `quote`.
use std::fmt::Write;

pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c <= '\u{1f}' => { let _ = write!(out, "\\u{:04x}", c as u32); }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub fn array(items: impl IntoIterator<Item = String>) -> String {
    format!("[{}]", items.into_iter().collect::<Vec<_>>().join(","))
}

pub fn object(items: impl IntoIterator<Item = (String, String)>) -> String {
    format!("{{{}}}", items.into_iter().map(|(k,v)| format!("{}:{}", quote(&k), v)).collect::<Vec<_>>().join(","))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn escape_external_strings() {
        assert_eq!(quote("a\"b\\c\n\t\u{0}🌐"), "\"a\\\"b\\\\c\\n\\t\\u0000🌐\"");
    }
}
