pub fn parse_feature(setting: Option<&str>) -> Result<Vec<String>, String> {
    let Some(setting) = setting else { return Ok(vec!["search".into(), "labels".into()]); };
    if setting.is_empty() { return Ok(Vec::new()); }
    setting.split(',').map(|entry| {
        let entry = entry.trim();
        if entry.is_empty() || !entry.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-') {
            Err("invalid feature".into())
        } else { Ok(entry.to_string()) }
    }).collect()
}
