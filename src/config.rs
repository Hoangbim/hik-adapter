use std::{
    fmt,
    path::{Path, PathBuf},
};

use secrecy::SecretString;
use serde::{Deserialize, Serialize};

use crate::error::{AdapterError, ErrorCategory};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct AppConfig {
    pub hik_connect: HikConnectConfig,
    pub recording: RecordingConfig,
    pub server: ServerConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct HikConnectConfig {
    pub api_base_url: String,
    pub web_base_url: String,
    pub username_env: String,
    pub password_env: String,
    pub request_timeout_seconds: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct RecordingConfig {
    pub duration_seconds: u64,
    pub output_dir: PathBuf,
    pub ffmpeg_path: PathBuf,
    pub ffprobe_path: PathBuf,
    pub camera_serials: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct ServerConfig {
    pub bind: String,
    pub public_base_url: Option<String>,
}

#[derive(Clone)]
pub struct Credentials {
    pub username: SecretString,
    pub password: SecretString,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credentials { username: [REDACTED], password: [REDACTED] }")
    }
}

impl Credentials {
    pub fn from_config(config: &HikConnectConfig) -> Result<Self, AdapterError> {
        let username = std::env::var(&config.username_env).map_err(|_| {
            AdapterError::new(
                ErrorCategory::ConfigurationInvalid,
                format!(
                    "required environment variable {} is missing",
                    config.username_env
                ),
            )
        })?;
        let password = std::env::var(&config.password_env).map_err(|_| {
            AdapterError::new(
                ErrorCategory::ConfigurationInvalid,
                format!(
                    "required environment variable {} is missing",
                    config.password_env
                ),
            )
        })?;
        if username.is_empty() || password.is_empty() {
            return Err(AdapterError::new(
                ErrorCategory::ConfigurationInvalid,
                "credential environment variables must be non-empty",
            ));
        }
        Ok(Self {
            username: SecretString::from(username),
            password: SecretString::from(password),
        })
    }
}

impl AppConfig {
    pub fn load(path: &Path) -> Result<Self, AdapterError> {
        let raw = std::fs::read_to_string(path).map_err(AdapterError::configuration)?;
        let config: Self = serde_yaml::from_str(&raw).map_err(AdapterError::configuration)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), AdapterError> {
        if self.recording.duration_seconds == 0 {
            return Err(AdapterError::new(
                ErrorCategory::ConfigurationInvalid,
                "duration_seconds must be positive",
            ));
        }
        if self.hik_connect.request_timeout_seconds == 0 {
            return Err(AdapterError::new(
                ErrorCategory::ConfigurationInvalid,
                "request_timeout_seconds must be positive",
            ));
        }
        self.server
            .bind
            .parse::<std::net::SocketAddr>()
            .map_err(|_| {
                AdapterError::new(
                    ErrorCategory::ConfigurationInvalid,
                    "server.bind must be a socket address",
                )
            })?;
        Ok(())
    }
}

impl Default for HikConnectConfig {
    fn default() -> Self {
        Self {
            api_base_url: "https://api.hik-connect.com".into(),
            web_base_url: "https://www.hik-connect.com".into(),
            username_env: "HIKCONNECT_USERNAME".into(),
            password_env: "HIKCONNECT_PASSWORD".into(),
            request_timeout_seconds: 20,
        }
    }
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            duration_seconds: 30,
            output_dir: PathBuf::from("recordings"),
            ffmpeg_path: PathBuf::from("ffmpeg"),
            ffprobe_path: PathBuf::from("ffprobe"),
            camera_serials: Vec::new(),
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:8787".into(),
            public_base_url: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};

    use secrecy::ExposeSecret;

    use super::*;

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    #[test]
    fn credentials_come_from_named_environment_variables() {
        let _guard = ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        unsafe {
            std::env::set_var("TEST_HIK_USER", "recipient");
            std::env::set_var("TEST_HIK_PASSWORD", "secret");
        }
        let config = HikConnectConfig {
            username_env: "TEST_HIK_USER".into(),
            password_env: "TEST_HIK_PASSWORD".into(),
            ..HikConnectConfig::default()
        };
        let credentials = Credentials::from_config(&config).unwrap();
        assert_eq!(credentials.username.expose_secret(), "recipient");
        assert_eq!(credentials.password.expose_secret(), "secret");
        assert!(!format!("{credentials:?}").contains("secret"));
        unsafe {
            std::env::remove_var("TEST_HIK_USER");
            std::env::remove_var("TEST_HIK_PASSWORD");
        }
    }
}
