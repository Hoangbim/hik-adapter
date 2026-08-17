use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{Arc, atomic::AtomicBool},
    thread,
    time::Duration,
};

use hik_adapter::{
    api::VtmContext,
    error::ErrorCategory,
    model::Camera,
    vtm::{Channel, MessageCode, VTM_HEADER_SIZE, VtmPacket, VtmSession, decode_header},
};
use secrecy::SecretString;

fn context(port: u16) -> VtmContext {
    VtmContext {
        camera: Camera {
            index: 1,
            name: "Camera".to_owned(),
            device_name: "Device".to_owned(),
            serial: "SERIAL".to_owned(),
            channel: 1,
        },
        relay_host: "127.0.0.1".to_owned(),
        relay_port: port,
        stream_url: SecretString::from(format!(
            "ysproto://127.0.0.1:{port}/live?dev=SERIAL&chn=1&ssn=token"
        )),
    }
}

fn read_packet(stream: &mut impl Read) -> VtmPacket {
    let mut header = [0_u8; VTM_HEADER_SIZE];
    stream.read_exact(&mut header).unwrap();
    let decoded = decode_header(header).unwrap();
    let mut body = vec![0; decoded.body_len.into()];
    stream.read_exact(&mut body).unwrap();
    VtmPacket {
        header: decoded,
        body,
    }
}

fn write_packet(stream: &mut impl Write, channel: Channel, code: MessageCode, body: Vec<u8>) {
    stream
        .write_all(&VtmPacket::new(channel, code, 0, body).encode().unwrap())
        .unwrap();
}

#[test]
fn negotiates_and_streams_mpeg_ps_without_buffering() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        assert_eq!(
            read_packet(&mut stream).header.code,
            MessageCode::StreamInfoReq
        );
        write_packet(
            &mut stream,
            Channel::Message,
            MessageCode::StreamInfoRsp,
            vec![
                0x08, 0x00, 0x22, 0x07, b's', b'e', b's', b's', b'i', b'o', b'n',
            ],
        );
        write_packet(
            &mut stream,
            Channel::Stream,
            MessageCode::StreamInfoNotify,
            vec![0, 0, 1, 0xba, 1, 2, 3],
        );
        thread::sleep(Duration::from_millis(150));
        write_packet(
            &mut stream,
            Channel::Stream,
            MessageCode::StreamInfoNotify,
            vec![4, 5, 6],
        );
        thread::sleep(Duration::from_millis(100));
    });
    let mut session = VtmSession::connect(context(port), Duration::from_millis(500)).unwrap();
    let response = session.start(3).unwrap();
    assert_eq!(response.result, Some(0));
    let shutdown = AtomicBool::new(false);
    let received = Arc::new(std::sync::Mutex::new(Vec::new()));
    let target = Arc::clone(&received);
    let stats = session
        .for_each_media(
            Duration::from_millis(100),
            Duration::from_millis(500),
            &shutdown,
            move |body| {
                target.lock().unwrap().extend_from_slice(body);
                Ok(())
            },
        )
        .unwrap();
    assert!(stats.packets >= 1);
    assert!(received.lock().unwrap().starts_with(&[0, 0, 1, 0xba]));
    server.join().unwrap();
}

#[test]
fn rejects_encrypted_media_without_exposing_body() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_packet(&mut stream);
        write_packet(
            &mut stream,
            Channel::Message,
            MessageCode::StreamInfoRsp,
            vec![0x08, 0x00],
        );
        write_packet(
            &mut stream,
            Channel::EncryptedStream,
            MessageCode::StreamInfoNotify,
            b"super-secret-payload".to_vec(),
        );
    });
    let mut session = VtmSession::connect(context(port), Duration::from_millis(500)).unwrap();
    session.start(3).unwrap();
    let error = session
        .for_each_media(
            Duration::from_millis(20),
            Duration::from_millis(500),
            &AtomicBool::new(false),
            |_| Ok(()),
        )
        .unwrap_err();
    assert_eq!(error.category, ErrorCategory::EncryptedStream);
    assert!(!error.to_string().contains("super-secret"));
    server.join().unwrap();
}

#[test]
fn maps_negotiated_but_idle_session_to_no_media() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_packet(&mut stream);
        write_packet(
            &mut stream,
            Channel::Message,
            MessageCode::StreamInfoRsp,
            vec![0x08, 0x00],
        );
        thread::sleep(Duration::from_millis(300));
    });
    let mut session = VtmSession::connect(context(port), Duration::from_millis(30)).unwrap();
    session.start(3).unwrap();
    let error = session
        .for_each_media(
            Duration::from_millis(20),
            Duration::from_millis(80),
            &AtomicBool::new(false),
            |_| Ok(()),
        )
        .unwrap_err();
    assert_eq!(error.category, ErrorCategory::NoMedia);
    server.join().unwrap();
}
