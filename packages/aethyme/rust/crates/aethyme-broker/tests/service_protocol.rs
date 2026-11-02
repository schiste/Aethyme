use aethyme_broker::service::protocol::*;
use std::io::{self, Cursor};

#[test]
fn ping_wire_contract_and_roundtrip() {
    let request = Request::ping(42);
    let mut bytes = Vec::new();
    write_frame(&mut bytes, &request).unwrap();
    assert_eq!(
        bytes,
        b"{\"protocol_version\":3,\"request_id\":42,\"method\":\"ping\"}\n"
    );
    assert_eq!(
        read_frame::<Request>(&mut Cursor::new(bytes)).unwrap(),
        request
    );
}

#[test]
fn incompatible_protocol_is_an_explicit_error() {
    for version in [1, 2, 4] {
        let request = Request {
            protocol_version: version,
            ..Request::ping(1)
        };
        assert_eq!(
            request.validate().unwrap_err().code,
            ErrorCode::UnsupportedProtocol
        );
    }
}

#[test]
fn unknown_fields_and_methods_are_rejected() {
    for frame in [
        "{\"protocol_version\":3,\"request_id\":1,\"method\":\"ping\",\"cwd\":\"/tmp\"}\n",
        "{\"protocol_version\":3,\"request_id\":1,\"method\":\"execute\"}\n",
    ] {
        assert_eq!(
            read_frame::<Request>(&mut Cursor::new(frame))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
}

#[test]
fn valid_json_without_frame_terminator_is_incomplete() {
    let frame = serde_json::to_vec(&Request::ping(1)).unwrap();
    assert_eq!(
        read_frame::<Request>(&mut Cursor::new(frame))
            .unwrap_err()
            .kind(),
        io::ErrorKind::UnexpectedEof
    );
}

#[test]
fn frame_bound_includes_newline_and_preserves_next_frame() {
    // JSON string: two quotes, content, newline.
    let content = "x".repeat(MAX_FRAME_BYTES - 3);
    let mut bytes = Vec::new();
    write_frame(&mut bytes, &content).unwrap();
    assert_eq!(bytes.len(), MAX_FRAME_BYTES);
    bytes.extend_from_slice(b"123\n");
    let mut reader = Cursor::new(bytes);
    assert_eq!(read_frame::<String>(&mut reader).unwrap(), content);
    assert_eq!(read_frame::<u32>(&mut reader).unwrap(), 123);
}

#[test]
fn oversized_input_and_output_are_rejected() {
    let content = "x".repeat(MAX_FRAME_BYTES - 2);
    let mut output = Vec::new();
    assert_eq!(
        write_frame(&mut output, &content).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert!(
        output.is_empty(),
        "oversized output must not publish a prefix"
    );
    let mut bytes = serde_json::to_vec(&content).unwrap();
    bytes.push(b'\n');
    assert_eq!(
        read_frame::<String>(&mut Cursor::new(bytes))
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn malformed_and_multiple_json_values_are_rejected() {
    for frame in [b"{}{}\n".as_slice(), b"\xff\n", b"\n"] {
        assert_eq!(
            read_frame::<Request>(&mut Cursor::new(frame))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
}

#[test]
fn response_correlation_and_error_wire_contract() {
    let request = Request::ping(7);
    let mut response = Response {
        protocol_version: PROTOCOL_VERSION,
        request_id: 7,
        result: Err(ServiceError {
            code: ErrorCode::UnsupportedProtocol,
            message: "upgrade client".into(),
        }),
    };
    response.validate_for(&request).unwrap();
    let value = serde_json::to_value(&response).unwrap();
    assert_eq!(value["result"]["Err"]["code"], "unsupported_protocol");
    response.request_id = 8;
    assert!(response.validate_for(&request).is_err());
    response.request_id = 7;
    response.protocol_version = 1;
    assert!(response.validate_for(&request).is_err());
}
