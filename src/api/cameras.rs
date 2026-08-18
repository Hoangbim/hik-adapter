use std::collections::{HashMap, HashSet};

use serde_json::Value;
use url::Url;

use crate::{
    error::{AdapterError, ErrorCategory},
    model::Camera,
};

use super::{ApiClient, AuthSession};

const DEVICE_FILTER: &str =
    "TIME_PLAN,CONNECTION,SWITCH,STATUS,STATUS_EXT,WIFI,NODISTURB,P2P,KMS,HIDDNS";
const SHARE_PAGE_LIMIT: usize = 1_000;

/// Per-device stream encryption information from the device pagelist.
#[derive(Clone, Debug)]
pub struct DeviceEncryptInfo {
    pub is_encrypt: bool,
    pub encrypt_pwd: Option<String>,
    pub kms_secret_key: Option<String>,
}

impl ApiClient {
    /// Walk the device pagelist and collect per-device stream encryption
    /// flags (`statusInfos.isEncrypt`) plus the key material the API exposes
    /// for encrypted streams. Covers devices bound to this account only;
    /// shared devices never appear in the pagelist and must be classified by
    /// sniffing the media stream instead.
    pub fn device_encrypt_flags(
        &self,
        session: &AuthSession,
    ) -> Result<HashMap<String, DeviceEncryptInfo>, AdapterError> {
        let mut flags = HashMap::new();
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
            let serials = payload
                .get("deviceInfos")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|device| device.get("deviceSerial").and_then(Value::as_str))
                .map(str::to_owned)
                .collect::<Vec<_>>();
            for serial in serials {
                let status = payload
                    .pointer(&format!("/statusInfos/{serial}"))
                    .and_then(Value::as_object);
                let kms = payload
                    .pointer(&format!("/kmsInfos/{serial}"))
                    .and_then(Value::as_object);
                flags.insert(
                    serial,
                    DeviceEncryptInfo {
                        is_encrypt: status
                            .and_then(|value| value.get("isEncrypt"))
                            .and_then(Value::as_u64)
                            == Some(1),
                        encrypt_pwd: status
                            .and_then(|value| value.get("encryptPwd"))
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        kms_secret_key: kms
                            .and_then(|value| value.get("secretKey"))
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    },
                );
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
        Ok(flags)
    }

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

    pub fn list_shared_serials(
        &self,
        session: &AuthSession,
        web_base_url: &str,
    ) -> Result<Vec<String>, AdapterError> {
        let base_url = Url::parse(web_base_url).map_err(|_| {
            AdapterError::new(
                ErrorCategory::ConfigurationInvalid,
                "invalid Hik-Connect web base URL",
            )
        })?;
        let endpoint = Self::endpoint(&base_url, "/v3/hcweb/share/domains")?;
        let mut serials = Vec::new();
        let mut seen = HashSet::new();
        let mut offset = 0_usize;
        loop {
            let response = self
                .http
                .post(endpoint.clone())
                .headers(self.headers(Some(session))?)
                .header("clientsource", "0")
                .header("customno", "1000002")
                .form(&[
                    ("limit", SHARE_PAGE_LIMIT.to_string()),
                    ("offset", offset.to_string()),
                ])
                .send()
                .map_err(|_| {
                    AdapterError::new(
                        ErrorCategory::ApiFailed,
                        "Hik-Connect shared-domain request failed",
                    )
                })?;
            if !response.status().is_success() {
                return Err(AdapterError::new(
                    ErrorCategory::ApiFailed,
                    format!(
                        "Hik-Connect shared-domain HTTP status {}",
                        response.status().as_u16()
                    ),
                ));
            }
            let payload: Value = response.json().map_err(|_| {
                AdapterError::new(
                    ErrorCategory::ApiFailed,
                    "invalid Hik-Connect shared-domain response",
                )
            })?;
            Self::require_meta_success(&payload)?;
            if payload.pointer("/data/success").and_then(Value::as_bool) != Some(true) {
                let code = payload
                    .pointer("/data/resultCode")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                return Err(AdapterError::new(
                    ErrorCategory::ApiFailed,
                    format!("Hik-Connect shared-domain result code {code}"),
                ));
            }
            let rows = payload
                .pointer("/data/data")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    AdapterError::new(
                        ErrorCategory::ApiFailed,
                        "shared-domain response is missing data",
                    )
                })?;
            for row in rows {
                let Some(serial) = row
                    .get("subSerial")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                else {
                    continue;
                };
                if seen.insert(serial.to_owned()) {
                    serials.push(serial.to_owned());
                }
            }
            let received = rows.len();
            if received < SHARE_PAGE_LIMIT {
                break;
            }
            offset += received;
        }
        Ok(serials)
    }

    pub fn list_cameras_with_shared(
        &self,
        session: &AuthSession,
        shared_serials: &[String],
    ) -> Result<Vec<Camera>, AdapterError> {
        let mut cameras = self.list_cameras(session)?;
        let mut known_serials = cameras
            .iter()
            .map(|camera| camera.serial.clone())
            .collect::<HashSet<_>>();
        let mut known_channels = cameras
            .iter()
            .map(|camera| (camera.serial.clone(), camera.channel))
            .collect::<HashSet<_>>();
        for serial in shared_serials {
            if !known_serials.insert(serial.clone()) {
                continue;
            }
            let mut channels = self.cameras_for_device(session, serial, serial)?;
            channels.sort_by_key(|camera| camera.channel);
            for camera in channels {
                if known_channels.insert((camera.serial.clone(), camera.channel)) {
                    cameras.push(camera);
                }
            }
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
