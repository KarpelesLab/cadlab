//! S-expression reader and writer for the Specctra design language.
//!
//! Specctra files are lists of atoms and parenthesized lists. Two details differ from other
//! S-expression dialects (Specctra Design Language Reference, "parser" descriptor):
//! - the string quote character is configurable with `(string_quote <char>)`, where the
//!   character itself follows unquoted (`(string_quote ")`); files that do not declare one
//!   (session files usually do not) use `"`, as every writer we know of does;
//! - quoted strings have no escape sequences: a string runs to the next quote character.

use std::fmt::Write as _;

/// A node: an atom (with whether it was quoted) or a list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sx {
    /// A token or a quoted string.
    Atom {
        /// Text without quotes.
        text: String,
        /// Whether it was quoted in the file.
        quoted: bool,
    },
    /// A parenthesized list.
    List(Vec<Sx>),
}

/// A syntax error with its 1-based line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntaxError {
    /// Line of the problem.
    pub line: usize,
    /// What is wrong.
    pub message: String,
}

impl Sx {
    /// An unquoted atom.
    pub fn atom(s: impl Into<String>) -> Sx {
        Sx::Atom { text: s.into(), quoted: false }
    }

    /// A string atom, quoted on output when it needs to be.
    pub fn string(s: impl Into<String>) -> Sx {
        Sx::Atom { text: s.into(), quoted: true }
    }

    /// A list starting with the keyword `head`.
    pub fn list(head: &str, items: impl IntoIterator<Item = Sx>) -> Sx {
        let mut v = vec![Sx::atom(head)];
        v.extend(items);
        Sx::List(v)
    }

    /// Text of an atom.
    pub fn text(&self) -> Option<&str> {
        match self {
            Sx::Atom { text, .. } => Some(text),
            Sx::List(_) => None,
        }
    }

    /// Items of a list (empty for an atom).
    pub fn items(&self) -> &[Sx] {
        match self {
            Sx::List(v) => v,
            Sx::Atom { .. } => &[],
        }
    }

    /// Keyword of a list: its first item when that is an atom.
    pub fn head(&self) -> Option<&str> {
        self.items().first().and_then(Sx::text)
    }

    /// Items after the keyword.
    pub fn args(&self) -> &[Sx] {
        self.items().get(1..).unwrap_or(&[])
    }

    /// Child lists with keyword `head`.
    pub fn children<'a>(&'a self, head: &'a str) -> impl Iterator<Item = &'a Sx> + 'a {
        self.args().iter().filter(move |c| c.head() == Some(head))
    }

    /// First child list with keyword `head`.
    pub fn child(&self, head: &str) -> Option<&Sx> {
        self.args().iter().find(|c| c.head() == Some(head))
    }
}

/// Parses one top-level list (leading and trailing white space allowed).
pub fn parse(text: &str) -> Result<Sx, SyntaxError> {
    let mut r = Reader { s: text.as_bytes(), pos: 0, line: 1, quote: Some(b'"') };
    r.skip_ws();
    if r.peek() != Some(b'(') {
        return Err(r.error("expected `(` at the start of the file"));
    }
    let node = r.list()?;
    r.skip_ws();
    if r.pos < r.s.len() {
        return Err(r.error("unexpected text after the closing parenthesis"));
    }
    Ok(node)
}

struct Reader<'a> {
    s: &'a [u8],
    pos: usize,
    line: usize,
    quote: Option<u8>,
}

impl Reader<'_> {
    fn error(&self, m: &str) -> SyntaxError {
        SyntaxError { line: self.line, message: m.to_string() }
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let c = self.peek()?;
        self.pos += 1;
        if c == b'\n' {
            self.line += 1;
        }
        Some(c)
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(|c| c.is_ascii_whitespace()) {
            self.bump();
        }
    }

    fn list(&mut self) -> Result<Sx, SyntaxError> {
        let start = self.line;
        self.bump(); // '('
        let mut items = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                None => {
                    return Err(SyntaxError { line: start, message: "list opened here is never closed".into() });
                }
                Some(b')') => {
                    self.bump();
                    return Ok(Sx::List(items));
                }
                Some(b'(') => items.push(self.list()?),
                Some(_) => {
                    // `(string_quote X)`: X is the new quote character, read raw.
                    if items.len() == 1 && items[0].text() == Some("string_quote") {
                        let c = self.bump().expect("peeked");
                        self.quote = Some(c);
                        items.push(Sx::atom((c as char).to_string()));
                        continue;
                    }
                    items.push(self.atom()?);
                }
            }
        }
    }

    fn atom(&mut self) -> Result<Sx, SyntaxError> {
        let c = self.peek().expect("caller checked");
        if Some(c) == self.quote {
            let line = self.line;
            self.bump();
            let start = self.pos;
            while let Some(d) = self.peek() {
                if d == c {
                    let text = String::from_utf8_lossy(&self.s[start..self.pos]).into_owned();
                    self.bump();
                    return Ok(Sx::Atom { text, quoted: true });
                }
                self.bump();
            }
            return Err(SyntaxError { line, message: "string opened here is never closed".into() });
        }
        let start = self.pos;
        while let Some(d) = self.peek() {
            if d.is_ascii_whitespace() || d == b'(' || d == b')' {
                break;
            }
            self.bump();
        }
        Ok(Sx::atom(String::from_utf8_lossy(&self.s[start..self.pos]).into_owned()))
    }
}

