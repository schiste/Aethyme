#[path = "source.rs"]
mod subject;

fn item(raw: &str, label: &str) -> subject::Item {
    subject::Item { raw: raw.to_string(), label: label.to_string() }
}

fn projection(items: &[subject::Item]) -> Vec<(String, String)> {
    items.iter().map(|item| (item.raw.clone(), item.label.clone())).collect()
}

#[test]
fn requested_display_escapes_both_fields_exactly_once() {
    let original = item("<&\"", "A<&\"");
    assert_eq!(subject::display(&original), "A&lt;&amp;&quot; [&lt;&amp;&quot;]");
    assert_eq!(original.raw, "<&\"");
    assert_eq!(original.label, "A<&\"");
    assert_eq!(subject::display(&item("&lt;", "&amp;")), "&amp;amp; [&amp;lt;]");
    assert_eq!(subject::display(&item("", "")), " []");
    for (input, expected) in [
        ("", ""), ("&lt;", "&amp;lt;"), ("<>\"&", "&lt;&gt;&quot;&amp;"),
        ("'\\", "'\\"), ("é猫", "é猫"),
    ] {
        assert_eq!(subject::escaped(input), expected);
    }
}

#[test]
fn requested_filter_uses_raw_case_sensitive_labels_and_preserves_originals() {
    let items = vec![
        item(" raw<&\" ", "Harbor Lake"), item("Two", "lake"),
        item("three", "Lakeside"), item("", ""),
        item("literal", "&lt;"), item("angle", "<"),
    ];
    let original = projection(&items);
    assert_eq!(projection(&subject::filtered(&items, "Lake")), vec![original[0].clone(), original[2].clone()]);
    assert_eq!(projection(&subject::filtered(&items, "lake")), vec![original[1].clone()]);
    assert_eq!(projection(&subject::filtered(&items, "")), original);
    assert_eq!(projection(&subject::filtered(&items, "&lt;")), vec![original[4].clone()]);
    assert!(subject::filtered(&items, "missing").is_empty());
    assert!(subject::filtered(&[], "").is_empty());
    assert_eq!(projection(&items), original);
}

#[test]
fn survives_exact_lookup_without_presentation_or_unicode_normalization() {
    let items = vec![
        item("<&\"", "escaped"), item(" X ", "spaces"), item("Key", "case"),
        item("é", "composed"), item("e\u{301}", "decomposed"), item("", "empty"),
    ];
    let original = projection(&items);
    for (raw, expected_label) in [
        ("<&\"", "escaped"), (" X ", "spaces"), ("Key", "case"),
        ("é", "composed"), ("e\u{301}", "decomposed"), ("", "empty"),
    ] {
        let found = subject::lookup(&items, raw).expect("exact raw identity exists");
        assert_eq!(found.raw, raw);
        assert_eq!(found.label, expected_label);
        let _presentation = subject::display(found);
        assert_eq!(subject::lookup(&items, raw).unwrap().raw, raw);
    }
    for absent in ["&lt;&amp;&quot;", "X", "key", "É", "e"] {
        assert!(subject::lookup(&items, absent).is_none(), "must not normalize: {absent}");
    }
    assert!(subject::lookup(&[], "").is_none());
    assert_eq!(projection(&items), original);
}
