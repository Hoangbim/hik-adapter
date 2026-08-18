use crate::{
    error::{AdapterError, ErrorCategory},
    model::StreamClass,
};

use super::transport::TransportLock;

const ANNEX_B_START_CODE: [u8; 4] = [0x00, 0x00, 0x00, 0x01];

/// 12-byte private container prefix used by encrypted streams (newer
/// AcuSense NVR firmware): parameter sets and IDR fragments arrive wrapped
/// in this container with transformed payloads that FFmpeg cannot parse.
const ENCRYPTED_CONTAINER_PREFIX: [u8; 12] =
    [0x40, 0x00, 0x00, 0x02, 0x80, 0x06, 0x00, 0x01, 0x11, 0x21, 0x02, 0x01];

/// Wire transport locked for the session, and the FFmpeg demuxer that must
/// consume it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaTransport {
    /// MPEG program stream fragments, fed to FFmpeg with `-f mpeg`.
    MpegPs,
    /// Raw H.265 NAL units carried in RTP payload type 96, fed with `-f hevc`.
    RtpH265,
    /// Raw H.264 NAL units carried in RTP payload type 96, fed with `-f h264`.
    RtpH264,
}

/// Reassembles raw H.265 NAL units from RTP payloads.
///
/// Complete NAL units pass through unchanged. Fragmented NAL units (type 49,
/// FD_FU) are buffered across packets and emitted once the end fragment
/// arrives. The relay does not repeat the inner NAL header on start
/// fragments, so it is reconstructed from the FU type field.
#[derive(Default)]
pub struct H265Assembler {
    fu_buffer: Option<Vec<u8>>,
}

impl H265Assembler {
    /// Accept one RTP payload (header already stripped). Returns the complete
    /// NAL unit when one becomes available.
    pub fn accept(&mut self, payload: &[u8]) -> Option<Vec<u8>> {
        if payload.len() < 2 || payload[0] & 0x80 != 0 {
            return None; // truncated payload or forbidden NAL bit set
        }
        let header = u16::from_be_bytes([payload[0], payload[1]]);
        let nal_type = (header >> 9) & 0x3f;
        if nal_type != 49 {
            return Some(payload.to_vec());
        }
        if payload.len() < 3 {
            return None;
        }
        let fu = payload[2];
        let start = fu & 0x80 != 0;
        let end = fu & 0x40 != 0;
        let fu_type = fu & 0x3f;
        if start {
            let mut buffer = Vec::with_capacity(16 * 1024);
            let inner_header = (fu_type as u16) << 9 | (header & 0x7);
            buffer.extend_from_slice(&inner_header.to_be_bytes());
            // Some encoders do include the inner NAL header on start
            // fragments; skip it when it is valid and matches the FU type.
            let fragment = if payload.len() >= 5 {
                let inner = u16::from_be_bytes([payload[3], payload[4]]);
                if payload[3] & 0x80 == 0 && (inner >> 9) & 0x3f == u16::from(fu_type) {
                    &payload[5..]
                } else {
                    &payload[3..]
                }
            } else {
                &payload[3..]
            };
            buffer.extend_from_slice(fragment);
            self.fu_buffer = Some(buffer);
            None
        } else if end {
            let Some(mut buffer) = self.fu_buffer.take() else {
                return None; // end fragment without a start; drop it
            };
            buffer.extend_from_slice(&payload[3..]);
            Some(buffer)
        } else if let Some(buffer) = self.fu_buffer.as_mut() {
            buffer.extend_from_slice(&payload[3..]);
            None
        } else {
            None // middle fragment without a start; drop it
        }
    }
}

/// Reassembles raw H.264 NAL units from RTP payloads (RFC 6184).
#[derive(Default)]
pub struct H264Assembler {
    fu_buffer: Option<Vec<u8>>,
}

impl H264Assembler {
    /// Accept one RTP payload (header already stripped). Returns the complete
    /// NAL unit when one becomes available.
    pub fn accept(&mut self, payload: &[u8]) -> Option<Vec<u8>> {
        if payload.is_empty() || payload[0] & 0x80 != 0 {
            return None;
        }
        let nal_type = payload[0] & 0x1f;
        if nal_type != 28 {
            return Some(payload.to_vec()); // complete NAL (FU-A is type 28)
        }
        if payload.len() < 2 {
            return None;
        }
        let fu = payload[1];
        let start = fu & 0x80 != 0;
        let end = fu & 0x40 != 0;
        let fu_type = fu & 0x1f;
        if start {
            let mut buffer = Vec::with_capacity(16 * 1024);
            buffer.push((payload[0] & 0xe0) | fu_type);
            buffer.extend_from_slice(&payload[2..]);
            self.fu_buffer = Some(buffer);
            None
        } else if end {
            let Some(mut buffer) = self.fu_buffer.take() else {
                return None; // end fragment without a start; drop it
            };
            buffer.extend_from_slice(&payload[2..]);
            Some(buffer)
        } else if let Some(buffer) = self.fu_buffer.as_mut() {
            buffer.extend_from_slice(&payload[2..]);
            None
        } else {
            None // middle fragment without a start; drop it
        }
    }
}

/// Detects and locks the media transport from the first stream packets, then
/// converts subsequent packets into recordable media chunks.
#[derive(Default)]
pub struct MediaPipe {
    transport: Option<MediaTransport>,
    ps: TransportLock,
    h265: H265Assembler,
    h264: H264Assembler,
}

