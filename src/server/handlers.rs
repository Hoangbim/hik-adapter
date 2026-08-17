use std::{path::PathBuf, sync::Arc};

use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

use crate::{model::RecordingInfo, runtime::AdapterState};

use super::parse_single_range;

#[derive(Clone)]
pub struct ServerState {
    pub adapter: AdapterState,
    pub output_dir: PathBuf,
}

pub fn router(state: ServerState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/api/cameras", get(cameras))
        .route("/api/recordings", get(recordings))
        .route("/recordings/{file_name}", get(recording))
        .with_state(Arc::new(state))
}

async fn index() -> Html<&'static str> {
    Html(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>Hik VTM recordings</title></head><body><h1>Hik VTM recordings</h1><p>HEVC playback depends on browser and operating-system support. Download the MP4 if inline playback fails.</p><main id="items"></main><script>fetch('/api/recordings').then(r=>r.json()).then(items=>{const root=document.getElementById('items');for(const item of items){const section=document.createElement('section');const title=document.createElement('h2');title.textContent=item.file_name;const video=document.createElement('video');video.controls=true;video.preload='metadata';video.src=item.playback_url;const link=document.createElement('a');link.href=item.playback_url;link.textContent='Download MP4';link.download=item.file_name;section.append(title,video,link);root.append(section)}});</script></body></html>"#,
    )
}

async fn health() -> impl IntoResponse {
    axum::Json(serde_json::json!({"status": "ok"}))
}

async fn cameras(State(state): State<Arc<ServerState>>) -> impl IntoResponse {
    axum::Json(state.adapter.snapshot())
}

async fn recordings(State(state): State<Arc<ServerState>>) -> impl IntoResponse {
    let items = state
        .adapter
        .snapshot()
        .cameras
        .into_iter()
        .filter_map(|entry| entry.recording)
        .collect::<Vec<RecordingInfo>>();
    axum::Json(items)
}

async fn recording(
    State(state): State<Arc<ServerState>>,
    Path(file_name): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Some(public) = state.adapter.recording(&file_name) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if public.file_name != file_name || file_name.contains(['/', '\\']) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let path = state.output_dir.join(&file_name);
    let mut file = match tokio::fs::File::open(&path).await {
        Ok(file) => file,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let metadata = match file.metadata().await {
        Ok(metadata) if metadata.is_file() => metadata,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let file_len = metadata.len();
    let requested = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok());
    let range = match parse_single_range(requested, file_len) {
        Ok(range) => range,
        Err(_) => {
            let mut response = StatusCode::RANGE_NOT_SATISFIABLE.into_response();
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{file_len}")).expect("valid range"),
            );
            return response;
        }
    };
    let (status, start, end) = match range {
        Some(range) => (StatusCode::PARTIAL_CONTENT, range.start, range.end),
        None => (StatusCode::OK, 0, file_len.saturating_sub(1)),
    };
    if file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let content_len = end.saturating_sub(start) + 1;
    let stream = ReaderStream::new(file.take(content_len));
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = status;
    let response_headers = response.headers_mut();
    response_headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("video/mp4"));
    response_headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    response_headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=0"),
    );
    response_headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&content_len.to_string()).expect("valid length"),
    );
    if let Some(range) = range {
        response_headers.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {}-{}/{file_len}", range.start, range.end))
                .expect("valid range"),
        );
    }
    if let Ok(modified) = metadata.modified() {
        response_headers.insert(
            header::LAST_MODIFIED,
            HeaderValue::from_str(&httpdate::fmt_http_date(modified)).expect("valid date"),
        );
        let modified_seconds = modified
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or_default();
        response_headers.insert(
            header::ETAG,
            HeaderValue::from_str(&format!("\"{file_len:x}-{modified_seconds:x}\""))
                .expect("valid etag"),
        );
    }
    response
}
