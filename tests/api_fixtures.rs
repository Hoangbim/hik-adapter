use std::time::Duration;

use hik_adapter::{api::ApiClient, config::Credentials, error::ErrorCategory};
use httpmock::{
    Method::{GET, POST},
    MockServer,
};
use secrecy::SecretString;
use serde_json::{Value, json};

fn credentials(user: &str, password: &str) -> Credentials {
    Credentials {
        username: SecretString::from(user.to_owned()),
        password: SecretString::from(password.to_owned()),
    }
}

fn fixture(name: &str) -> Value {
    let raw = match name {
        "login_success" => include_str!("fixtures/login_success.json"),
        "device_page" => include_str!("fixtures/device_page.json"),
        "camera_info" => include_str!("fixtures/camera_info.json"),
        _ => panic!("unknown fixture"),
    };
    serde_json::from_str(raw).unwrap()
}

#[test]
fn login_hashes_password_and_redacts_session() {
    let server = MockServer::start();
    let login = server.mock(|when, then| {
        when.method(POST)
            .path("/v3/users/login/v2")
            .body_contains("password=5f4dcc3b5aa765d61d8327deb882cf99");
        then.status(200).json_body(fixture("login_success"));
    });
    let session = ApiClient::new(server.url(""), Duration::from_secs(2))
        .unwrap()
        .login(&credentials("user", "password"))
        .unwrap();
    login.assert();
    assert!(!format!("{session:?}").contains("refresh-value"));
}

#[test]
fn login_follows_one_regional_redirect() {
    let initial = MockServer::start();
    let regional = MockServer::start();
    let first = initial.mock(|when, then| {
        when.method(POST).path("/v3/users/login/v2");
        then.status(200).json_body(json!({
            "meta": {"code": 1100},
            "loginArea": {"apiDomain": regional.url("")}
        }));
    });
    let second = regional.mock(|when, then| {
        when.method(POST).path("/v3/users/login/v2");
        then.status(200).json_body(fixture("login_success"));
    });
    ApiClient::new(initial.url(""), Duration::from_secs(2))
        .unwrap()
        .login(&credentials("user", "password"))
        .unwrap();
    first.assert();
    second.assert();
}

#[test]
fn login_maps_captcha() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/v3/users/login/v2");
        then.status(200).json_body(json!({"meta": {"code": 1015}}));
    });
    let error = ApiClient::new(server.url(""), Duration::from_secs(2))
        .unwrap()
        .login(&credentials("user", "password"))
        .unwrap_err();
    assert_eq!(error.category, ErrorCategory::CaptchaRequired);
}

#[test]
fn inventory_orders_channels_and_indexes_cameras() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/v3/userdevices/v1/devices/pagelist");
        then.status(200).json_body(fixture("device_page"));
    });
    server.mock(|when, then| {
        when.method(GET)
            .path("/v3/userdevices/v1/cameras/info")
            .query_param("deviceSerial", "SERIAL-A");
        then.status(200).json_body(fixture("camera_info"));
    });
    server.mock(|when, then| {
        when.method(GET)
            .path("/v3/userdevices/v1/cameras/info")
            .query_param("deviceSerial", "SERIAL-B");
        then.status(200).json_body(json!({
            "meta": {"code": 200},
            "cameraInfos": [{"channelNo": 1, "cameraName": "Only"}]
        }));
    });
    let client = ApiClient::new(server.url(""), Duration::from_secs(2)).unwrap();
    server.mock(|when, then| {
        when.method(POST).path("/v3/users/login/v2");
        then.status(200).json_body(fixture("login_success"));
    });
    let session = client.login(&credentials("user", "password")).unwrap();
    let cameras = client.list_cameras(&session).unwrap();
    assert_eq!(
        cameras
            .iter()
            .map(|c| (c.serial.as_str(), c.channel))
            .collect::<Vec<_>>(),
        vec![("SERIAL-A", 1), ("SERIAL-A", 2), ("SERIAL-B", 1)]
    );
    assert_eq!(
        cameras.iter().map(|c| c.index).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
}
