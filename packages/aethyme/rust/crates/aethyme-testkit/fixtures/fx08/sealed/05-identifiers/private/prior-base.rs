#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item { pub raw: String, pub label: String }
pub fn escaped(raw: &str) -> String { raw.replace('&',"&amp;").replace('<',"&lt;").replace('>',"&gt;").replace('"',"&quot;") }
pub fn lookup<'a>(items: &'a [Item], raw: &str) -> Option<&'a Item> { items.iter().find(|item| escaped(&item.raw) == raw) }
pub fn display(item: &Item) -> String { escaped(&item.raw) }
