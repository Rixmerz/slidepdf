//! A tiny read-only DOM over quick-xml.
//!
//! OOXML parts are small (a few KB to a few hundred KB), and the inheritance
//! rules need random access across slide, layout and master, so a tree is
//! simpler than streaming. Element prefixes are dropped (`a:solidFill` is stored as
//! `solidFill`); attributes keep their qualified name but can be looked up
//! by local name.

use quick_xml::events::Event;
use quick_xml::Reader;

#[derive(Debug, Default, Clone)]
pub struct Node {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Node>,
    pub text: String,
}

impl Node {
    /// Looks an attribute up by qualified name first (`r:id`), then by
    /// local name (`embed` matches `r:embed`).
    pub fn attr(&self, key: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == key)
            .or_else(|| self.attrs.iter().find(|(k, _)| k.rsplit(':').next() == Some(key)))
            .map(|(_, v)| v.as_str())
    }

    pub fn attr_i64(&self, key: &str) -> Option<i64> {
        self.attr(key).and_then(|v| v.trim().parse().ok())
    }

    pub fn attr_bool(&self, key: &str) -> Option<bool> {
        self.attr(key).map(|v| v == "1" || v == "true" || v == "on")
    }

    pub fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == name)
    }

    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }

    /// Follows a path of child names, e.g. `node.path(&["spPr", "xfrm", "off"])`.
    pub fn path(&self, names: &[&str]) -> Option<&Node> {
        let mut cur = self;
        for n in names {
            cur = cur.child(n)?;
        }
        Some(cur)
    }

    /// Depth-first search for the first descendant with this name.
    pub fn find(&self, name: &str) -> Option<&Node> {
        for c in &self.children {
            if c.name == name {
                return Some(c);
            }
            if let Some(f) = c.find(name) {
                return Some(f);
            }
        }
        None
    }
}

fn local(name: &[u8]) -> String {
    let s = match name.iter().position(|&b| b == b':') {
        Some(i) => &name[i + 1..],
        None => name,
    };
    String::from_utf8_lossy(s).into_owned()
}

pub const MAX_DEPTH: usize = 256;

pub fn parse(src: &[u8]) -> Result<Node, String> {
    let text = std::str::from_utf8(src).map_err(|e| format!("invalid UTF-8 in XML: {e}"))?;
    let mut reader = Reader::from_str(text);
    let mut stack: Vec<Node> = vec![Node::default()];

    let start = |e: &quick_xml::events::BytesStart| -> Node {
        let mut n = Node {
            name: local(e.name().as_ref()),
            ..Default::default()
        };
        for a in e.attributes().flatten() {
            let v = a.unescape_value().map(|v| v.into_owned()).unwrap_or_default();
            n.attrs.push((String::from_utf8_lossy(a.key.as_ref()).into_owned(), v));
        }
        n
    };

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                // Later passes recurse over the tree; cap nesting so a
                // hostile file cannot overflow the stack.
                if stack.len() > MAX_DEPTH {
                    return Err(format!("XML nested deeper than {MAX_DEPTH} levels"));
                }
                stack.push(start(&e))
            }
            Ok(Event::Empty(e)) => {
                let n = start(&e);
                stack.last_mut().unwrap().children.push(n);
            }
            Ok(Event::End(_)) => {
                if stack.len() > 1 {
                    let n = stack.pop().unwrap();
                    stack.last_mut().unwrap().children.push(n);
                }
            }
            Ok(Event::Text(t)) => {
                if let Ok(s) = t.unescape() {
                    stack.last_mut().unwrap().text.push_str(&s);
                }
            }
            Ok(Event::CData(t)) => {
                stack.last_mut().unwrap().text.push_str(&String::from_utf8_lossy(&t));
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(format!("XML error at {}: {e}", reader.buffer_position())),
        }
    }
    while stack.len() > 1 {
        let n = stack.pop().unwrap();
        stack.last_mut().unwrap().children.push(n);
    }
    let mut root = stack.pop().unwrap();
    let first = root.children.drain(..).next();
    first.ok_or_else(|| "empty XML document".to_string())
}
