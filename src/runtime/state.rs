use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

use serde::Serialize;

use crate::{
    error::{AdapterError, ErrorCategory},
    model::{Camera, CameraKey, CameraStatus, PublicError, RecordingInfo},
};

#[derive(Clone, Debug, Serialize)]
pub struct CameraRuntimeState {
    pub camera: Camera,
    pub status: CameraStatus,
    pub recording: Option<RecordingInfo>,
    pub error: Option<PublicError>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AdapterSnapshot {
    pub total: usize,
    pub completed: usize,
    pub failed: usize,
    pub cameras: Vec<CameraRuntimeState>,
}

#[derive(Clone, Default)]
pub struct AdapterState {
    entries: Arc<RwLock<BTreeMap<CameraKey, CameraRuntimeState>>>,
}

impl AdapterState {
    pub fn register(&self, cameras: &[Camera]) {
        let mut entries = self.entries.write().expect("adapter state poisoned");
        for camera in cameras {
            entries
                .entry(camera.key())
                .or_insert_with(|| CameraRuntimeState {
                    camera: camera.clone(),
                    status: CameraStatus::Pending,
                    recording: None,
                    error: None,
                });
        }
    }

    pub fn transition(&self, key: &CameraKey, status: CameraStatus) -> Result<(), AdapterError> {
        let mut entries = self.entries.write().expect("adapter state poisoned");
        let entry = entries.get_mut(key).ok_or_else(|| {
            AdapterError::new(ErrorCategory::ValidationFailed, "camera is not registered")
        })?;
        if matches!(entry.status, CameraStatus::Completed | CameraStatus::Failed) {
            return Err(AdapterError::new(
                ErrorCategory::ValidationFailed,
                "camera already reached terminal state",
            ));
        }
        entry.status = status;
        Ok(())
    }

    pub fn complete(&self, key: &CameraKey, recording: RecordingInfo) {
        if let Some(entry) = self
            .entries
            .write()
            .expect("adapter state poisoned")
            .get_mut(key)
        {
            entry.status = CameraStatus::Completed;
            entry.recording = Some(recording);
            entry.error = None;
        }
    }

    pub fn fail(&self, key: &CameraKey, error: PublicError) {
        if let Some(entry) = self
            .entries
            .write()
            .expect("adapter state poisoned")
            .get_mut(key)
        {
            entry.status = CameraStatus::Failed;
            entry.error = Some(error);
        }
    }

    pub fn snapshot(&self) -> AdapterSnapshot {
        let entries = self.entries.read().expect("adapter state poisoned");
        let cameras = entries.values().cloned().collect::<Vec<_>>();
        AdapterSnapshot {
            total: cameras.len(),
            completed: cameras
                .iter()
                .filter(|entry| entry.status == CameraStatus::Completed)
                .count(),
            failed: cameras
                .iter()
                .filter(|entry| entry.status == CameraStatus::Failed)
                .count(),
            cameras,
        }
    }

    pub fn recording(&self, file_name: &str) -> Option<RecordingInfo> {
        self.entries
            .read()
            .expect("adapter state poisoned")
            .values()
            .filter_map(|entry| entry.recording.as_ref())
            .find(|recording| recording.file_name == file_name)
            .cloned()
    }
}
