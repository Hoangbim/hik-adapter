use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, atomic::AtomicBool},
    thread::{self, JoinHandle, ThreadId},
    time::Duration,
};

use crate::{
    api::{ApiClient, AuthSession},
    config::RecordingConfig,
    error::{AdapterError, ErrorCategory},
    model::{Camera, CameraKey, CameraStatus, PublicError, RecordingInfo},
    recording::{FfmpegRecorder, validate_recording},
    vtm::VtmSession,
};

use super::AdapterState;

pub trait CameraJob: Send + Sync + 'static {
    fn run(&self, camera: Camera, shutdown: Arc<AtomicBool>)
    -> Result<RecordingInfo, AdapterError>;
}

pub struct WorkerResult {
    pub camera: CameraKey,
    pub thread_id: ThreadId,
    pub thread_name: String,
    pub result: Result<RecordingInfo, PublicError>,
}

pub fn spawn_camera_workers<J: CameraJob>(
    cameras: Vec<Camera>,
    state: AdapterState,
    job: J,
    shutdown: Arc<AtomicBool>,
) -> Result<Vec<JoinHandle<WorkerResult>>, AdapterError> {
    if cameras.len() > 10 {
        return Err(AdapterError::new(
            ErrorCategory::ConfigurationInvalid,
            "at most 10 camera workers are allowed",
        ));
    }
    state.register(&cameras);
    let job = Arc::new(job);
    let mut handles = Vec::with_capacity(cameras.len());
    for camera in cameras {
        let state = state.clone();
        let job = Arc::clone(&job);
        let shutdown = Arc::clone(&shutdown);
        let key = camera.key();
        let thread_name = format!(
            "hik-vtm-{}-{}",
            sanitize_name(&camera.serial),
            camera.channel
        );
        let handle = thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                let thread = thread::current();
                let thread_id = thread.id();
                let thread_name = thread.name().unwrap_or("hik-vtm-worker").to_owned();
                let result = catch_unwind(AssertUnwindSafe(|| job.run(camera, shutdown)))
                    .map_err(|_| {
                        AdapterError::new(ErrorCategory::ValidationFailed, "camera worker panicked")
                    })
                    .and_then(|result| result)
                    .map_err(public_error);
                match &result {
                    Ok(recording) => state.complete(&key, recording.clone()),
                    Err(error) => state.fail(&key, error.clone()),
                }
                WorkerResult {
                    camera: key,
                    thread_id,
                    thread_name,
                    result,
                }
            })
            .map_err(|_| {
                AdapterError::new(
                    ErrorCategory::ValidationFailed,
                    "cannot spawn camera worker",
                )
            })?;
        handles.push(handle);
    }
    Ok(handles)
}

pub fn join_workers(handles: Vec<JoinHandle<WorkerResult>>) -> Vec<WorkerResult> {
    handles
        .into_iter()
        .filter_map(|handle| handle.join().ok())
        .collect()
}

pub struct VtmCameraJob {
    pub api: ApiClient,
    pub session: AuthSession,
    pub config: RecordingConfig,
    pub state: AdapterState,
    pub socket_timeout: Duration,
    pub no_media_timeout: Duration,
}

impl CameraJob for VtmCameraJob {
    fn run(
        &self,
        camera: Camera,
        shutdown: Arc<AtomicBool>,
    ) -> Result<RecordingInfo, AdapterError> {
        let key = camera.key();
        self.state.transition(&key, CameraStatus::Resolving)?;
        let context = self.api.resolve_vtm(&self.session, &camera)?;
        self.state.transition(&key, CameraStatus::Connecting)?;
        let mut vtm = VtmSession::connect(context, self.socket_timeout)?;
        vtm.start(3)?;
        let mut recorder = FfmpegRecorder::start(&self.config, &camera)?;
        self.state.transition(&key, CameraStatus::Recording)?;
        vtm.for_each_media(
            Duration::from_secs(self.config.duration_seconds),
            self.no_media_timeout,
            &shutdown,
            |body| recorder.write_media(body),
        )?;
        let part_path = recorder.finish(Duration::from_secs(15))?;
        self.state.transition(&key, CameraStatus::Validating)?;
        validate_recording(
            &part_path,
            &self.config.ffprobe_path,
            &self.config.ffmpeg_path,
        )
    }
}