/// Whether a string atom must be quoted: empty, or containing white space, parentheses or the
/// quote character.
fn needs_quotes(s: &str) -> bool {
    s.is_empty() || s.chars().any(|c| c.is_whitespace() || c == '(' || c == ')' || c == '"')
}

/// Writes a node with `"` as the string quote. Lists of atoms, and lists without grandchildren
/// that fit in 100 characters, stay on one line; other lists put each child list on its own
/// line, indented by two spaces per level. A `"` inside a string (which Specctra cannot
/// escape) is written as `'`.
pub fn write(node: &Sx) -> String {
    let mut out = String::new();
    write_node(node, 0, &mut out);
    out.push('\n');
    out
}

fn write_atom(text: &str, quoted: bool, out: &mut String) {
    if quoted && needs_quotes(text) {
        // There is no escape: a quote inside a string cannot be written. Callers sanitize names.
        let _ = write!(out, "\"{}\"", text.replace('"', "'"));
    } else {
        out.push_str(text);
    }
}

fn write_node(node: &Sx, depth: usize, out: &mut String) {
    match node {
        Sx::Atom { text, quoted } => write_atom(text, *quoted, out),
        Sx::List(items) => {
            // Lists without grandchildren stay on one line when short.
            let shallow = items.iter().all(|i| i.items().iter().all(|g| matches!(g, Sx::Atom { .. })));
            if shallow {
                let mut line = String::new();
                write_flat(node, &mut line);
                let flat = items.iter().all(|i| matches!(i, Sx::Atom { .. }));
                if flat || line.len() <= 100 {
                    out.push_str(&line);
                    return;
                }
            }
            out.push('(');
            let mut first = true;
            for item in items {
                match item {
                    Sx::Atom { text, quoted } => {
                        if !first {
                            out.push(' ');
                        }
                        write_atom(text, *quoted, out);
                    }
                    Sx::List(_) => {
                        out.push('\n');
                        out.push_str(&"  ".repeat(depth + 1));
                        write_node(item, depth + 1, out);
                    }
                }
                first = false;
            }
            out.push('\n');
            out.push_str(&"  ".repeat(depth));
            out.push(')');
        }
    }
}

/// One-line rendering.
fn write_flat(node: &Sx, out: &mut String) {
    match node {
        Sx::Atom { text, quoted } => write_atom(text, *quoted, out),
        Sx::List(items) => {
            out.push('(');
            // `(string_quote ")` writes the quote character raw.
            let raw = items.first().and_then(Sx::text) == Some("string_quote");
            for (k, item) in items.iter().enumerate() {
                if k > 0 {
                    out.push(' ');
                }
                match item {
                    Sx::Atom { text, .. } if raw && k == 1 => out.push_str(text),
                    _ => write_flat(item, out),
                }
            }
            out.push(')');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_quote_and_round_trip() {
        let text = "(pcb \"my board\"\n  (parser\n    (string_quote \")\n    (space_in_quoted_tokens on))\n  (net \"A (x)\" (pins U1-1 \"J 1-2\")))";
        let n = parse(text).unwrap();
        assert_eq!(n.head(), Some("pcb"));
        assert_eq!(n.args()[0].text(), Some("my board"));
        let net = n.child("net").unwrap();
        assert_eq!(net.args()[0].text(), Some("A (x)"));
        assert_eq!(net.child("pins").unwrap().args()[1].text(), Some("J 1-2"));
        let again = parse(&write(&n)).unwrap();
        assert_eq!(again, n);
    }

    #[test]
    fn quote_defaults_and_changes() {
        let n = parse("(a \"b c\")").unwrap();
        assert_eq!(n.args()[0].text(), Some("b c"));
        // After `(string_quote ')`, `"` is an ordinary character.
        let n = parse("(a (string_quote ') 'b c' \"d)").unwrap();
        assert_eq!(n.args()[1].text(), Some("b c"));
        assert_eq!(n.args()[2].text(), Some("\"d"));
    }

    #[test]
    fn errors_have_lines() {
        let e = parse("(a\n(b\n").unwrap_err();
        assert_eq!(e.line, 2);
        assert!(parse("x").is_err());
        assert!(parse("(a) b").is_err());
    }
}
