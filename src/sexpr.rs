//! Minimal S-expression reader for the KiCad file formats (netlists today, boards later).
//!
//! Written from the published description of KiCad's S-expression files: lists in parentheses,
//! bare atoms, and double-quoted strings with backslash escapes. Strings and atoms both become
//! [`Sexpr::Atom`]s; the reader does not interpret numbers. It never panics on bad input.

use std::fmt;

/// A parsed S-expression.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sexpr {
    /// A bare atom or a string (unquoted, escapes resolved).
    Atom(String),
    /// A list.
    List(Vec<Sexpr>),
}

/// A syntax error with its byte offset.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("line {line}: {message}")]
pub struct SexprError {
    /// 1-based line of the error.
    pub line: usize,
    /// What went wrong.
    pub message: String,
}

impl Sexpr {
    /// The atom text, if this is an atom.
    pub fn atom(&self) -> Option<&str> {
        match self {
            Sexpr::Atom(a) => Some(a),
            Sexpr::List(_) => None,
        }
    }

    /// The first atom of a list (`net` in `(net (code 1) ...)`).
    pub fn head(&self) -> Option<&str> {
        match self {
            Sexpr::List(v) => v.first().and_then(Sexpr::atom),
            Sexpr::Atom(_) => None,
        }
    }

    /// Items of a list (head included); empty for atoms.
    pub fn items(&self) -> &[Sexpr] {
        match self {
            Sexpr::List(v) => v,
            Sexpr::Atom(_) => &[],
        }
    }

    /// Child lists whose head is `head`, in order.
    pub fn all<'a>(&'a self, head: &'a str) -> impl Iterator<Item = &'a Sexpr> + 'a {
        self.items().iter().filter(move |c| c.head() == Some(head))
    }

    /// The first child list whose head is `head`.
    pub fn get(&self, head: &str) -> Option<&Sexpr> {
        self.items().iter().find(|c| c.head() == Some(head))
    }

    /// The first atom after the head: `"1"` in `(code "1")`.
    pub fn value(&self) -> Option<&str> {
        self.items().get(1).and_then(Sexpr::atom)
    }

    /// The value of the child list `head`: `get(head)?.value()`.
    pub fn child_value(&self, head: &str) -> Option<&str> {
        self.get(head).and_then(Sexpr::value)
    }
}

impl fmt::Display for Sexpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Sexpr::Atom(a) => {
                let bare =
                    !a.is_empty() && a.chars().all(|c| !c.is_whitespace() && !matches!(c, '(' | ')' | '"' | '\\'));
                if bare {
                    f.write_str(a)
                } else {
                    write!(f, "\"{}\"", a.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n"))
                }
            }
            Sexpr::List(v) => {
                f.write_str("(")?;
                for (i, x) in v.iter().enumerate() {
                    if i > 0 {
                        f.write_str(" ")?;
                    }
                    x.fmt(f)?;
                }
                f.write_str(")")
            }
        }
    }
}

/// Parses one top-level expression (trailing whitespace allowed).
pub fn parse(text: &str) -> Result<Sexpr, SexprError> {
    let b = text.as_bytes();
    let mut i = 0;
    let mut line = 1;
    let err = |line: usize, m: &str| SexprError { line, message: m.to_string() };
    let mut stack: Vec<Vec<Sexpr>> = Vec::new();
    let mut done: Option<Sexpr> = None;
    while i < b.len() {
        let c = b[i];
        match c {
            b'\n' => {
                line += 1;
                i += 1;
            }
            c if c.is_ascii_whitespace() => i += 1,
            b'(' => {
                if done.is_some() {
                    return Err(err(line, "text after the top-level expression"));
                }
                stack.push(Vec::new());
                i += 1;
            }
            b')' => {
                let list = Sexpr::List(stack.pop().ok_or_else(|| err(line, "unbalanced `)`"))?);
                match stack.last_mut() {
                    Some(parent) => parent.push(list),
                    None => done = Some(list),
                }
                i += 1;
            }
            b'"' => {
                let start_line = line;
                let mut s: Vec<u8> = Vec::new();
                i += 1;
                loop {
                    let Some(&c) = b.get(i) else { return Err(err(start_line, "unterminated string")) };
                    match c {
                        b'"' => break,
                        b'\\' => {
                            i += 1;
                            let Some(&e) = b.get(i) else { return Err(err(start_line, "unterminated string")) };
                            match e {
                                b'n' => s.push(b'\n'),
                                b't' => s.push(b'\t'),
                                b'r' => s.push(b'\r'),
                                e => s.push(e),
                            }
                        }
                        b'\n' => {
                            line += 1;
                            s.push(c);
                        }
                        c => s.push(c),
                    }
                    i += 1;
                }
                i += 1;
                let s = String::from_utf8(s).map_err(|_| err(start_line, "string is not valid UTF-8"))?;
                stack.last_mut().ok_or_else(|| err(line, "string outside a list"))?.push(Sexpr::Atom(s));
            }
            _ => {
                let start = i;
                while i < b.len() && !b[i].is_ascii_whitespace() && !matches!(b[i], b'(' | b')' | b'"') {
                    i += 1;
                }
                // Splits happen on ASCII bytes only, so the slice is valid UTF-8.
                let atom = text[start..i].to_string();
                stack.last_mut().ok_or_else(|| err(line, "atom outside a list"))?.push(Sexpr::Atom(atom));
            }
        }
    }
    if !stack.is_empty() {
        return Err(err(line, "unbalanced `(`: the file ends inside a list"));
    }
    done.ok_or_else(|| err(line, "no expression found"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lists_atoms_strings() {
        let x = parse("(export (version \"E\")\n (net (code 1) (name \"/A \\\"b\\\"\")))").unwrap();
        assert_eq!(x.head(), Some("export"));
        assert_eq!(x.child_value("version"), Some("E"));
        let net = x.get("net").unwrap();
        assert_eq!(net.child_value("code"), Some("1"));
        assert_eq!(net.child_value("name"), Some("/A \"b\""));
        assert_eq!(parse(&x.to_string()).unwrap(), x);
        assert_eq!(parse("(a (b \"ü\"))").unwrap().get("b").unwrap().value(), Some("ü"));
    }

    #[test]
    fn reports_errors() {
        assert_eq!(parse("(a (b)").unwrap_err().message, "unbalanced `(`: the file ends inside a list");
        assert_eq!(parse("(a))").unwrap_err().message, "unbalanced `)`");
        assert_eq!(parse("(a \"x)").unwrap_err().message, "unterminated string");
        assert_eq!(parse("\n\nfoo").unwrap_err().line, 3);
        assert!(parse("").is_err());
        assert!(parse("(a) (b)").is_err());
    }
}
