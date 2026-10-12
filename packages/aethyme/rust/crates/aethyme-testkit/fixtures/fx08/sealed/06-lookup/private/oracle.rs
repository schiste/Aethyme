#[path = "source.rs"]
mod subject;

fn ready(items: &[&str]) -> subject::Lookup {
    subject::Lookup::Ready(items.iter().map(|item| (*item).to_string()).collect())
}

#[test]
fn requested_render_preserves_exact_state_and_join_strings() {
    assert_eq!(subject::render(&subject::Lookup::Unavailable), "Lookup unavailable");
    for (items, expected) in [
        (vec![], "No results"), (vec!["red", "blue"], "red, blue"),
        (vec!["Lookup unavailable"], "Lookup unavailable"),
        (vec![""], ""), (vec!["", "x"], ", x"),
        (vec!["No results"], "No results"),
    ] {
        assert_eq!(subject::render(&ready(&items)), expected);
    }
}

#[test]
fn requested_display_limits_unicode_scalar_values_after_render() {
    let value = ready(&["é猫🦊x"]);
    for (limit, expected) in [
        (0, ""), (1, "é"), (2, "é猫"), (3, "é猫🦊"),
        (4, "é猫🦊x"), (5, "é猫🦊x"), (usize::MAX, "é猫🦊x"),
    ] {
        assert_eq!(subject::display_field(&value, limit), expected);
    }
    assert_eq!(subject::display_field(&ready(&["e\u{301}x"]), 1), "e");
    assert_eq!(subject::display_field(&ready(&["e\u{301}x"]), 2), "e\u{301}");
    assert_eq!(subject::display_field(&ready(&["é", "猫"]), 3), "é, ");
    assert_eq!(subject::display_field(&ready(&["é", "猫"]), 4), "é, 猫");
    assert_eq!(subject::display_field(&subject::Lookup::Unavailable, 6), "Lookup");
    assert_eq!(subject::display_field(&ready(&[]), 3), "No ");
    assert_eq!(subject::display_field(&ready(&[]), 0), "");
    assert_eq!(subject::display_field(&ready(&[]), usize::MAX), "No results");
}

#[test]
fn survives_fallback_only_for_unavailable_not_empty_or_placeholder_ready() {
    assert!(subject::fallback(&subject::Lookup::Unavailable));
    for items in [vec![], vec![""], vec!["Lookup unavailable"], vec!["No results"], vec!["red", "blue"]] {
        let value = ready(&items);
        assert!(!subject::fallback(&value));
        let _formatted = subject::render(&value);
        let _bounded = subject::display_field(&value, 0);
        assert!(!subject::fallback(&value), "presentation cannot create unavailability");
    }
}
