use std::{
    io::{ErrorKind, Read, Write},
    net::{TcpStream, ToSocketAddrs},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use secrecy::{ExposeSecret, SecretString};
use url::Url;

use crate::{
    api::VtmContext,
    error::{AdapterError, ErrorCategory},
};

use super::{
    Channel, MessageCode, StreamInfoResponse, TransportLock, VTM_HEADER_SIZE, VtmPacket,
    build_keepalive_request, build_stream_info_request, decode_header, parse_stream_info_response,
};

#[derive(Clone, Copy, Debug, Default)]
pub struct MediaStats {
    pub packets: u64,
    pub bytes: u64,
    pub elapsed: Duration,
}

pub struct VtmSession {
    context: VtmContext,
    socket: TcpStream,
    timeout: Duration,
    sequence: u16,
    stream_session: Option<String>,
}

impl VtmSession {
    pub fn connect(context: VtmContext, timeout: Duration) -> Result<Self, AdapterError> {
        let socket = connect_url(&context.stream_url, timeout)?;
        Ok(Self {
            context,
            socket,
            timeout,
            sequence: 0,
            stream_session: None,
        })
    }

    pub fn start(&mut self, max_redirects: usize) -> Result<StreamInfoResponse, AdapterError> {
        let mut redirects = 0_usize;
        let mut redirect_key: Option<SecretString> = None;
        loop {
            self.send_packet(
                Channel::Message,
                MessageCode::StreamInfoReq,
                build_stream_info_request(&self.context.stream_url, redirect_key.as_ref()),
            )?;
            for _ in 0..20 {
                let packet = self.read_packet()?;
                if packet.header.channel == Channel::EncryptedMessage {
                    return Err(session_error(
                        ErrorCategory::EncryptedStream,
                        "encrypted VTM control channel is unsupported",
                    ));
                }
                if packet.header.code == MessageCode::KeepAliveReq {
                    self.send_packet(Channel::Message, MessageCode::KeepAliveRsp, packet.body)?;
                    continue;
                }
                if !matches!(
                    packet.header.code,
                    MessageCode::StreamInfoRsp | MessageCode::StreamInfoNotify
                ) || packet.header.channel != Channel::Message
                {
                    continue;
                }
                let response = parse_stream_info_response(&packet.body)?;
                if response.result == Some(0) {
                    self.stream_session = response.stream_session.clone();
                    return Ok(response);
                }
                if let (Some(url), Some(key)) = (
                    response.redirect_url.as_ref(),
                    response.redirect_key.as_ref(),
                ) {
                    if redirects >= max_redirects {
                        return Err(session_error(
                            ErrorCategory::VtmRedirectLimit,
                            "VTM redirect limit exceeded",
                        ));
                    }
                    redirects += 1;
                    self.context.stream_url = url.clone();
                    self.socket = connect_url(&self.context.stream_url, self.timeout)?;
                    redirect_key = Some(key.clone());
                    break;
                }
                return Err(session_error(
                    ErrorCategory::VtmNegotiationFailed,
                    format!(
                        "VTM negotiation was rejected (result={})",
                        response
                            .result
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "missing".to_owned())
                    ),
                ));
            }
            if redirect_key.is_none() {
                return Err(session_error(
                    ErrorCategory::VtmNegotiationFailed,
                    "VTM negotiation response was not received",
                ));
            }
        }
    }

    pub fn for_each_media<F>(
        &mut self,
        media_duration: Duration,
        no_media_timeout: Duration,
        shutdown: &AtomicBool,
        mut sink: F,
    ) -> Result<MediaStats, AdapterError>
    where
        F: FnMut(&[u8]) -> Result<(), AdapterError>,
    {
        let waiting_since = Instant::now();
        let mut media_since: Option<Instant> = None;
        let mut last_keepalive = Instant::now();
        let mut stats = MediaStats::default();
        let mut transport = TransportLock::default();
        loop {
            if shutdown.load(Ordering::Relaxed) {
                return Err(session_error(
                    ErrorCategory::Cancelled,
                    "recording cancelled",
                ));
            }
            if let Some(started) = media_since {
                if started.elapsed() >= media_duration {
                    stats.elapsed = started.elapsed();
                    return Ok(stats);
                }
            } else if waiting_since.elapsed() >= no_media_timeout {
                return Err(session_error(
                    ErrorCategory::NoMedia,
                    "VTM relay returned no media",
                ));
            }
            if last_keepalive.elapsed() >= Duration::from_secs(5) {
                if let Some(stream_session) = self.stream_session.clone() {
                    self.send_packet(
                        Channel::Message,
                        MessageCode::KeepAliveReq,
                        build_keepalive_request(&stream_session),
                    )?;
                }
                last_keepalive = Instant::now();
            }
            let packet = match self.read_packet() {
                Ok(packet) => packet,
                Err(error) if error.category == ErrorCategory::NoMedia => continue,
                Err(error) => return Err(error),
            };
            match packet.header.channel {
                Channel::EncryptedStream | Channel::EncryptedMessage => {
                    return Err(session_error(
                        ErrorCategory::EncryptedStream,
                        "encrypted VTM stream is unsupported",
                    ));
                }
                Channel::Stream => {
                    transport.accept(&packet.body)?;
                    let started = media_since.get_or_insert_with(Instant::now);
                    sink(&packet.body)?;
                    stats.packets += 1;
                    stats.bytes += packet.body.len() as u64;
                    stats.elapsed = started.elapsed();
                }
                Channel::Message if packet.header.code == MessageCode::KeepAliveReq => {
                    self.send_packet(Channel::Message, MessageCode::KeepAliveRsp, packet.body)?;
                }
                Channel::Message => {}
            }
        }
    }

    fn send_packet(
        &mut self,
        channel: Channel,
        code: MessageCode,
        body: Vec<u8>,
    ) -> Result<(), AdapterError> {
        let packet = VtmPacket::new(channel, code, self.sequence, body).encode()?;
        self.sequence = self.sequence.wrapping_add(1);
        self.socket.write_all(&packet).map_err(|_| {
            session_error(
                ErrorCategory::VtmNegotiationFailed,
                "VTM socket write failed",
            )
        })
    }

    fn read_packet(&mut self) -> Result<VtmPacket, AdapterError> {
        let mut header_bytes = [0_u8; VTM_HEADER_SIZE];
        if let Err(error) = self.socket.read_exact(&mut header_bytes) {
            let category = if matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) {
                ErrorCategory::NoMedia
            } else {
                ErrorCategory::VtmNegotiationFailed
            };
            return Err(session_error(category, "VTM socket read failed"));
        }
        let header = decode_header(header_bytes)?;
        let mut body = vec![0_u8; usize::from(header.body_len)];
        self.socket.read_exact(&mut body).map_err(|_| {
            session_error(ErrorCategory::VtmNegotiationFailed, "truncated VTM packet")
        })?;
        Ok(VtmPacket { header, body })
    }
}

