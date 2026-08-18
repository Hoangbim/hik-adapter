use std::{
    sync::Arc,
    thread::{self, JoinHandle},
    time::Duration,
};

use crate::{
    api::{ApiClient, AuthSession},
    error::AdapterError,
    model::{Camera, CameraKey, StreamClass},
    vtm::VtmSession,
};

pub struct StreamProbeJob {
    pub api: ApiClient,
    pub session: AuthSession,
    pub socket_timeout: Duration,
    pub probe_timeout: Duration,
}

pub struct ProbeOutcome {
    pub camera: CameraKey,
    pub stream_class: StreamClass,
}

fn probe_camera(
    api: &ApiClient,
    session: &AuthSession,
    camera: &Camera,
    socket_timeout: Duration,
    probe_timeout: Duration,
) -> StreamClass {
    let Ok(context) = api.resolve_vtm(session, camera) else {
        return StreamClass::Unknown;
    };
    let Ok(mut vtm) = VtmSession::connect(context, socket_timeout) else {
        return StreamClass::Unknown;
    };
    if vtm.start(3).is_err() {
        return StreamClass::Unknown;
    }
    vtm.probe_transport(probe_timeout).unwrap_or(StreamClass::Unknown)
}

/// Open a short VTM session per camera and classify the media transport by
/// sniffing the first stream packets. One OS thread per camera, mirroring
/// the recording worker pattern.
pub fn spawn_stream_probes(
    cameras: Vec<Camera>,
    job: Arc<StreamProbeJob>,
) -> Result<Vec<JoinHandle<ProbeOutcome>>, AdapterError> {
    let mut handles = Vec::with_capacity(cameras.len());
    for camera in cameras {
        let job = Arc::clone(&job);
        let key = camera.key();
        let handle = thread::Builder::new()
            .name(format!("hik-probe-{}", camera.index))
            .spawn(move || ProbeOutcome {
                camera: key,
                stream_class: probe_camera(
                    &job.api,
                    &job.session,
                    &camera,
                    job.socket_timeout,
                    job.probe_timeout,
                ),
            })
            .map_err(|_| {
                AdapterError::new(crate::error::ErrorCategory::ValidationFailed, "cannot spawn probe")
            })?;
        handles.push(handle);
    }
    Ok(handles)
}

pub fn join_probe_handles(handles: Vec<JoinHandle<ProbeOutcome>>) -> Vec<ProbeOutcome> {
    handles
        .into_iter()
        .filter_map(|handle| handle.join().ok())
        .collect()
}
