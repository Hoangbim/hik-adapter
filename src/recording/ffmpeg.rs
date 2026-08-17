use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::{
    config::RecordingConfig,
    error::{AdapterError, ErrorCategory},
    model::Camera,
};

pub struct FfmpegRecorder {
    child: Child,
    stdin: Option<ChildStdin>,
    stderr_reader: Option<JoinHandle<Vec<u8>>>,
    part_path: PathBuf,
}

impl FfmpegRecorder {
    pub fn start(config: &RecordingConfig, camera: &Camera) -> Result<Self, AdapterError> {
        fs::create_dir_all(&config.output_dir)
            .map_err(|_| ffmpeg_error("cannot create output directory"))?;
        let final_path = config
            .output_dir
            .join(recording_name(&camera.serial, camera.channel));
        let part_path = part_path_for(&final_path)?;
        if final_path.exists() || part_path.exists() {
            return Err(AdapterError::new(
                ErrorCategory::ValidationFailed,
                "recording output already exists",
            ));
        }
        let mut child = Command::new(&config.ffmpeg_path)
            .args(ffmpeg_args(&part_path))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| ffmpeg_error("cannot start FFmpeg"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ffmpeg_error("FFmpeg stdin unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| ffmpeg_error("FFmpeg stderr unavailable"))?;
        let stderr_reader = thread::spawn(move || {
            let mut reader = stderr.take(8192);
            let mut output = Vec::new();
            let _ = reader.read_to_end(&mut output);
            output
        });
        Ok(Self {
            child,
            stdin: Some(stdin),
            stderr_reader: Some(stderr_reader),
            part_path,
        })
    }

    pub fn write_media(&mut self, body: &[u8]) -> Result<(), AdapterError> {
        self.stdin
            .as_mut()
            .ok_or_else(|| ffmpeg_error("FFmpeg input is closed"))?
            .write_all(body)
            .map_err(|_| ffmpeg_error("FFmpeg rejected MPEG-PS input"))
    }

    pub fn finish(mut self, timeout: Duration) -> Result<PathBuf, AdapterError> {
        self.stdin.take();
        let deadline = Instant::now() + timeout;
        let status = loop {
            if let Some(status) = self
                .child
                .try_wait()
                .map_err(|_| ffmpeg_error("cannot inspect FFmpeg process"))?
            {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                return Err(ffmpeg_error("FFmpeg finalization timed out"));
            }
            thread::sleep(Duration::from_millis(25));
        };
        let stderr = self
            .stderr_reader
            .take()
            .and_then(|handle| handle.join().ok())
            .unwrap_or_default();
        if !status.success() {
            let detail = String::from_utf8_lossy(&stderr)
                .replace(['\r', '\n'], " ")
                .chars()
                .take(500)
                .collect::<String>();
            return Err(ffmpeg_error(if detail.is_empty() {
                "FFmpeg exited unsuccessfully".to_owned()
            } else {
                format!("FFmpeg exited unsuccessfully: {detail}")
            }));
        }
        Ok(self.part_path.clone())
    }
}

impl Drop for FfmpegRecorder {
    fn drop(&mut self) {
        self.stdin.take();
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

pub fn ffmpeg_args(output: &Path) -> Vec<String> {
    [
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "mpeg",
        "-i",
        "pipe:0",
        "-map",
        "0:v:0",
        "-c:v",
        "copy",
        "-an",
        "-movflags",
        "+faststart",
    ]
    .into_iter()
    .map(str::to_owned)
    .chain(std::iter::once(output.to_string_lossy().into_owned()))
    .collect()
}

pub fn recording_name(serial: &str, channel: u16) -> String {
    let safe = serial
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches(['.', '_'])
        .to_owned();
    let safe = if safe.is_empty() {
        "camera"
    } else {
        safe.as_str()
    };
    format!("{safe}-{channel}.mp4")
}

fn part_path_for(final_path: &Path) -> Result<PathBuf, AdapterError> {
    let stem = final_path
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| ffmpeg_error("invalid recording output name"))?;
    Ok(final_path.with_file_name(format!("{stem}.part.mp4")))
}

fn ffmpeg_error(message: impl Into<String>) -> AdapterError {
    AdapterError::new(ErrorCategory::FfmpegFailed, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffmpeg_command_preserves_video_and_drops_audio() {
        assert_eq!(
            ffmpeg_args(Path::new("SERIAL-1.part.mp4")),
            [
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "mpeg",
                "-i",
                "pipe:0",
                "-map",
                "0:v:0",
                "-c:v",
                "copy",
                "-an",
                "-movflags",
                "+faststart",
                "SERIAL-1.part.mp4",
            ]
        );
    }

    #[test]
    fn output_name_sanitizes_serial() {
        assert_eq!(recording_name("../GK/8814", 1), "GK_8814-1.mp4");
    }
}