fn public_error(error: AdapterError) -> PublicError {
    let message = match error.category {
        ErrorCategory::AuthenticationFailed => "authentication failed",
        ErrorCategory::CaptchaRequired => "CAPTCHA required",
        ErrorCategory::CameraOffline => "camera is offline",
        ErrorCategory::PermissionDenied => "camera permission denied",
        ErrorCategory::VtmResourceMissing => "VTM resource is unavailable",
        ErrorCategory::VtmTokenFailed => "VTM token acquisition failed",
        ErrorCategory::VtmNegotiationFailed => "VTM negotiation failed",
        ErrorCategory::VtmRedirectLimit => "VTM redirect limit exceeded",
        ErrorCategory::EncryptedStream => "encrypted VTM stream is unsupported",
        ErrorCategory::NoMedia => "VTM stream returned no media",
        ErrorCategory::UnsupportedTransport => "stream transport is unsupported",
        ErrorCategory::FfmpegFailed => "FFmpeg recording failed",
        ErrorCategory::ValidationFailed => "recording validation failed",
        ErrorCategory::Cancelled => "recording cancelled",
        ErrorCategory::ConfigurationInvalid => "configuration is invalid",
        ErrorCategory::ApiFailed => "Hik-Connect API request failed",
    }
    .to_owned();
    PublicError {
        category: error.category,
        message,
    }
}

fn sanitize_name(serial: &str) -> String {
    serial
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        .take(24)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    struct FakeJob;

    impl CameraJob for FakeJob {
        fn run(
            &self,
            camera: Camera,
            _shutdown: Arc<AtomicBool>,
        ) -> Result<RecordingInfo, AdapterError> {
            if camera.index == 4 {
                return Err(AdapterError::new(
                    ErrorCategory::VtmTokenFailed,
                    "secret-token-sentinel",
                ));
            }
            Ok(RecordingInfo {
                file_name: format!("{}.mp4", camera.index),
                playback_url: format!("/recordings/{}.mp4", camera.index),
                duration_seconds: 30.0,
                size_bytes: 1,
                codec: "hevc".to_owned(),
                width: 1280,
                height: 720,
                frame_rate: "20/1".to_owned(),
            })
        }
    }

    fn cameras(count: usize) -> Vec<Camera> {
        (1..=count)
            .map(|index| Camera {
                index,
                name: format!("Camera {index}"),
                device_name: "Device".to_owned(),
                serial: format!("SERIAL-{index}"),
                channel: 1,
            })
            .collect()
    }

    #[test]
    fn ten_workers_use_ten_os_threads_and_isolate_failure() {
        let state = AdapterState::default();
        let handles = spawn_camera_workers(
            cameras(10),
            state.clone(),
            FakeJob,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let joined = join_workers(handles);
        assert_eq!(joined.len(), 10);
        assert_eq!(
            joined
                .iter()
                .map(|result| result.thread_id)
                .collect::<HashSet<_>>()
                .len(),
            10
        );
        assert!(
            joined
                .iter()
                .all(|result| result.thread_name.starts_with("hik-vtm-"))
        );
        assert_eq!(state.snapshot().completed, 9);
        assert_eq!(state.snapshot().failed, 1);
        assert!(
            !serde_json::to_string(&state.snapshot())
                .unwrap()
                .contains("secret-token-sentinel")
        );
    }

    #[test]
    fn refuses_more_than_ten_workers() {
        assert!(
            spawn_camera_workers(
                cameras(11),
                AdapterState::default(),
                FakeJob,
                Arc::new(AtomicBool::new(false)),
            )
            .is_err()
        );
    }
}