fn connect_url(url: &SecretString, timeout: Duration) -> Result<TcpStream, AdapterError> {
    let parsed = Url::parse(url.expose_secret()).map_err(|_| {
        session_error(
            ErrorCategory::VtmNegotiationFailed,
            "invalid VTM stream URL",
        )
    })?;
    if parsed.scheme() != "ysproto" {
        return Err(session_error(
            ErrorCategory::VtmNegotiationFailed,
            "invalid VTM stream URL scheme",
        ));
    }
    let host = parsed.host_str().ok_or_else(|| {
        session_error(
            ErrorCategory::VtmNegotiationFailed,
            "missing VTM relay host",
        )
    })?;
    let port = parsed.port().ok_or_else(|| {
        session_error(
            ErrorCategory::VtmNegotiationFailed,
            "missing VTM relay port",
        )
    })?;
    let address = (host, port)
        .to_socket_addrs()
        .map_err(|_| session_error(ErrorCategory::VtmNegotiationFailed, "VTM DNS failed"))?
        .next()
        .ok_or_else(|| {
            session_error(
                ErrorCategory::VtmNegotiationFailed,
                "VTM relay did not resolve",
            )
        })?;
    let socket = TcpStream::connect_timeout(&address, timeout).map_err(|_| {
        session_error(
            ErrorCategory::VtmNegotiationFailed,
            "VTM relay connection failed",
        )
    })?;
    socket.set_read_timeout(Some(timeout)).map_err(|_| {
        session_error(
            ErrorCategory::VtmNegotiationFailed,
            "cannot configure VTM socket",
        )
    })?;
    socket.set_write_timeout(Some(timeout)).map_err(|_| {
        session_error(
            ErrorCategory::VtmNegotiationFailed,
            "cannot configure VTM socket",
        )
    })?;
    Ok(socket)
}

fn session_error(category: ErrorCategory, message: impl Into<String>) -> AdapterError {
    AdapterError::new(category, message)
}
