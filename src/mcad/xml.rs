//! A small non-validating XML reader for IDX messages: elements with attributes, text and
//! the byte offsets of their tags, so that a response can be written as the message it answers
//! with a few insertions. Comments, processing instructions, CDATA and a simple `DOCTYPE` are
//! accepted; namespaces are not resolved (callers match local names and look prefixes up in
//! `xmlns` attributes).

/// An element.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Element {
    /// Qualified name (`pdm:Item`).
    pub name: String,
    /// Attributes in document order, values unescaped.
    pub attrs: Vec<(String, String)>,
    /// Child elements.
    pub children: Vec<Element>,
    /// Text content directly inside the element (unescaped, concatenated, trimmed).
    pub text: String,
    /// Offset of the `<` of the start tag.
    pub start: usize,
    /// Offset just after the start tag's `>`.
    pub content_start: usize,
    /// Offset of the `<` of the end tag (`content_start` for an empty-element tag).
    pub content_end: usize,
    /// Offset just after the end tag (or the empty-element tag).
    pub end: usize,
}

impl Element {
    /// The name without its prefix.
    pub fn local(&self) -> &str {
        local(&self.name)
    }

    /// The prefix of the name (empty without one).
    pub fn prefix(&self) -> &str {
        self.name.split_once(':').map_or("", |(p, _)| p)
    }

    /// An attribute by qualified name.
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    /// Children by local name.
    pub fn kids<'a>(&'a self, local_name: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.children.iter().filter(move |c| c.local() == local_name)
    }

    /// First child by local name.
    pub fn kid(&self, local_name: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.local() == local_name)
    }

    /// Text of the first child by local name, or of its `Value` child (IDX properties).
    pub fn value(&self, local_name: &str) -> Option<&str> {
        let k = self.kid(local_name)?;
        if !k.text.is_empty() {
            return Some(&k.text);
        }
        k.kid("Value").map(|v| v.text.as_str())
    }

    /// This element and every descendant, depth first.
    pub fn walk<'a>(&'a self, out: &mut Vec<&'a Element>) {
        out.push(self);
        for c in &self.children {
            c.walk(out);
        }
    }

    /// The prefix bound to `namespace` by an `xmlns:` attribute of this element.
    pub fn prefix_of(&self, namespace: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, v)| v == namespace && k.starts_with("xmlns:")).map(|(k, _)| &k[6..])
    }
}

/// The local part of a qualified name.
pub fn local(name: &str) -> &str {
    name.rsplit_once(':').map_or(name, |(_, l)| l)
}

/// Parses a document; returns the root element or a message with the byte offset.
pub fn parse(text: &str) -> Result<Element, String> {
    let mut r = Reader { s: text, b: text.as_bytes(), i: 0 };
    r.misc()?;
    if r.i >= r.b.len() || r.b[r.i] != b'<' {
        return Err(r.err("no root element"));
    }
    let root = r.element()?;
    r.misc()?;
    if r.i != r.b.len() {
        return Err(r.err("content after the root element"));
    }
    Ok(root)
}

struct Reader<'a> {
    s: &'a str,
    b: &'a [u8],
    i: usize,
}

