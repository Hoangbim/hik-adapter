use serde_json::Value;

use crate::{
    error::{AdapterError, ErrorCategory},
    model::Camera,
};

use super::{ApiClient, AuthSession};

const DEVICE_FILTER: &str =
    "TIME_PLAN,CONNECTION,SWITCH,STATUS,STATUS_EXT,WIFI,NODISTURB,P2P,KMS,HIDDNS";

impl ApiClient {
    pub fn list_cameras(&self, session: &AuthSession) -> Result<Vec<Camera>, AdapterError> {
        let mut cameras = Vec::new();
        let mut offset = 0_usize;
        let limit = 50_usize;
        loop {
            let payload = self.get_json(
                session,
                "/v3/userdevices/v1/devices/pagelist",
                &[
                    ("groupId", "-1".into()),
                    ("limit", limit.to_string()),
                    ("offset", offset.to_string()),
                    ("filter", DEVICE_FILTER.into()),
                ],
            )?;
            Self::require_meta_success(&payload)?;
            if let Some(devices) = payload.get("deviceInfos").and_then(Value::as_array) {
                for device in devices {
                    let Some(serial) = device
                        .get("deviceSerial")
                        .and_then(Value::as_str)
                        .filter(|v| !v.is_empty())
                    else {
                        continue;
                    };
                    let device_name = device.get("name").and_then(Value::as_str).unwrap_or(serial);
                    let mut channels = self.cameras_for_device(session, serial, device_name)?;
                    channels.sort_by_key(|camera| camera.channel);
                    cameras.extend(channels);
                }
            }
            let has_next = payload
                .pointer("/page/hasNext")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !has_next {
                break;
            }
            offset += limit;
        }
        for (index, camera) in cameras.iter_mut().enumerate() {
            camera.index = index + 1;
        }
        Ok(cameras)
    }

    fn cameras_for_device(
        &self,
        session: &AuthSession,
        serial: &str,
        device_name: &str,
    ) -> Result<Vec<Camera>, AdapterError> {
        let payload = self.get_json(
            session,
            "/v3/userdevices/v1/cameras/info",
            &[("deviceSerial", serial.to_owned())],
        )?;
        Self::require_meta_success(&payload)?;
        let mut cameras = Vec::new();
        for item in payload
            .get("cameraInfos")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(channel) = item.get("channelNo").and_then(Value::as_u64) else {
                continue;
            };
            let channel = u16::try_from(channel).map_err(|_| {
                AdapterError::new(ErrorCategory::ApiFailed, "camera channel is out of range")
            })?;
            cameras.push(Camera {
                index: 0,
                name: item
                    .get("cameraName")
                    .and_then(Value::as_str)
                    .unwrap_or("Camera")
                    .to_owned(),
                device_name: device_name.to_owned(),
                serial: serial.to_owned(),
                channel,
            });
        }
        Ok(cameras)
    }
}
