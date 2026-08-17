use std::fmt;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;
use url::{Url, form_urlencoded};

use crate::{
    error::{AdapterError, ErrorCategory},
    model::Camera,
};

use super::{ApiClient, AuthSession};

#[derive(Clone)]
pub struct VtmContext {
    pub camera: Camera,
    pub relay_host: String,
    pub relay_port: u16,
    pub stream_url: SecretString,
}

impl fmt::Debug for VtmContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VtmContext")
            .field("camera", &self.camera)
            .field("relay_host", &self.relay_host)
            .field("relay_port", &self.relay_port)
            .field("stream_url", &"[REDACTED]")
            .finish()
    }
}

impl ApiClient {
    pub fn resolve_vtm(
        &self,
        session: &AuthSession,
        camera: &Camera,
    ) -> Result<VtmContext, AdapterError> {
        let resources = self.get_json(
            session,
            &format!("/v3/userdevices/v1/resources/{}", camera.serial),
            &[("filter", "VTM".to_owned())],
        )?;
        Self::require_meta_success(&resources)?;
        let resource = resources
            .pointer("/resourceInfos")
            .or_else(|| resources.pointer("/data/resourceInfos"))
            .and_then(Value::as_array)
            .and_then(|items| {
                items.iter().find(|item| {
                    item.get("deviceSerial").and_then(Value::as_str) == Some(camera.serial.as_str())
                        && value_as_u16(item.get("localIndex")) == Some(camera.channel)
                })
            })
            .ok_or_else(|| {
                vtm_error(
                    ErrorCategory::VtmResourceMissing,
                    format!("no VTM resource for channel {}", camera.channel),
                )
            })?;
        required_str(resource, "resourceId", ErrorCategory::VtmResourceMissing)?;
        let stream_biz_url = resource
            .get("streamBizUrl")
            .and_then(Value::as_str)
            .unwrap_or_default();

        let current = self.get_json(
            session,
            &format!("/v3/streaming/vtm/{}/{}", camera.serial, camera.channel),
            &[],
        )?;
        Self::require_meta_success(&current)?;
        let config = current
            .pointer("/streamServerConfig")
            .or_else(|| current.pointer("/data/streamServerConfig"))
            .and_then(Value::as_object)
            .ok_or_else(|| vtm_error(ErrorCategory::VtmResourceMissing, "missing VTM relay"))?;
        let relay_host = ["externalIp", "domain", "internalIp"]
            .iter()
            .find_map(|key| config.get(*key).and_then(Value::as_str))
            .map(str::trim)
            .filter(|host| !host.is_empty())
            .ok_or_else(|| vtm_error(ErrorCategory::VtmResourceMissing, "missing VTM relay host"))?
            .to_owned();
        let relay_port = config
            .get("port")
            .and_then(|value| value_as_u16(Some(value)))
            .filter(|port| *port > 0)
            .ok_or_else(|| {
                vtm_error(ErrorCategory::VtmResourceMissing, "invalid VTM relay port")
            })?;
        let token = self.acquire_vtdu_token(session)?;
        let stream_url = build_vtm_url(
            &relay_host,
            relay_port,
            &camera.serial,
            camera.channel,
            stream_biz_url,
            token,
            55,
            chrono::Utc::now().timestamp_millis(),
        )?;
        Ok(VtmContext {
            camera: camera.clone(),
            relay_host,
            relay_port,
            stream_url,
        })
    }

    fn acquire_vtdu_token(&self, session: &AuthSession) -> Result<SecretString, AdapterError> {
        let url = Self::endpoint(&session.api_base_url, "/api/user/token/get")?;
        let response = self
            .http
            .post(url)
            .headers(self.headers(Some(session))?)
            .form(&[
                ("sessionId", session.session_id.expose_secret()),
                ("count", "5"),
            ])
            .send()
            .map_err(|_| vtm_error(ErrorCategory::VtmTokenFailed, "VTDU token request failed"))?;
        if response.status().is_success() {
            let payload: Value = response.json().map_err(|_| {
                vtm_error(ErrorCategory::VtmTokenFailed, "invalid VTDU token response")
            })?;
            if let Some(token) = first_token(&payload) {
                return Ok(SecretString::from(token.to_owned()));
            }
        }
        self.acquire_fallback_token(session)
    }

