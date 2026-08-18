use crate::error::AdapterError;

#[derive(Default)]
pub struct TransportLock {
    locked: bool,
}

impl TransportLock {
    pub fn accept<'a>(&mut self, body: &'a [u8]) -> Result<Option<&'a [u8]>, AdapterError> {
        if self.locked {
            return Ok(Some(body));
        }
        let marker = [0x00, 0x00, 0x01, 0xba];
        let Some(offset) = body
            .windows(marker.len())
            .position(|window| window == marker)
        else {
            return Ok(None);
        };
        self.locked = true;
        Ok(Some(&body[offset..]))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RtpFields {
    pub payload_type: u8,
    pub marker: bool,
    pub sequence: u16,
    pub timestamp: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TransportGuess {
    pub rtp: Option<RtpFields>,
    pub ps_pack_start: bool,
    pub annex_b: bool,
    pub ts_sync: bool,
}

/// Classify the transport of a single stream-packet body. Diagnostic only:
/// used when dumping packets that failed to lock onto MPEG-PS.
pub fn classify(body: &[u8]) -> TransportGuess {
    let rtp = if body.len() >= 12 && body[0] >> 6 == 2 {
        Some(RtpFields {
            payload_type: body[1] & 0x7f,
            marker: body[1] & 0x80 != 0,
            sequence: u16::from_be_bytes([body[2], body[3]]),
            timestamp: u32::from_be_bytes([body[4], body[5], body[6], body[7]]),
        })
    } else {
        None
    };
    TransportGuess {
        rtp,
        ps_pack_start: body
            .windows(4)
            .any(|window| window == [0x00, 0x00, 0x01, 0xba]),
        annex_b: body
            .windows(4)
            .any(|window| window == [0x00, 0x00, 0x00, 0x01])
            || body.windows(3).any(|window| window == [0x00, 0x00, 0x01]),
        ts_sync: body.first() == Some(&0x47) && body.len().is_multiple_of(188),
    }
}

pub fn hex_prefix(bytes: &[u8], limit: usize) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(limit.min(bytes.len()) + 1);
    for byte in bytes.iter().take(limit) {
        parts.push(format!("{byte:02x}"));
    }
    let mut output = parts.join(" ");
    if bytes.len() > limit {
        output.push_str(" ...");
    }
    output
}
