#[path = "source.rs"]
mod subject;

// A literal old-consumer grammar derived from the written wire contract.
// It is not a shared serializer or an implementation proposal.
fn old_reader(wire: &str) -> Option<(u32, u32)> {
    let (count, id) = wire.strip_prefix("recieved=")?.split_once(";id=")?;
    if count.is_empty() || id.is_empty()
        || !count.bytes().all(|b| b.is_ascii_digit())
        || !id.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    Some((count.parse().ok()?, id.parse().ok()?))
}

#[test]
fn requested_public_spelling_and_exact_descriptions() {
    for (received, id, expected) in [
        (0, 0, "Received 0 items for record 0"),
        (1, 42, "Received 1 items for record 42"),
        (u32::MAX, u32::MAX, "Received 4294967295 items for record 4294967295"),
    ] {
        let record = subject::Record { received, id };
        assert_eq!(record.received, received);
        assert_eq!(record.describe(), expected);
    }
}

#[test]
fn survives_exact_historical_wire_and_independent_old_reader() {
    for (received, id, expected) in [
        (0, 0, "recieved=0;id=0"),
        (1, 42, "recieved=1;id=42"),
        (u32::MAX, u32::MAX, "recieved=4294967295;id=4294967295"),
    ] {
        let record = subject::Record { received, id };
        let wire = record.serialize();
        assert_eq!(wire, expected);
        assert_eq!(old_reader(&wire), Some((received, id)));
        assert_eq!(record.received, received);
        assert_eq!(record.id, id);
    }
    for invalid in [
        "received=1;id=42", "recieved=1; id=42", "recieved=1;id=42\n",
        "recieved=1,id=42", "recieved=1;id=42;extra=0", "recieved=1",
        "recieved=4294967296;id=0",
    ] {
        assert_eq!(old_reader(invalid), None, "calibrate old reader: {invalid}");
    }
}