impl MediaPipe {
    pub fn transport(&self) -> Option<MediaTransport> {
        self.transport
    }

    /// Accept one VTM stream-packet body. Returns a media chunk ready for the
    /// recorder, or None while no media could be extracted (not yet locked,
    /// metadata payload type, or an incomplete FU fragment). Encrypted
    /// streams produce an error so the caller fails fast.
    pub fn accept(&mut self, body: &[u8]) -> Result<Option<(MediaTransport, Vec<u8>)>, AdapterError> {
        match self.transport {
            Some(MediaTransport::MpegPs) => Ok(self
                .ps
                .accept(body)?
                .map(|media| (MediaTransport::MpegPs, media.to_vec()))),
            Some(MediaTransport::RtpH265) => {
                let media = rtp_payload(body)
                    .filter(|_| rtp_payload_type(body) == Some(96))
                    .and_then(|payload| self.h265.accept(payload))
                    .map(annex_b);
                Ok(media.map(|media| (MediaTransport::RtpH265, media)))
            }
            Some(MediaTransport::RtpH264) => {
                let media = rtp_payload(body)
                    .filter(|_| rtp_payload_type(body) == Some(96))
                    .and_then(|payload| self.h264.accept(payload))
                    .map(annex_b);
                Ok(media.map(|media| (MediaTransport::RtpH264, media)))
            }
            None => {
                // Keep the MPEG-PS path first: it is the existing, working
                // transport for older devices.
                if body.windows(4).any(|window| window == [0x00, 0x00, 0x01, 0xba]) {
                    self.transport = Some(MediaTransport::MpegPs);
                    let media = self.ps.accept(body)?;
                    return Ok(media.map(|media| (MediaTransport::MpegPs, media.to_vec())));
                }
                if rtp_payload_type(body) == Some(96) {
                    let payload = rtp_payload(body).expect("type matched");
                    if payload.starts_with(&ENCRYPTED_CONTAINER_PREFIX) {
                        return Err(AdapterError::new(
                            ErrorCategory::EncryptedStream,
                            "encrypted VTM stream is unsupported",
                        ));
                    }
                    if let Some(transport) = detect_h265(payload).or_else(|| detect_h264(payload)) {
                        self.transport = Some(transport);
                        let media = match transport {
                            MediaTransport::RtpH265 => self.h265.accept(payload).map(annex_b),
                            MediaTransport::RtpH264 => self.h264.accept(payload).map(annex_b),
                            MediaTransport::MpegPs => unreachable!("detection never returns PS"),
                        };
                        return Ok(media.map(|media| (transport, media)));
                    }
                }
                Ok(None)
            }
        }
    }
}

pub fn rtp_payload(body: &[u8]) -> Option<&[u8]> {
    (body.len() >= 12 && body[0] >> 6 == 2).then(|| &body[12..])
}

pub fn rtp_payload_type(body: &[u8]) -> Option<u8> {
    (body.len() >= 12 && body[0] >> 6 == 2).then(|| body[1] & 0x7f)
}

/// Returns H.265 when the payload starts with a NAL header unique to the
/// clear H.265 streams the relay sends: VPS/SPS/PPS (32/33/34), FU (49), or
/// TRAIL_R (1). Slices, SEI, and AUD types are deliberately excluded because
/// their leading bytes collide with H.264 NAL headers; the lock simply waits
/// for the next parameter set (they repeat every few seconds).
fn detect_h265(payload: &[u8]) -> Option<MediaTransport> {
    if payload.len() < 2 || payload[0] & 0x80 != 0 {
        return None;
    }
    let header = u16::from_be_bytes([payload[0], payload[1]]);
    let nal_type = (header >> 9) & 0x3f;
    matches!(nal_type, 32 | 33 | 34 | 49 | 1).then_some(MediaTransport::RtpH265)
}

/// Returns H.264 when the payload starts with a single-byte H.264 NAL header
/// (SPS/PPS/IDR/slices/SEI/AUD/FU-A).
fn detect_h264(payload: &[u8]) -> Option<MediaTransport> {
    if payload.is_empty() || payload[0] & 0x80 != 0 {
        return None;
    }
    matches!(payload[0] & 0x1f, 1 | 5 | 6 | 7 | 8 | 9 | 28).then_some(MediaTransport::RtpH264)
}

/// Classify a single stream-packet body for the probe layer.
pub fn classify_stream(body: &[u8]) -> Option<StreamClass> {
    if body.windows(4).any(|window| window == [0x00, 0x00, 0x01, 0xba]) {
        return Some(StreamClass::MpegPs);
    }
    if rtp_payload_type(body) != Some(96) {
        return None;
    }
    let payload = rtp_payload(body)?;
    if payload.starts_with(&ENCRYPTED_CONTAINER_PREFIX) {
        return Some(StreamClass::Encrypted);
    }
    if detect_h265(payload).is_some() {
        return Some(StreamClass::ClearH265);
    }
    if detect_h264(payload).is_some() {
        return Some(StreamClass::H264);
    }
    None
}

fn annex_b(nal: Vec<u8>) -> Vec<u8> {
    let mut chunk = Vec::with_capacity(nal.len() + ANNEX_B_START_CODE.len());
    chunk.extend_from_slice(&ANNEX_B_START_CODE);
    chunk.extend_from_slice(&nal);
    chunk
}
