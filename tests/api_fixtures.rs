use std::time::Duration;

use hik_adapter::{
    api::{ApiClient, build_vtm_url},
    config::Credentials,
    error::ErrorCategory,
    model::Camera,
};
use httpmock::{
    Method::{GET, POST},
    MockServer,
};
use secrecy::{ExposeSecret, SecretString};
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
        "vtm_resource" => include_str!("fixtures/vtm_resource.json"),
        "vtm_current" => include_str!("fixtures/vtm_current.json"),
        "vtdu_tokens" => include_str!("fixtures/vtdu_tokens.json"),
        _ => panic!("unknown fixture"),
    };
    serde_json::from_str(raw).unwrap()
}

fn camera(channel: u16) -> Camera {
    Camera {
        index: channel as usize,
        name: format!("Camera {channel}"),
        device_name: "Device".to_owned(),
        serial: "SERIAL-A".to_owned(),
        channel,
    }
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

#[test]
fn vtm_url_contains_required_query_but_debug_is_redacted() {
    let url = build_vtm_url(
        "relay.test",
        8555,
        "SERIAL",
        2,
        "?foo=bar&stream=1",
        SecretString::from("token-value".to_owned()),
        55,
        1_700_000_000_000,
    )
    .unwrap();
    let exposed = url.expose_secret();
    for required in [
        "ysproto://relay.test:8555/live?",
        "foo=bar",
        "dev=SERIAL",
        "chn=2",
        "stream=2",
        "cln=55",
        "ssn=token-value",
    ] {
        assert!(exposed.contains(required), "missing {required}");
    }
    assert!(!format!("{url:?}").contains("token-value"));
}

#[test]
fn vtm_selects_exact_channel_and_primary_token() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/v3/users/login/v2");
        then.status(200).json_body(fixture("login_success"));
    });
    let resource = server.mock(|when, then| {
        when.method(GET)
            .path("/v3/userdevices/v1/resources/SERIAL-A")
            .query_param("filter", "VTM");
        then.status(200).json_body(fixture("vtm_resource"));
    });
    server.mock(|when, then| {
        when.method(GET).path("/v3/streaming/vtm/SERIAL-A/2");
        then.status(200).json_body(fixture("vtm_current"));
    });
    let token = server.mock(|when, then| {
        when.method(POST)
            .path("/api/user/token/get")
            .body_contains("count=5");
        then.status(200)
            .json_body(json!({"tokens": ["", "primary-token"]}));
    });
    let client = ApiClient::new(server.url(""), Duration::from_secs(2)).unwrap();
    let session = client.login(&credentials("user", "password")).unwrap();
    let context = client.resolve_vtm(&session, &camera(2)).unwrap();
    resource.assert();
    token.assert();
    assert_eq!(context.relay_host, "relay.test");
    assert_eq!(context.relay_port, 8555);
    assert!(context.stream_url.expose_secret().contains("biz=two"));
    assert!(context.stream_url.expose_secret().contains("primary-token"));
    assert!(!format!("{context:?}").contains("primary-token"));
}

#[test]
fn vtm_missing_channel_has_stable_error_category() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/v3/users/login/v2");
        then.status(200).json_body(fixture("login_success"));
    });
    server.mock(|when, then| {
        when.method(GET)
            .path("/v3/userdevices/v1/resources/SERIAL-A");
        then.status(200).json_body(fixture("vtm_resource"));
    });
    let client = ApiClient::new(server.url(""), Duration::from_secs(2)).unwrap();
    let session = client.login(&credentials("user", "password")).unwrap();
    let error = client.resolve_vtm(&session, &camera(3)).unwrap_err();
    assert_eq!(error.category, ErrorCategory::VtmResourceMissing);
}

#[test]
fn vtm_falls_back_to_auth_service_token() {
    let api = MockServer::start();
    let auth = MockServer::start();
    let jwt = "eyJhbGciOiJub25lIn0.eyJzIjoic2lnbi12YWx1ZSJ9.";
    api.mock(|when, then| {
        when.method(POST).path("/v3/users/login/v2");
        then.status(200).json_body(json!({
            "meta": {"code": 200},
            "loginSession": {"sessionId": jwt}
        }));
    });
    api.mock(|when, then| {
        when.method(GET)
            .path("/v3/userdevices/v1/resources/SERIAL-A");
        then.status(200).json_body(fixture("vtm_resource"));
    });
    api.mock(|when, then| {
        when.method(GET).path("/v3/streaming/vtm/SERIAL-A/2");
        then.status(200).json_body(fixture("vtm_current"));
    });
    api.mock(|when, then| {
        when.method(POST).path("/api/user/token/get");
        then.status(200).json_body(json!({"tokens": []}));
    });
    api.mock(|when, then| {
        when.method(GET).path("/v3/configurations/system/info");
        then.status(200).json_body(json!({
            "meta": {"code": 200}, "serviceUrls": {"authAddr": auth.url("")}
        }));
    });
    let fallback = auth.mock(|when, then| {
        when.method(GET)
            .path("/vtdutoken2")
            .query_param("ssid", jwt)
            .query_param("sign", "sign-value");
        then.status(200).json_body(fixture("vtdu_tokens"));
    });
    let client = ApiClient::new(api.url(""), Duration::from_secs(2)).unwrap();
    let session = client.login(&credentials("user", "password")).unwrap();
    let context = client.resolve_vtm(&session, &camera(2)).unwrap();
    fallback.assert();
    assert!(
        context
            .stream_url
            .expose_secret()
            .contains("fallback-token")
    );
}
