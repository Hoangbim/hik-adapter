use hik_adapter::{
    error::ErrorCategory,
    vtm::{
        Channel, MessageCode, TransportLock, VtmPacket, build_stream_info_request, decode_header,
        parse_stream_info_response,
    },
};
use secrecy::{ExposeSecret, SecretString};

#[test]
fn encodes_vtm_header_big_endian() {
    let packet = VtmPacket::new(
        Channel::Message,
        MessageCode::StreamInfoReq,
        0,
        vec![0x0a, 0x01, b'x'],
    );
    assert_eq!(
        packet.encode().unwrap(),
        vec![
            0x24, 0x00, 0x00, 0x03, 0x00, 0x00, 0x01, 0x3b, 0x0a, 0x01, b'x'
        ]
    );
}

#[test]
fn rejects_unknown_channel_and_bad_body_length() {
    assert!(decode_header([0x24, 0x7f, 0, 0, 0, 0, 1, 0x3b]).is_err());
    assert!(VtmPacket::decode(&[0x24, 0, 0, 2, 0, 0, 1, 0x3b, 0]).is_err());
}

#[test]
fn parses_stream_info_response_fields_and_redacts_secrets() {
    let body = [
        0x08, 0x00, 0x22, 0x03, b's', b's', b'n', 0x2a, 0x03, b'k', b'e', b'y', 0x3a, 0x03, b'u',
        b'r', b'l',
    ];
    let response = parse_stream_info_response(&body).unwrap();
    assert_eq!(response.result, Some(0));
    assert_eq!(response.stream_session.as_deref(), Some("ssn"));
    assert_eq!(
        response
            .redirect_key
            .as_ref()
            .map(ExposeSecret::expose_secret),
        Some("key")
    );
    assert!(!format!("{response:?}").contains("Some(\"key\")"));
}

#[test]
fn rejects_malformed_protobuf() {
    for body in [
        vec![0x80],
        vec![0x0b],
        vec![0x22, 0x05, b'x'],
        vec![0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x02],
    ] {
        assert!(
            parse_stream_info_response(&body).is_err(),
            "accepted {body:x?}"
        );
    }
}

#[test]
fn builds_minimal_stream_info_request() {
    let url = SecretString::from("ysproto://relay/live?ssn=secret".to_owned());
    let key = SecretString::from("redirect-key".to_owned());
    let body = build_stream_info_request(&url, Some(&key));
    assert!(body.windows(8).any(|part| part == b"ysproto:"));
    assert!(body.windows(12).any(|part| part == b"redirect-key"));
}

#[test]
fn transport_locks_once_to_mpeg_ps() {
    let mut lock = TransportLock::default();
    let error = lock.accept(b"fragment").unwrap_err();
    assert_eq!(error.category, ErrorCategory::UnsupportedTransport);
    lock.accept(&[0, 0, 1, 0xba, 1, 2]).unwrap();
    lock.accept(b"later-fragment").unwrap();
}
