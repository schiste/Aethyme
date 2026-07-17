use crate::model::intern::InternedStr;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct UnresolvedNode {
    pub id: InternedStr,
    pub name: InternedStr,
    pub expected_kind: Option<InternedStr>,
    pub referenced_from_id: InternedStr,
    pub file_id: InternedStr,
    pub file_path: InternedStr,
    pub area_id: Option<InternedStr>,
    pub language: InternedStr,
}

impl UnresolvedNode {
    pub fn new(
        id: InternedStr,
        name: InternedStr,
        expected_kind: Option<InternedStr>,
        referenced_from_id: InternedStr,
        file_id: InternedStr,
        file_path: InternedStr,
        area_id: Option<InternedStr>,
        language: InternedStr,
    ) -> Self {
        Self {
            id,
            name,
            expected_kind,
            referenced_from_id,
            file_id,
            file_path,
            area_id,
            language,
        }
    }
}
