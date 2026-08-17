use std::fs;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use hik_adapter::{
    model::{Camera, RecordingInfo},
    runtime::AdapterState,
    server::{ServerState, router},
};
use tempfile::tempdir;
use tower::ServiceExt;

fn test_app() -> (axum::Router, tempfile::TempDir) {
    let directory = tempdir().unwrap();
    let bytes = (0_u8..100).collect::<Vec<_>>();
    fs::write(directory.path().join("SERIAL-1.mp4"), bytes).unwrap();
    let state = AdapterState::default();
    let camera = Camera {
        index: 1,
        name: "Camera".to_owned(),
        device_name: "Device".to_owned(),
        serial: "SERIAL".to_owned(),
        channel: 1,
    };
    state.register(std::slice::from_ref(&camera));
    state.complete(
        &camera.key(),
        RecordingInfo {
            file_name: "SERIAL-1.mp4".to_owned(),
            playback_url: "/recordings/SERIAL-1.mp4".to_owned(),
            duration_seconds: 30.0,
            size_bytes: 100,
            codec: "hevc".to_owned(),
            width: 1280,
            height: 720,
            frame_rate: "20/1".to_owned(),
        },
    );
    (
        router(ServerState {
            adapter: state,
            output_dir: directory.path().to_owned(),
        }),
        directory,
    )
}

#[tokio::test]
async fn serves_full_and_partial_recording_without_buffer_registry_escape() {
    let (app, _directory) = test_app();
    let full = app
        .clone()
        .oneshot(
            Request::get("/recordings/SERIAL-1.mp4")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(full.status(), StatusCode::OK);
    assert_eq!(full.headers()[header::ACCEPT_RANGES], "bytes");
    assert_eq!(to_bytes(full.into_body(), 200).await.unwrap().len(), 100);

    let partial = app
        .oneshot(
            Request::get("/recordings/SERIAL-1.mp4")
                .header(header::RANGE, "bytes=10-19")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(partial.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(partial.headers()[header::CONTENT_RANGE], "bytes 10-19/100");
    assert_eq!(
        to_bytes(partial.into_body(), 20).await.unwrap().as_ref(),
        &(10_u8..20).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn rejects_multiple_unknown_and_traversal_ranges() {
    let (app, _directory) = test_app();
    for range in ["bytes=100-", "bytes=0-1,3-4"] {
        let response = app
            .clone()
            .oneshot(
                Request::get("/recordings/SERIAL-1.mp4")
                    .header(header::RANGE, range)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    }
    for path in ["/recordings/unknown.mp4", "/recordings/%2e%2e%2fsecret"] {
        let response = app
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}

#[tokio::test]
async fn json_endpoints_contain_only_public_state() {
    let (app, _directory) = test_app();
    for path in ["/health", "/api/cameras", "/api/recordings"] {
        let response = app
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 8192).await.unwrap();
        assert!(!String::from_utf8_lossy(&body).contains("token-sentinel"));
    }
}
