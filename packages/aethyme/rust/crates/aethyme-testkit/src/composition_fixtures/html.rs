//! A minimal HTML model for the oracle. Shares nothing with any composer.

/// Just enough HTML for the fixtures: elements with double-quoted
/// attributes, text, comments and void elements. Anything else is an error,
/// so a mangled merge fails loudly rather than parsing leniently.
pub(super) struct Html {
    /// Node 0 is a synthetic root.
    pub(super) nodes: Vec<Node>,
}

pub(super) struct Node {
    pub(super) tag: String,
    pub(super) attrs: Vec<(String, String)>,
    parent: Option<usize>,
    children: Vec<Child>,
}

enum Child {
    Element(usize),
    Text(String),
}

impl Node {
    pub(super) fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

const VOID: &[&str] = &["input", "br", "img", "meta", "link", "hr"];

impl Html {
    pub(super) fn parse(source: &str) -> Result<Self, String> {
        let mut nodes = vec![Node {
            tag: String::new(),
            attrs: Vec::new(),
            parent: None,
            children: Vec::new(),
        }];
        let mut stack = vec![0];
        let mut rest = source;
        while !rest.is_empty() {
            let top = *stack.last().expect("root stays on the stack");
            if let Some(after) = rest.strip_prefix("<!--") {
                let end = after.find("-->").ok_or("unterminated comment")?;
                rest = &after[end + 3..];
            } else if let Some(after) = rest.strip_prefix("</") {
                let end = after.find('>').ok_or("unterminated end tag")?;
                let name = after[..end].trim();
                if stack.len() == 1 || nodes[top].tag != name {
                    return Err(format!("unexpected </{name}> (open: <{}>)", nodes[top].tag));
                }
                stack.pop();
                rest = &after[end + 1..];
            } else if let Some(after) = rest.strip_prefix('<') {
                let (node, self_closing, remaining) = Self::start_tag(after)?;
                let index = nodes.len();
                let void = VOID.contains(&node.0.as_str());
                nodes.push(Node {
                    tag: node.0,
                    attrs: node.1,
                    parent: Some(top),
                    children: Vec::new(),
                });
                nodes[top].children.push(Child::Element(index));
                if !self_closing && !void {
                    stack.push(index);
                }
                rest = remaining;
            } else {
                let end = rest.find('<').unwrap_or(rest.len());
                nodes[top]
                    .children
                    .push(Child::Text(rest[..end].to_owned()));
                rest = &rest[end..];
            }
        }
        if stack.len() != 1 {
            return Err(format!(
                "unclosed <{}>",
                nodes[*stack.last().expect("non-empty")].tag
            ));
        }
        Ok(Self { nodes })
    }

    #[allow(clippy::type_complexity)]
    fn start_tag(source: &str) -> Result<((String, Vec<(String, String)>), bool, &str), String> {
        let name_end = source
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
            .ok_or("unterminated start tag")?;
        if name_end == 0 {
            return Err("empty tag name".into());
        }
        let tag = source[..name_end].to_owned();
        let mut rest = &source[name_end..];
        let mut attrs: Vec<(String, String)> = Vec::new();
        loop {
            rest = rest.trim_start();
            if let Some(after) = rest.strip_prefix("/>") {
                return Ok(((tag, attrs), true, after));
            }
            if let Some(after) = rest.strip_prefix('>') {
                return Ok(((tag, attrs), false, after));
            }
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || "-_:".contains(c)))
                .ok_or("unterminated attribute")?;
            if end == 0 {
                return Err(format!(
                    "bad attribute in <{tag}> near {:?}",
                    &rest[..rest.len().min(12)]
                ));
            }
            let name = rest[..end].to_owned();
            rest = &rest[end..];
            let value = if let Some(after) = rest.strip_prefix("=\"") {
                let close = after.find('"').ok_or("unterminated attribute value")?;
                rest = &after[close + 1..];
                after[..close].to_owned()
            } else {
                String::new()
            };
            if attrs.iter().any(|(existing, _)| *existing == name) {
                return Err(format!("<{tag}> repeats attribute {name}"));
            }
            attrs.push((name, value));
        }
    }

    pub(super) fn ancestors(&self, node: usize) -> impl Iterator<Item = usize> + '_ {
        std::iter::successors(self.nodes[node].parent, |node| self.nodes[*node].parent)
            .filter(|node| *node != 0)
    }

    /// Descendant text, whitespace-normalized.
    pub(super) fn text(&self, node: usize) -> String {
        fn collect(doc: &Html, node: usize, out: &mut String) {
            for child in &doc.nodes[node].children {
                match child {
                    Child::Text(text) => {
                        out.push(' ');
                        out.push_str(text);
                    }
                    Child::Element(child) => collect(doc, *child, out),
                }
            }
        }
        let mut out = String::new();
        collect(self, node, &mut out);
        out.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Elements matching a descendant selector of compounds such as
    /// `tag#id.class[attr=value]`, in document order.
    pub(super) fn select(&self, selector: &str) -> Vec<usize> {
        let compounds: Vec<Compound> = selector.split_whitespace().map(Compound::parse).collect();
        let Some((last, outer)) = compounds.split_last() else {
            return Vec::new();
        };
        (1..self.nodes.len())
            .filter(|node| last.matches(&self.nodes[*node]) && self.ancestors_match(*node, outer))
            .collect()
    }

    fn ancestors_match(&self, node: usize, outer: &[Compound]) -> bool {
        let mut remaining = outer;
        for ancestor in self.ancestors(node) {
            let Some((last, rest)) = remaining.split_last() else {
                break;
            };
            if last.matches(&self.nodes[ancestor]) {
                remaining = rest;
            }
        }
        remaining.is_empty()
    }
}

#[derive(Default)]
struct Compound {
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
    attrs: Vec<(String, String)>,
}

impl Compound {
    fn parse(text: &str) -> Self {
        let mut compound = Self::default();
        let mut rest = text;
        let token_end = |rest: &str| rest.find(['#', '.', '[']).unwrap_or(rest.len());
        let end = token_end(rest);
        if end > 0 {
            compound.tag = Some(rest[..end].to_owned());
        }
        rest = &rest[end..];
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix('#') {
                let end = token_end(after);
                compound.id = Some(after[..end].to_owned());
                rest = &after[end..];
            } else if let Some(after) = rest.strip_prefix('.') {
                let end = token_end(after);
                compound.classes.push(after[..end].to_owned());
                rest = &after[end..];
            } else if let Some(after) = rest.strip_prefix('[') {
                let end = after
                    .find(']')
                    .unwrap_or_else(|| panic!("selector {text:?}: unterminated ["));
                let (name, value) = after[..end]
                    .split_once('=')
                    .unwrap_or_else(|| panic!("selector {text:?}: [name=value]"));
                compound
                    .attrs
                    .push((name.to_owned(), value.trim_matches('"').to_owned()));
                rest = &after[end + 1..];
            } else {
                panic!("selector {text:?}: unexpected {rest:?}");
            }
        }
        compound
    }

    fn matches(&self, node: &Node) -> bool {
        self.tag.as_ref().is_none_or(|tag| *tag == node.tag)
            && self
                .id
                .as_ref()
                .is_none_or(|id| node.attr("id") == Some(id))
            && self.classes.iter().all(|class| {
                node.attr("class")
                    .is_some_and(|classes| classes.split_whitespace().any(|c| c == class))
            })
            && self
                .attrs
                .iter()
                .all(|(name, value)| node.attr(name) == Some(value))
    }
}