impl Reader<'_> {
    fn err(&self, what: &str) -> String {
        format!("{what} at byte {}", self.i)
    }

    fn starts(&self, p: &str) -> bool {
        self.b[self.i..].starts_with(p.as_bytes())
    }

    fn skip_to(&mut self, end: &str) -> Result<(), String> {
        match self.s[self.i..].find(end) {
            Some(k) => {
                self.i += k + end.len();
                Ok(())
            }
            None => Err(self.err(&format!("missing `{end}`"))),
        }
    }

    fn ws(&mut self) {
        while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    /// Whitespace, comments, processing instructions and a doctype outside the root.
    fn misc(&mut self) -> Result<(), String> {
        loop {
            self.ws();
            if self.starts("<?") {
                self.skip_to("?>")?;
            } else if self.starts("<!--") {
                self.skip_to("-->")?;
            } else if self.starts("<!DOCTYPE") {
                self.skip_to(">")?;
            } else {
                return Ok(());
            }
        }
    }

    fn name(&mut self) -> Result<String, String> {
        let st = self.i;
        while self.i < self.b.len() {
            let c = self.b[self.i];
            if c.is_ascii_whitespace() || matches!(c, b'>' | b'/' | b'=') {
                break;
            }
            self.i += 1;
        }
        if st == self.i {
            return Err(self.err("expected a name"));
        }
        Ok(self.s[st..self.i].to_string())
    }

    fn element(&mut self) -> Result<Element, String> {
        let start = self.i;
        self.i += 1;
        let name = self.name()?;
        let mut attrs = Vec::new();
        loop {
            self.ws();
            if self.i >= self.b.len() {
                return Err(self.err("unterminated start tag"));
            }
            if self.starts("/>") {
                self.i += 2;
                let at = self.i;
                return Ok(Element {
                    name,
                    attrs,
                    start,
                    content_start: at,
                    content_end: at,
                    end: at,
                    ..Default::default()
                });
            }
            if self.b[self.i] == b'>' {
                self.i += 1;
                break;
            }
            let k = self.name()?;
            self.ws();
            if self.i >= self.b.len() || self.b[self.i] != b'=' {
                return Err(self.err("expected `=`"));
            }
            self.i += 1;
            self.ws();
            let q = *self.b.get(self.i).ok_or_else(|| self.err("expected a quote"))?;
            if q != b'"' && q != b'\'' {
                return Err(self.err("expected a quote"));
            }
            self.i += 1;
            let st = self.i;
            while self.i < self.b.len() && self.b[self.i] != q {
                self.i += 1;
            }
            if self.i >= self.b.len() {
                return Err(self.err("unterminated attribute value"));
            }
            let v = unescape(&self.s[st..self.i]);
            self.i += 1;
            attrs.push((k, v));
        }
        let content_start = self.i;
        let mut children = Vec::new();
        let mut text = String::new();
        loop {
            if self.i >= self.b.len() {
                return Err(self.err(&format!("missing end tag of `{name}`")));
            }
            if self.starts("</") {
                let content_end = self.i;
                self.i += 2;
                let end = self.name()?;
                if end != name {
                    return Err(self.err(&format!("`{end}` closes `{name}`")));
                }
                self.ws();
                if self.b.get(self.i) != Some(&b'>') {
                    return Err(self.err("expected `>`"));
                }
                self.i += 1;
                let text = text.trim().to_string();
                return Ok(Element { name, attrs, children, text, start, content_start, content_end, end: self.i });
            } else if self.starts("<!--") {
                self.skip_to("-->")?;
            } else if self.starts("<?") {
                self.skip_to("?>")?;
            } else if self.starts("<![CDATA[") {
                let st = self.i + 9;
                self.skip_to("]]>")?;
                text.push_str(&self.s[st..self.i - 3]);
            } else if self.b[self.i] == b'<' {
                children.push(self.element()?);
            } else {
                let st = self.i;
                while self.i < self.b.len() && self.b[self.i] != b'<' {
                    self.i += 1;
                }
                text.push_str(&unescape(&self.s[st..self.i]));
            }
        }
    }
}

/// Replaces the predefined and numeric character references.
fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(k) = rest.find('&') {
        out.push_str(&rest[..k]);
        rest = &rest[k..];
        let Some(e) = rest.find(';') else { break };
        let ent = &rest[1..e];
        let c = match ent {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => ent
                .strip_prefix("#x")
                .map(|h| u32::from_str_radix(h, 16).ok())
                .unwrap_or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match c {
            Some(c) => {
                out.push(c);
                rest = &rest[e + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_elements_offsets_and_text() {
        let src = "<?xml version=\"1.0\"?>\n<!-- c --><a:r xmlns:a=\"urn:x\" k='v &amp; w'>\n  <a:b>one &lt;2&gt;</a:b><c/>\
                   <![CDATA[<raw>]]></a:r>\n";
        let r = parse(src).unwrap();
        assert_eq!((r.local(), r.prefix(), r.attr("k")), ("r", "a", Some("v & w")));
        assert_eq!(r.prefix_of("urn:x"), Some("a"));
        assert_eq!(r.kid("b").unwrap().text, "one <2>");
        assert_eq!(r.text, "<raw>");
        let c = r.kid("c").unwrap();
        assert_eq!(&src[c.start..c.content_end], "<c/>");
        let b = r.kid("b").unwrap();
        assert_eq!(&src[b.content_start..b.content_end], "one &lt;2&gt;");
        assert!(parse("<a><b></a>").is_err());
        assert!(parse("<a/><b/>").is_err());
        assert_eq!(unescape("&#65;&#x42;&bogus;"), "AB&bogus;");
    }
}
