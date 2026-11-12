//! Valve's text KeyValues format (`libraryfolders.vdf`, `appmanifest_*.acf`):
//! `"key" "value"` pairs and `"key" { … }` blocks, with `//` comments.

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Text(String),
    Block(Node),
}

/// One block. Keys are matched case-insensitively, as Steam does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Node {
    pub entries: Vec<(String, Value)>,
}

impl Node {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v)
    }

    pub fn text(&self, key: &str) -> Option<&str> {
        match self.get(key)? {
            Value::Text(s) => Some(s),
            Value::Block(_) => None,
        }
    }

    pub fn block(&self, key: &str) -> Option<&Node> {
        match self.get(key)? {
            Value::Block(n) => Some(n),
            Value::Text(_) => None,
        }
    }

    pub fn blocks(&self) -> impl Iterator<Item = (&str, &Node)> {
        self.entries.iter().filter_map(|(k, v)| match v {
            Value::Block(n) => Some((k.as_str(), n)),
            Value::Text(_) => None,
        })
    }

    pub fn texts(&self) -> BTreeMap<&str, &str> {
        self.entries
            .iter()
            .filter_map(|(k, v)| match v {
                Value::Text(s) => Some((k.as_str(), s.as_str())),
                Value::Block(_) => None,
            })
            .collect()
    }
}

enum Token {
    Str(String),
    Open,
    Close,
}

fn tokenize(text: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' => out.push(Token::Open),
            '}' => out.push(Token::Close),
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            '"' => {
                let mut s = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => match chars.next() {
                            Some('n') => s.push('\n'),
                            Some('t') => s.push('\t'),
                            Some(other) => s.push(other),
                            None => {}
                        },
                        _ => s.push(c),
                    }
                }
                out.push(Token::Str(s));
            }
            c if c.is_whitespace() => {}
            _ => {
                // Unquoted token.
                let mut s = String::from(c);
                while let Some(&n) = chars.peek() {
                    if n.is_whitespace() || n == '{' || n == '}' || n == '"' {
                        break;
                    }
                    s.push(n);
                    chars.next();
                }
                out.push(Token::Str(s));
            }
        }
    }
    out
}

/// Parses a whole file. Malformed input yields whatever could be read, never an error.
pub fn parse(text: &str) -> Node {
    let tokens = tokenize(text.trim_start_matches('\u{feff}'));
    let mut iter = tokens.into_iter();
    parse_block(&mut iter)
}

fn parse_block(iter: &mut impl Iterator<Item = Token>) -> Node {
    let mut node = Node::default();
    while let Some(tok) = iter.next() {
        match tok {
            Token::Close => break,
            Token::Open => {
                // A block without a key; keep its contents under an empty key.
                node.entries.push((String::new(), Value::Block(parse_block(iter))));
            }
            Token::Str(key) => match iter.next() {
                Some(Token::Str(v)) => node.entries.push((key, Value::Text(v))),
                Some(Token::Open) => node.entries.push((key, Value::Block(parse_block(iter)))),
                Some(Token::Close) | None => break,
            },
        }
    }
    node
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_blocks() {
        let n = parse(
            "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"C:\\\\Program Files (x86)\\\\Steam\" // main\n\t\t\"apps\" { \"228980\" \"1\" }\n\t}\n}\n",
        );
        let root = n.block("LibraryFolders").unwrap();
        let first = root.block("0").unwrap();
        assert_eq!(first.text("path"), Some(r"C:\Program Files (x86)\Steam"));
        assert_eq!(first.block("apps").unwrap().text("228980"), Some("1"));
    }

    #[test]
    fn survives_garbage() {
        let n = parse("\"a\" { \"b\" \"c\" ");
        assert_eq!(n.block("a").unwrap().text("b"), Some("c"));
        assert_eq!(parse("}}}").entries.len(), 0);
    }
}
