pub struct Record { pub recieved: u32, pub id: u32 }
impl Record {
    pub fn serialize(&self) -> String { format!("received={};id={}", self.recieved, self.id) }
    pub fn describe(&self) -> String { format!("record {}", self.id) }
}
