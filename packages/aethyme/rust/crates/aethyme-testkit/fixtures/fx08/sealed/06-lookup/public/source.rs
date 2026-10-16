#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lookup { Ready(Vec<String>), Unavailable }
pub fn fallback(value: &Lookup) -> bool { matches!(value, Lookup::Unavailable) }
pub fn render(value: &Lookup) -> String {
    match value { Lookup::Ready(items) => items.join(", "), Lookup::Unavailable => "unavailable".into() }
}
