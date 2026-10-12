#[path = "source.rs"]
mod subject;

fn entries() -> Vec<subject::Entry> {
    [
        (50, 2, 9), (40, 5, 7), (30, 5, 3),
        (10, 5, 3), (20, -2, 1), (60, 0, 4),
    ]
    .into_iter()
    .map(|(id, priority, created)| subject::Entry { id, priority, created })
    .collect()
}

fn ids(values: &[subject::Entry]) -> Vec<u64> {
    values.iter().map(|entry| entry.id).collect()
}

fn triples(values: &[subject::Entry]) -> Vec<(u64, i32, u64)> {
    values.iter().map(|entry| (entry.id, entry.priority, entry.created)).collect()
}

#[test]
fn requested_inclusive_ordered_filter_and_shared_paging_order() {
    let input = entries();
    let original = triples(&input);
    for (minimum, expected) in [
        (None, vec![10, 30, 40, 50, 60, 20]),
        (Some(2), vec![10, 30, 40, 50]),
        (Some(5), vec![10, 30, 40]),
        (Some(6), vec![]),
        (Some(-2), vec![10, 30, 40, 50, 60, 20]),
    ] {
        assert_eq!(ids(&subject::ordered(&input, minimum)), expected);
        assert_eq!(ids(&subject::page(&input, None, usize::MAX, minimum)), expected);
    }
    assert_eq!(triples(&input), original, "read-only ordering must preserve input");
    assert!(subject::ordered(&[], None).is_empty());
    assert!(subject::page(&input, None, 0, None).is_empty());
}

#[test]
fn survives_strict_cursor_positions_including_absent_and_filtered_cursors() {
    let input = entries();
    for (cursor, limit, minimum, expected) in [
        ((5, 3, 10), 2, None, vec![30, 40]),
        ((5, 7, 40), 2, None, vec![50, 60]),
        ((5, 3, 20), usize::MAX, None, vec![30, 40, 50, 60, 20]),
        ((5, 4, 0), usize::MAX, None, vec![40, 50, 60, 20]),
        ((6, 0, 0), usize::MAX, None, vec![10, 30, 40, 50, 60, 20]),
        ((-3, 0, 0), usize::MAX, None, vec![]),
        ((0, 4, 60), usize::MAX, Some(2), vec![]),
        ((5, 3, 20), usize::MAX, Some(5), vec![30, 40]),
    ] {
        assert_eq!(ids(&subject::page(&input, Some(cursor), limit, minimum)), expected);
    }
    let mut cursor = None;
    let mut seen = Vec::new();
    for _ in 0..7 {
        let page = subject::page(&input, cursor, 1, None);
        if let Some(entry) = page.first() {
            seen.push(entry.id);
            cursor = Some((entry.priority, entry.created, entry.id));
        } else {
            break;
        }
    }
    assert_eq!(seen, vec![10, 30, 40, 50, 60, 20]);
}

#[test]
fn survives_priority_and_cursor_extremes_without_overflow() {
    let input = vec![
        subject::Entry { id: 1, priority: i32::MIN, created: u64::MAX },
        subject::Entry { id: 2, priority: 0, created: 0 },
        subject::Entry { id: 3, priority: i32::MAX, created: u64::MAX },
    ];
    assert_eq!(ids(&subject::ordered(&input, None)), vec![3, 2, 1]);
    assert_eq!(ids(&subject::ordered(&input, Some(i32::MIN))), vec![3, 2, 1]);
    assert_eq!(ids(&subject::ordered(&input, Some(i32::MAX))), vec![3]);
    assert_eq!(ids(&subject::page(&input, Some((i32::MAX, u64::MAX, 3)), usize::MAX, None)), vec![2, 1]);
    assert!(subject::page(&input, Some((i32::MIN, u64::MAX, u64::MAX)), usize::MAX, None).is_empty());
}

#[test]
fn survives_creation_order_opposing_ids_and_strict_cursor_continuation() {
    // Independently derived from priority-descending, creation-ascending,
    // id-ascending contract. Creation must precede id; this permutation
    // deliberately makes the two keys disagree.
    let input = vec![
        subject::Entry { id: 900, priority: 5, created: 2 },
        subject::Entry { id: 100, priority: 5, created: 9 },
        subject::Entry { id: 700, priority: 5, created: 2 },
        subject::Entry { id: 300, priority: 5, created: 5 },
    ];
    let original = triples(&input);
    assert_eq!(ids(&subject::ordered(&input, None)), vec![700, 900, 300, 100]);
    assert_eq!(ids(&subject::ordered(&input, Some(5))), vec![700, 900, 300, 100]);
    assert_eq!(ids(&subject::page(&input, None, 2, None)), vec![700, 900]);
    assert_eq!(ids(&subject::page(&input, Some((5, 2, 900)), 2, None)), vec![300, 100]);
    for (cursor, expected) in [
        ((5, 2, 700), vec![900, 300, 100]),
        ((5, 2, 800), vec![900, 300, 100]),
        ((5, 4, 999), vec![300, 100]),
        ((5, 5, 300), vec![100]),
        ((5, 9, 100), vec![]),
    ] {
        assert_eq!(ids(&subject::page(&input, Some(cursor), usize::MAX, None)), expected);
    }
    assert_eq!(triples(&input), original);
}
