use std::fmt;

use secrecy::{ExposeSecret, SecretString};

use crate::error::{AdapterError, ErrorCategory};

pub const VTM_MAGIC: u8 = 0x24;
pub const VTM_HEADER_SIZE: usize = 8;
pub const CLIENT_VERSION: &str = "v3.6.3.20221124";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Channel {
    Message = 0x00,
    Stream = 0x01,
    EncryptedMessage = 0x0a,
    EncryptedStream = 0x0b,
}

impl TryFrom<u8> for Channel {
    type Error = AdapterError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(Self::Message),
            0x01 => Ok(Self::Stream),
            0x0a => Ok(Self::EncryptedMessage),
            0x0b => Ok(Self::EncryptedStream),
            _ => Err(protocol_error("unknown VTM channel")),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum MessageCode {
    KeepAliveReq = 0x0132,
    KeepAliveRsp = 0x0133,
    StreamInfoReq = 0x013b,
    StreamInfoRsp = 0x013c,
    StreamInfoNotify = 0x013d,
}

impl TryFrom<u16> for MessageCode {
    type Error = AdapterError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            0x0132 => Ok(Self::KeepAliveReq),
            0x0133 => Ok(Self::KeepAliveRsp),
            0x013b => Ok(Self::StreamInfoReq),
            0x013c => Ok(Self::StreamInfoRsp),
            0x013d => Ok(Self::StreamInfoNotify),
            _ => Err(protocol_error("unknown VTM message code")),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VtmHeader {
    pub channel: Channel,
    pub body_len: u16,
    pub sequence: u16,
    pub code: MessageCode,
}

#[derive(Clone)]
pub struct VtmPacket {
    pub header: VtmHeader,
    pub body: Vec<u8>,
}

impl fmt::Debug for VtmPacket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VtmPacket")
            .field("header", &self.header)
            .field("body_len", &self.body.len())
            .finish()
    }
}

