mod ffmpeg;
mod validation;

pub use ffmpeg::{FfmpegRecorder, ffmpeg_args, recording_name};
pub use validation::validate_recording;
