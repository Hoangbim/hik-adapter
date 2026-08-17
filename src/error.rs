use serde::Serialize;
use thiserror::Error;

use crate::model::PublicError;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    ConfigurationInvalid,
    AuthenticationFailed,
    CaptchaRequired,
    ApiFailed,
    CameraOffline,
    PermissionDenied,
    VtmResourceMissing,
    VtmTokenFailed,
    VtmNegotiationFailed,
    VtmRedirectLimit,
    EncryptedStream,
    NoMedia,
    UnsupportedTransport,
    FfmpegFailed,
    ValidationFailed,
    Cancelled,
}

#[derive(Clone, Debug, Error)]
#[error("{message}")]
pub struct AdapterError {
    pub category: ErrorCategory,
    pub message: String,
}

impl AdapterError {
    pub fn new(category: ErrorCategory, message: impl Into<String>) -> Self {
        Self {
            category,
            message: message.into(),
        }
    }

    pub fn configuration(error: impl std::fmt::Display) -> Self {
        Self::new(ErrorCategory::ConfigurationInvalid, error.to_string())
    }

    pub fn public(&self) -> PublicError {
        PublicError {
            category: self.category,
            message: self.message.clone(),
        }
    }

    pub fn sanitize(mut self, secrets: &[&str]) -> Self {
        for secret in secrets.iter().filter(|value| !value.is_empty()) {
            self.message = self.message.replace(secret, "[REDACTED]");
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_secret_values() {
        let error = AdapterError::new(ErrorCategory::ApiFailed, "token=secret-value")
            .sanitize(&["secret-value"]);
        assert_eq!(error.message, "token=[REDACTED]");
    }
}
