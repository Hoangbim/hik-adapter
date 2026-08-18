use hik_adapter::vtm::{
    Channel, H265Assembler, MediaPipe, MediaTransport, MessageCode, RtpFields, TransportLock,
    VtmPacket, build_stream_info_request, classify, decode_header, hex_prefix,
    parse_stream_info_response,
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
fn accepts_live_ecdh_control_notification() {
    let header = decode_header([0x24, 0, 0, 0, 0, 1, 0x01, 0x4a]).unwrap();
    assert_eq!(header.code, MessageCode::VtmStreamEcdhNotify);
}

#[test]
fn preserves_unknown_message_codes_for_ignored_packets() {
    let header = decode_header([0x24, 0, 0, 0, 0, 1, 0x02, 0x01]).unwrap();
    assert_eq!(header.code.raw(), 0x0201);
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
    assert_eq!(lock.accept(b"fragment").unwrap(), None);
    assert_eq!(
        lock.accept(&[9, 8, 0, 0, 1, 0xba, 1, 2]).unwrap(),
        Some(&[0, 0, 1, 0xba, 1, 2][..])
    );
    assert_eq!(
        lock.accept(b"later-fragment").unwrap(),
        Some(&b"later-fragment"[..])
    );
}

#[test]
fn classifies_rtp_packet_fields() {
    let guess = classify(&[
        0x80, 0xe0, 0x12, 0x34, 0x00, 0x01, 0x02, 0x03, 0xde, 0xad, 0xbe, 0xef, 0x67,
    ]);
    assert_eq!(
        guess.rtp,
        Some(RtpFields {
            payload_type: 96,
            marker: true,
            sequence: 0x1234,
            timestamp: 0x00010203,
        })
    );
    assert!(!guess.ps_pack_start);
    assert!(!guess.annex_b);
    assert!(!guess.ts_sync);
}

#[test]
fn classifies_annex_b_nal_units() {
    let guess = classify(&[0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00]);
    assert!(guess.annex_b);
    assert_eq!(guess.rtp, None);
    assert!(!guess.ps_pack_start);
    assert!(!guess.ts_sync);
}

#[test]
fn classifies_ts_sync_and_ps_pack_start() {
    let ts = vec![0x47; 188];
    let guess = classify(&ts);
    assert!(guess.ts_sync);
    assert_eq!(guess.rtp, None);

    let guess = classify(&[0x00, 0x00, 0x01, 0xba, 0x44]);
    assert!(guess.ps_pack_start);
    assert!(!guess.ts_sync);
}

#[test]
fn hex_prefix_truncates_and_formats() {
    assert_eq!(hex_prefix(&[0x00, 0xff, 0x0a], 96), "00 ff 0a");
    assert_eq!(hex_prefix(&[0x00, 0x01, 0x02, 0x03], 2), "00 01 ...");
    assert_eq!(hex_prefix(&[], 96), "");
}

fn rtp_body(pt: u8, payload: &[u8]) -> Vec<u8> {
    let mut body = vec![0x80, pt, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x55, 0x66, 0x77, 0x88];
    body.extend_from_slice(payload);
    body
}

#[test]
fn h265_complete_nal_passes_through() {
    let mut assembler = H265Assembler::default();
    assert_eq!(assembler.accept(&[0x40, 0x01, 0x0c, 0x01]), Some(vec![0x40, 0x01, 0x0c, 0x01]));
}

#[test]
fn h265_fu_fragments_reassemble_with_reconstructed_header() {
    let mut assembler = H265Assembler::default();
    assert_eq!(assembler.accept(&[0x62, 0x01, 0x93, 0xde, 0xad]), None);
    assert_eq!(assembler.accept(&[0x62, 0x01, 0x26, 0xbe, 0xef]), None);
    assert_eq!(
        assembler.accept(&[0x62, 0x01, 0x66, 0xca, 0xfe]),
        Some(vec![0x26, 0x01, 0xde, 0xad, 0xbe, 0xef, 0xca, 0xfe])
    );
}

#[test]
fn h265_fu_start_skips_repeated_inner_header() {
    let mut assembler = H265Assembler::default();
    assert_eq!(assembler.accept(&[0x62, 0x01, 0x93, 0x26, 0x01, 0xde, 0xad]), None);
    assert_eq!(
        assembler.accept(&[0x62, 0x01, 0x66, 0xbe, 0xef]),
        Some(vec![0x26, 0x01, 0xde, 0xad, 0xbe, 0xef])
    );
}

#[test]
fn h265_orphan_fragments_are_dropped() {
    let mut assembler = H265Assembler::default();
    assert_eq!(assembler.accept(&[0x62, 0x01, 0x26, 0xbe, 0xef]), None);
    assert_eq!(assembler.accept(&[0x62, 0x01, 0x66, 0xca, 0xfe]), None);
}

#[test]
fn h265_forbidden_bit_payload_rejected() {
    let mut assembler = H265Assembler::default();
    assert_eq!(assembler.accept(&[0xaf, 0x01, 0x0c, 0x01]), None);
}

#[test]
fn media_pipe_locks_rtp_h265_and_reassembles() {
    let mut pipe = MediaPipe::default();
    assert_eq!(pipe.accept(b"garbage").unwrap(), None);
    let vps = rtp_body(96, &[0x40, 0x01, 0x0c, 0x01]);
    assert_eq!(
        pipe.accept(&vps).unwrap(),
        Some((MediaTransport::RtpH265, vec![0, 0, 0, 1, 0x40, 0x01, 0x0c, 0x01]))
    );
    assert_eq!(pipe.transport(), Some(MediaTransport::RtpH265));
    // PT112 metadata is ignored once locked.
    assert_eq!(pipe.accept(&rtp_body(112, &[0x48, 0x4b])).unwrap(), None);
    // Fragmented IDR across three packets.
    assert_eq!(pipe.accept(&rtp_body(96, &[0x62, 0x01, 0x93, 0xde, 0xad])).unwrap(), None);
    assert_eq!(pipe.accept(&rtp_body(96, &[0x62, 0x01, 0x26, 0xbe, 0xef])).unwrap(), None);
    assert_eq!(
        pipe.accept(&rtp_body(96, &[0x62, 0x01, 0x66, 0xca, 0xfe])).unwrap(),
        Some((
            MediaTransport::RtpH265,
            vec![0, 0, 0, 1, 0x26, 0x01, 0xde, 0xad, 0xbe, 0xef, 0xca, 0xfe]
        ))
    );
}

#[test]
fn media_pipe_locks_mpeg_ps_before_rtp() {
    let mut pipe = MediaPipe::default();
    assert_eq!(
        pipe.accept(&[9, 8, 0, 0, 1, 0xba, 1, 2]).unwrap(),
        Some((MediaTransport::MpegPs, vec![0, 0, 1, 0xba, 1, 2]))
    );
    // An RTP payload carrying a PS pack header locks to PS as well.
    let mut pipe = MediaPipe::default();
    let ps_rtp = rtp_body(96, &[0x00, 0x00, 0x01, 0xba, 0x44]);
    assert_eq!(
        pipe.accept(&ps_rtp).unwrap(),
        Some((MediaTransport::MpegPs, vec![0, 0, 1, 0xba, 0x44]))
    );
}
