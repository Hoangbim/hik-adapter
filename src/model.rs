use serde::Serialize;

use crate::error::ErrorCategory;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct CameraKey {
    pub serial: String,
    pub channel: u16,
}

#[derive(Clone, Debug, Serialize)]
pub struct Camera {
    pub index: usize,
    pub name: String,
    pub device_name: String,
    pub serial: String,
    pub channel: u16,
}

impl Camera {
    pub fn key(&self) -> CameraKey {
        CameraKey {
            serial: self.serial.clone(),
            channel: self.channel,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraStatus {
    Pending,
    Resolving,
    Connecting,
    Recording,
    Validating,
    Completed,
    Failed,
}

/// Stream classification produced by the probe layer and by media transport
/// detection.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamClass {
    ClearH265,
    H264,
    MpegPs,
    Encrypted,
    Unknown,
}

impl StreamClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClearH265 => "clear_h265",
            Self::H264 => "h264",
            Self::MpegPs => "mpeg_ps",
            Self::Encrypted => "encrypted",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct RecordingInfo {
    pub file_name: String,
    pub playback_url: String,
    pub duration_seconds: f64,
    pub size_bytes: u64,
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub frame_rate: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct PublicError {
    pub category: ErrorCategory,
    pub message: String,
}
