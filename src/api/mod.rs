mod auth;
mod cameras;

use std::{fmt, time::Duration};

use rand::RngCore;
use reqwest::{
    blocking::Client,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;
use url::Url;

use crate::error::{AdapterError, ErrorCategory};

pub const CLIENT_TYPE: &str = "55";
pub const CLIENT_VERSION: &str = "6.0.0.20250101";

pub struct ApiClient {
    pub(crate) http: Client,
    pub(crate) base_url: Url,
    pub(crate) feature_code: String,
    timeout: Duration,
}

#[derive(Clone)]
pub struct AuthSession {
    pub api_base_url: Url,
    pub session_id: SecretString,
    pub refresh_session_id: Option<SecretString>,
    pub feature_code: String,
}

impl fmt::Debug for AuthSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthSession")
            .field("api_base_url", &self.api_base_url)
            .field("session_id", &"[REDACTED]")
            .field(
                "refresh_session_id",
                &self.refresh_session_id.as_ref().map(|_| "[REDACTED]"),
            )
            .field("feature_code", &self.feature_code)
            .finish()
    }
}

impl ApiClient {
    pub fn new(base_url: impl AsRef<str>, timeout: Duration) -> Result<Self, AdapterError> {
        let base_url = Url::parse(base_url.as_ref()).map_err(|_| {
            AdapterError::new(
                ErrorCategory::ConfigurationInvalid,
                "invalid Hik-Connect API base URL",
            )
        })?;
        let http = Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .map_err(|_| {
                AdapterError::new(
                    ErrorCategory::ConfigurationInvalid,
                    "could not build HTTP client",
                )
            })?;
        let mut feature = [0_u8; 16];
        rand::thread_rng().fill_bytes(&mut feature);
        let feature_code = feature.iter().map(|byte| format!("{byte:02x}")).collect();
        Ok(Self {
            http,
            base_url,
            feature_code,
            timeout,
        })
    }

    pub fn clone_for_worker(&self) -> Result<Self, AdapterError> {
        let mut worker = Self::new(self.base_url.as_str(), self.timeout)?;
        worker.feature_code.clone_from(&self.feature_code);
        Ok(worker)
    }

    pub(crate) fn headers(&self, session: Option<&AuthSession>) -> Result<HeaderMap, AdapterError> {
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("clienttype", CLIENT_TYPE),
            ("clientversion", CLIENT_VERSION),
            ("featurecode", self.feature_code.as_str()),
            ("lang", "en-US"),
            ("clientno", "hik-adapter"),
            ("clientname", "hik-adapter"),
            ("osversion", "Linux"),
            ("nettype", "WIFI"),
        ] {
            headers.insert(
                HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
                    AdapterError::new(ErrorCategory::ApiFailed, "invalid client header")
                })?,
                HeaderValue::from_str(value).map_err(|_| {
                    AdapterError::new(ErrorCategory::ApiFailed, "invalid client header value")
                })?,
            );
        }
        if let Some(session) = session {
            headers.insert(
                HeaderName::from_static("sessionid"),
                HeaderValue::from_str(session.session_id.expose_secret()).map_err(|_| {
                    AdapterError::new(ErrorCategory::ApiFailed, "invalid session identifier")
                })?,
            );
        }
        Ok(headers)
    }

    pub(crate) fn endpoint(base: &Url, path: &str) -> Result<Url, AdapterError> {
        base.join(path)
            .map_err(|_| AdapterError::new(ErrorCategory::ApiFailed, "invalid API endpoint"))
    }

    pub(crate) fn get_json(
        &self,
        session: &AuthSession,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<Value, AdapterError> {
        let url = Self::endpoint(&session.api_base_url, path)?;
        let response = self
            .http
            .get(url)
            .headers(self.headers(Some(session))?)
            .query(query)
            .send()
            .map_err(|_| {
                AdapterError::new(ErrorCategory::ApiFailed, "Hik-Connect request failed")
            })?;
        if !response.status().is_success() {
            return Err(AdapterError::new(
                ErrorCategory::ApiFailed,
                format!("Hik-Connect HTTP status {}", response.status().as_u16()),
            ));
        }
        response.json().map_err(|_| {
            AdapterError::new(
                ErrorCategory::ApiFailed,
                "invalid Hik-Connect JSON response",
            )
        })
    }

    pub(crate) fn require_meta_success(payload: &Value) -> Result<(), AdapterError> {
        let code = payload
            .pointer("/meta/code")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        if code == 200 {
            return Ok(());
        }
        let category = match code {
            1013 | 1014 => ErrorCategory::AuthenticationFailed,
            1015 => ErrorCategory::CaptchaRequired,
            _ => ErrorCategory::ApiFailed,
        };
        Err(AdapterError::new(
            category,
            format!("Hik-Connect API error code {code}"),
        ))
    }
}
