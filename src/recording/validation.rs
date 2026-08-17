use std::{fs, path::Path, process::Command};

use serde::Deserialize;

use crate::{
    error::{AdapterError, ErrorCategory},
    model::RecordingInfo,
};

#[derive(Deserialize)]
struct ProbeOutput {
    format: ProbeFormat,
    streams: Vec<ProbeStream>,
}

#[derive(Deserialize)]
struct ProbeFormat {
    duration: String,
    size: String,
}

#[derive(Deserialize)]
struct ProbeStream {
    codec_name: String,
    codec_type: String,
    width: Option<u32>,
    height: Option<u32>,
    r_frame_rate: Option<String>,
}

pub fn validate_recording(
    part_path: &Path,
    ffprobe: &Path,
    ffmpeg: &Path,
) -> Result<RecordingInfo, AdapterError> {
    let probe = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration,size,format_name:stream=codec_name,codec_type,width,height,r_frame_rate",
            "-of",
            "json",
        ])
        .arg(part_path)
        .output()
        .map_err(|_| validation_error("cannot start ffprobe"))?;
    if !probe.status.success() {
        return Err(validation_error("ffprobe rejected recording"));
    }
    let info = parse_probe(&probe.stdout)?;
    let decode = Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(part_path)
        .args(["-frames:v", "1", "-f", "null", "-"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|_| validation_error("cannot start FFmpeg decode check"))?;
    if !decode.success() {
        return Err(validation_error("recording decode check failed"));
    }
    let final_path = final_path_for(part_path)?;
    if final_path.exists() {
        return Err(validation_error("final recording already exists"));
    }
    fs::rename(part_path, &final_path)
        .map_err(|_| validation_error("cannot publish recording atomically"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&final_path, fs::Permissions::from_mode(0o600))
            .map_err(|_| validation_error("cannot secure recording permissions"))?;
    }
    let file_name = final_path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| validation_error("invalid recording filename"))?
        .to_owned();
    Ok(RecordingInfo {
        playback_url: format!("/recordings/{file_name}"),
        file_name,
        duration_seconds: info.duration_seconds,
        size_bytes: info.size_bytes,
        codec: info.codec,
        width: info.width,
        height: info.height,
        frame_rate: info.frame_rate,
    })
}

struct ValidatedProbe {
    duration_seconds: f64,
    size_bytes: u64,
    codec: String,
    width: u32,
    height: u32,
    frame_rate: String,
}

fn parse_probe(bytes: &[u8]) -> Result<ValidatedProbe, AdapterError> {
    let probe: ProbeOutput =
        serde_json::from_slice(bytes).map_err(|_| validation_error("invalid ffprobe JSON"))?;
    let duration_seconds: f64 = probe
        .format
        .duration
        .parse()
        .map_err(|_| validation_error("invalid recording duration"))?;
    if !(25.0..=40.0).contains(&duration_seconds) {
        return Err(validation_error("recording duration outside 25-40 seconds"));
    }
    let size_bytes = probe
        .format
        .size
        .parse()
        .map_err(|_| validation_error("invalid recording size"))?;
    let stream = probe
        .streams
        .into_iter()
        .find(|stream| stream.codec_type == "video")
        .ok_or_else(|| validation_error("recording has no video stream"))?;
    if stream.codec_name != "hevc" {
        return Err(validation_error("recording video is not HEVC"));
    }
    let width = stream
        .width
        .filter(|value| *value > 0)
        .ok_or_else(|| validation_error("invalid video width"))?;
    let height = stream
        .height
        .filter(|value| *value > 0)
        .ok_or_else(|| validation_error("invalid video height"))?;
    let frame_rate = stream
        .r_frame_rate
        .filter(|value| value != "0/0" && !value.is_empty())
        .ok_or_else(|| validation_error("invalid video frame rate"))?;
    Ok(ValidatedProbe {
        duration_seconds,
        size_bytes,
        codec: stream.codec_name,
        width,
        height,
        frame_rate,
    })
}

fn final_path_for(part_path: &Path) -> Result<std::path::PathBuf, AdapterError> {
    let name = part_path
        .file_name()
        .and_then(|value| value.to_str())
        .and_then(|value| value.strip_suffix(".part.mp4"))
        .ok_or_else(|| validation_error("recording is not a .part.mp4 file"))?;
    Ok(part_path.with_file_name(format!("{name}.mp4")))
}

fn validation_error(message: impl Into<String>) -> AdapterError {
    AdapterError::new(ErrorCategory::ValidationFailed, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_expected_hevc_probe() {
        let info = parse_probe(include_bytes!("../../tests/fixtures/ffprobe_hevc.json")).unwrap();
        assert_eq!(info.codec, "hevc");
        assert_eq!((info.width, info.height), (1280, 720));
        assert_eq!(info.frame_rate, "20/1");
    }

    #[test]
    fn rejects_wrong_codec_and_duration() {
        let wrong_codec = br#"{"format":{"duration":"30","size":"10"},"streams":[{"codec_name":"h264","codec_type":"video","width":1280,"height":720,"r_frame_rate":"20/1"}]}"#;
        assert!(parse_probe(wrong_codec).is_err());
        let short = br#"{"format":{"duration":"2","size":"10"},"streams":[{"codec_name":"hevc","codec_type":"video","width":1280,"height":720,"r_frame_rate":"20/1"}]}"#;
        assert!(parse_probe(short).is_err());
    }
}
