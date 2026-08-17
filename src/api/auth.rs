use md5::{Digest, Md5};
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;
use url::Url;

use crate::{
    config::Credentials,
    error::{AdapterError, ErrorCategory},
};

use super::{ApiClient, AuthSession};

impl ApiClient {
    pub fn login(&self, credentials: &Credentials) -> Result<AuthSession, AdapterError> {
        self.login_at(self.base_url.clone(), credentials, 0)
    }

    fn login_at(
        &self,
        base_url: Url,
        credentials: &Credentials,
        redirects: u8,
    ) -> Result<AuthSession, AdapterError> {
        let password = format!(
            "{:x}",
            Md5::digest(credentials.password.expose_secret().as_bytes())
        );
        let form = [
            ("account", credentials.username.expose_secret()),
            ("password", password.as_str()),
            ("featureCode", self.feature_code.as_str()),
        ];
        let response = self
            .http
            .post(Self::endpoint(&base_url, "/v3/users/login/v2")?)
            .headers(self.headers(None)?)
            .form(&form)
            .send()
            .map_err(|_| {
                AdapterError::new(
                    ErrorCategory::AuthenticationFailed,
                    "Hik-Connect login request failed",
                )
            })?;
        if !response.status().is_success() {
            return Err(AdapterError::new(
                ErrorCategory::AuthenticationFailed,
                format!(
                    "Hik-Connect login HTTP status {}",
                    response.status().as_u16()
                ),
            ));
        }
        let payload: Value = response.json().map_err(|_| {
            AdapterError::new(
                ErrorCategory::AuthenticationFailed,
                "invalid Hik-Connect login response",
            )
        })?;
        let code = payload
            .pointer("/meta/code")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        if code == 1100 {
            if redirects >= 1 {
                return Err(AdapterError::new(
                    ErrorCategory::ApiFailed,
                    "Hik-Connect login redirected more than once",
                ));
            }
            let domain = payload
                .pointer("/loginArea/apiDomain")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    AdapterError::new(
                        ErrorCategory::ApiFailed,
                        "login redirect is missing API domain",
                    )
                })?;
            let regional = if domain.starts_with("http://") || domain.starts_with("https://") {
                Url::parse(domain)
            } else {
                Url::parse(&format!("https://{domain}"))
            }
            .map_err(|_| {
                AdapterError::new(ErrorCategory::ApiFailed, "invalid regional API domain")
            })?;
            return self.login_at(regional, credentials, redirects + 1);
        }
        Self::require_meta_success(&payload)?;
        let session_id = payload
            .pointer("/loginSession/sessionId")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                AdapterError::new(
                    ErrorCategory::AuthenticationFailed,
                    "login succeeded without a session",
                )
            })?;
        let refresh_session_id = payload
            .pointer("/loginSession/rfSessionId")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(|value| SecretString::from(value.to_owned()));
        Ok(AuthSession {
            api_base_url: base_url,
            session_id: SecretString::from(session_id.to_owned()),
            refresh_session_id,
            feature_code: self.feature_code.clone(),
        })
    }
}