    fn acquire_fallback_token(&self, session: &AuthSession) -> Result<SecretString, AdapterError> {
        let system = self.get_json(session, "/v3/configurations/system/info", &[])?;
        Self::require_meta_success(&system)?;
        let auth_addr = [
            "/authAddr",
            "/systemInfo/authAddr",
            "/serviceUrls/authAddr",
            "/data/authAddr",
            "/data/serviceUrls/authAddr",
        ]
        .iter()
        .find_map(|pointer| system.pointer(pointer).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty() && !matches!(*value, "null" | "none"))
        .ok_or_else(|| vtm_error(ErrorCategory::VtmTokenFailed, "missing authAddr"))?;
        let mut auth_base =
            if auth_addr.starts_with("http://") || auth_addr.starts_with("https://") {
                Url::parse(auth_addr)
            } else {
                Url::parse(&format!("https://{auth_addr}"))
            }
            .map_err(|_| vtm_error(ErrorCategory::VtmTokenFailed, "invalid authAddr"))?;
        if !auth_base.path().ends_with('/') {
            auth_base.set_path(&format!("{}/", auth_base.path()));
        }
        let sign = session_sign(session.session_id.expose_secret())?;
        let response = self
            .http
            .get(Self::endpoint(&auth_base, "/vtdutoken2")?)
            .query(&[
                ("ssid", session.session_id.expose_secret()),
                ("sign", sign.as_str()),
            ])
            .send()
            .map_err(|_| {
                vtm_error(
                    ErrorCategory::VtmTokenFailed,
                    "fallback token request failed",
                )
            })?;
        if !response.status().is_success() {
            return Err(vtm_error(
                ErrorCategory::VtmTokenFailed,
                "fallback token HTTP failure",
            ));
        }
        let payload: Value = response.json().map_err(|_| {
            vtm_error(
                ErrorCategory::VtmTokenFailed,
                "invalid fallback token response",
            )
        })?;
        let success = matches!(payload.get("retcode"), Some(Value::Number(n)) if n.as_i64() == Some(0))
            || payload.get("retcode").and_then(Value::as_str) == Some("0");
        if !success {
            return Err(vtm_error(
                ErrorCategory::VtmTokenFailed,
                "fallback token service rejected request",
            ));
        }
        first_token(&payload)
            .map(|token| SecretString::from(token.to_owned()))
            .ok_or_else(|| vtm_error(ErrorCategory::VtmTokenFailed, "fallback returned no token"))
    }
}

#[allow(clippy::too_many_arguments)]
pub fn build_vtm_url(
    host: &str,
    port: u16,
    serial: &str,
    channel: u16,
    stream_biz_url: &str,
    token: SecretString,
    client_type: u16,
    timestamp_ms: i64,
) -> Result<SecretString, AdapterError> {
    if host.trim().is_empty() || port == 0 || serial.trim().is_empty() || channel == 0 {
        return Err(vtm_error(
            ErrorCategory::ValidationFailed,
            "invalid VTM URL input",
        ));
    }
    let raw_query = stream_biz_url
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or(stream_biz_url.trim_start_matches('?'));
    let mut params = form_urlencoded::Serializer::new(String::new());
    for (key, value) in form_urlencoded::parse(raw_query.as_bytes()) {
        if !matches!(
            key.as_ref(),
            "dev" | "chn" | "stream" | "cln" | "isp" | "auth" | "ssn" | "vip" | "timestamp"
        ) {
            params.append_pair(&key, &value);
        }
    }
    params
        .append_pair("dev", serial)
        .append_pair("chn", &channel.to_string())
        .append_pair("stream", "2")
        .append_pair("cln", &client_type.to_string())
        .append_pair("isp", "0")
        .append_pair("auth", "1")
        .append_pair("ssn", token.expose_secret())
        .append_pair("vip", "0")
        .append_pair("timestamp", &timestamp_ms.to_string());
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    Ok(SecretString::from(format!(
        "ysproto://{host}:{port}/live?{}",
        params.finish()
    )))
}

fn first_token(payload: &Value) -> Option<&str> {
    payload
        .pointer("/tokens")
        .or_else(|| payload.pointer("/data/tokens"))
        .and_then(Value::as_array)?
        .iter()
        .filter_map(Value::as_str)
        .find(|token| !token.is_empty())
}

fn session_sign(session_id: &str) -> Result<String, AdapterError> {
    let payload = session_id
        .split('.')
        .nth(1)
        .ok_or_else(|| vtm_error(ErrorCategory::VtmTokenFailed, "session token is not a JWT"))?;
    let decoded = URL_SAFE_NO_PAD.decode(payload).map_err(|_| {
        vtm_error(
            ErrorCategory::VtmTokenFailed,
            "invalid session token claims",
        )
    })?;
    let claims: Value = serde_json::from_slice(&decoded).map_err(|_| {
        vtm_error(
            ErrorCategory::VtmTokenFailed,
            "invalid session token claims",
        )
    })?;
    claims
        .get("s")
        .and_then(Value::as_str)
        .filter(|sign| !sign.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            vtm_error(
                ErrorCategory::VtmTokenFailed,
                "session token has no sign claim",
            )
        })
}

fn value_as_u16(value: Option<&Value>) -> Option<u16> {
    value
        .and_then(Value::as_u64)
        .and_then(|value| u16::try_from(value).ok())
        .or_else(|| {
            value
                .and_then(Value::as_str)
                .and_then(|value| value.parse().ok())
        })
}

fn required_str<'a>(
    value: &'a Value,
    key: &str,
    category: ErrorCategory,
) -> Result<&'a str, AdapterError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| vtm_error(category, format!("missing {key}")))
}

fn vtm_error(category: ErrorCategory, message: impl Into<String>) -> AdapterError {
    AdapterError::new(category, message)
}