impl VtmPacket {
    pub fn new(channel: Channel, code: MessageCode, sequence: u16, body: Vec<u8>) -> Self {
        Self {
            header: VtmHeader {
                channel,
                body_len: body.len().try_into().unwrap_or(u16::MAX),
                sequence,
                code,
            },
            body,
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>, AdapterError> {
        let body_len: u16 = self
            .body
            .len()
            .try_into()
            .map_err(|_| protocol_error("VTM packet body exceeds 65535 bytes"))?;
        let mut bytes = Vec::with_capacity(VTM_HEADER_SIZE + self.body.len());
        bytes.push(VTM_MAGIC);
        bytes.push(self.header.channel as u8);
        bytes.extend_from_slice(&body_len.to_be_bytes());
        bytes.extend_from_slice(&self.header.sequence.to_be_bytes());
        bytes.extend_from_slice(&(self.header.code as u16).to_be_bytes());
        bytes.extend_from_slice(&self.body);
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, AdapterError> {
        if bytes.len() < VTM_HEADER_SIZE {
            return Err(protocol_error("truncated VTM header"));
        }
        let header = decode_header(bytes[..VTM_HEADER_SIZE].try_into().expect("fixed size"))?;
        if bytes.len() != VTM_HEADER_SIZE + usize::from(header.body_len) {
            return Err(protocol_error("VTM body length mismatch"));
        }
        Ok(Self {
            header,
            body: bytes[VTM_HEADER_SIZE..].to_vec(),
        })
    }
}

pub fn decode_header(bytes: [u8; VTM_HEADER_SIZE]) -> Result<VtmHeader, AdapterError> {
    if bytes[0] != VTM_MAGIC {
        return Err(protocol_error("invalid VTM magic"));
    }
    Ok(VtmHeader {
        channel: Channel::try_from(bytes[1])?,
        body_len: u16::from_be_bytes([bytes[2], bytes[3]]),
        sequence: u16::from_be_bytes([bytes[4], bytes[5]]),
        code: MessageCode::try_from(u16::from_be_bytes([bytes[6], bytes[7]]))?,
    })
}

#[derive(Clone)]
pub struct StreamInfoResponse {
    pub result: Option<u64>,
    pub stream_session: Option<String>,
    pub redirect_key: Option<SecretString>,
    pub redirect_url: Option<SecretString>,
}

impl fmt::Debug for StreamInfoResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamInfoResponse")
            .field("result", &self.result)
            .field(
                "stream_session",
                &self.stream_session.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "redirect_key",
                &self.redirect_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "redirect_url",
                &self.redirect_url.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

pub fn build_stream_info_request(
    stream_url: &SecretString,
    redirect_key: Option<&SecretString>,
) -> Vec<u8> {
    let mut body = Vec::new();
    encode_bytes_field(1, stream_url.expose_secret().as_bytes(), &mut body);
    if let Some(key) = redirect_key {
        encode_bytes_field(2, key.expose_secret().as_bytes(), &mut body);
    }
    encode_bytes_field(3, CLIENT_VERSION.as_bytes(), &mut body);
    encode_varint_field(4, 0, &mut body);
    encode_bytes_field(6, CLIENT_VERSION.as_bytes(), &mut body);
    body
}

pub fn build_keepalive_request(stream_session: &str) -> Vec<u8> {
    let mut body = Vec::new();
    encode_bytes_field(1, stream_session.as_bytes(), &mut body);
    body
}

pub fn parse_stream_info_response(body: &[u8]) -> Result<StreamInfoResponse, AdapterError> {
    let mut offset = 0;
    let mut response = StreamInfoResponse {
        result: None,
        stream_session: None,
        redirect_key: None,
        redirect_url: None,
    };
    while offset < body.len() {
        let key = read_varint(body, &mut offset)?;
        let field = key >> 3;
        match key & 0x07 {
            0 => {
                let value = read_varint(body, &mut offset)?;
                if field == 1 {
                    response.result = Some(value);
                }
            }
            2 => {
                let len = read_varint(body, &mut offset)?;
                let len: usize = len
                    .try_into()
                    .map_err(|_| protocol_error("protobuf field length too large"))?;
                let end = offset
                    .checked_add(len)
                    .filter(|end| *end <= body.len())
                    .ok_or_else(|| protocol_error("protobuf field exceeds payload"))?;
                let value = &body[offset..end];
                offset = end;
                let text = || {
                    std::str::from_utf8(value)
                        .map(str::to_owned)
                        .map_err(|_| protocol_error("invalid protobuf UTF-8"))
                };
                match field {
                    4 => response.stream_session = Some(text()?),
                    5 => response.redirect_key = Some(SecretString::from(text()?)),
                    7 => response.redirect_url = Some(SecretString::from(text()?)),
                    _ => {}
                }
            }
            _ => return Err(protocol_error("unsupported protobuf wire type")),
        }
    }
    Ok(response)
}

fn encode_varint(mut value: u64, output: &mut Vec<u8>) {
    while value >= 0x80 {
        output.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    output.push(value as u8);
}

fn encode_varint_field(field: u64, value: u64, output: &mut Vec<u8>) {
    encode_varint(field << 3, output);
    encode_varint(value, output);
}

fn encode_bytes_field(field: u64, value: &[u8], output: &mut Vec<u8>) {
    encode_varint((field << 3) | 2, output);
    encode_varint(value.len() as u64, output);
    output.extend_from_slice(value);
}

fn read_varint(input: &[u8], offset: &mut usize) -> Result<u64, AdapterError> {
    let mut value = 0_u64;
    for shift in (0..70).step_by(7) {
        let byte = *input
            .get(*offset)
            .ok_or_else(|| protocol_error("truncated protobuf varint"))?;
        *offset += 1;
        if shift == 63 && byte > 1 {
            return Err(protocol_error("protobuf varint overflow"));
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(protocol_error("protobuf varint overflow"))
}

fn protocol_error(message: impl Into<String>) -> AdapterError {
    AdapterError::new(ErrorCategory::VtmNegotiationFailed, message)
}
