pub struct Parcel {
    pub id: String,
}

pub fn display(id: &str) -> String {
    format!("parcel:{id}")
}
