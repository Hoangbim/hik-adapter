# Hik-Connect VTM Feasibility Adapter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a standalone Rust `hik-adapter` that logs in to Hik-Connect, lists camera channels, records up to ten VTM streams concurrently on distinct OS threads into validated 30-second HEVC MP4 files, and serves those files over HTTP Range endpoints.

**Architecture:** A blocking Rust Hik-Connect/VTM client performs cloud bootstrap and owns one TCP/FFmpeg pipeline per camera worker thread. Shared runtime state is exposed by an Axum server running on the coordinating Tokio runtime; validated MP4 files are the only media exposed to HTTP clients.

**Tech Stack:** Rust 1.96 / edition 2024, `reqwest` blocking + rustls, `serde`, `clap`, `thiserror`, `secrecy`, `axum`, `tokio`, `tokio-util`, external FFmpeg/ffprobe, `httpmock` and local TCP fixtures for tests.

## Global Constraints

- Implement VTM only; do not add UDP P2P, SRT, HLS, WebRTC, RTSP, PTZ, cloud replay, or Ermis MediaBus integration.
- Run at most ten selected cameras, with one named OS thread and one VTM socket/FFmpeg child per camera.
- Record exactly 30 seconds from the first accepted media packet by default; allow configuration but require a positive duration.
- Lock transport to MPEG-PS after the first `00 00 01 BA` body and preserve all later VTM media bodies verbatim.
- Produce HEVC MP4 with `-c:v copy -an -movflags +faststart`; do not implement an MP4 muxer.
- Keep username, password, session IDs, VTDU tokens, resource URLs, and relay query strings out of logs, errors, files, and HTTP responses.
- Read credentials only from environment-variable names configured in YAML.
- Serve validated `.mp4` files only; `.part.mp4` files never appear in playback APIs.
- Default HTTP bind is `127.0.0.1:8787`; external exposure is for trusted LAN/VPN only.
- Preserve Apache-2.0 attribution for behavior ported from `hikcloudstream` and `pyezvizapi`; Python is not a runtime dependency.

---

## File Map

- `Cargo.toml`: package metadata, runtime dependencies, and test dependencies.
- `src/lib.rs`: public module surface used by the binary and integration tests.
- `src/config.rs`: YAML/environment loading and bounded configuration validation.
- `src/error.rs`: typed, sanitized error categories.
- `src/model.rs`: camera, session metadata, recording metadata, and public status types.
- `src/api/{auth,cameras,vtm}.rs`: REST login, inventory, VTM resource, relay, and token bootstrap.
- `src/vtm/{protocol,transport,session}.rs`: URL/protobuf/header encoding, blocking TCP framing, redirects, keepalive, and media iteration.
- `src/recording/{ffmpeg,validation}.rs`: child process lifecycle, atomic output, and ffprobe/frame validation.
- `src/runtime/{camera_worker,state}.rs`: ten-thread coordinator and failure-isolated shared status.
- `src/server/{handlers,range}.rs`: Axum routes, HTML playback page, and streaming byte ranges.
- `src/main.rs`: `list` and `run` command orchestration and shutdown.
- `tests/fixtures/*.json`: redacted REST payload fixtures.
- `tests/{api_fixtures,vtm_protocol,vtm_mock_server,http_range}.rs`: offline integration coverage.

---

### Task 1: Repository Scaffold, Configuration, Models, and Source Documentation

**Files:**
- Create: `Cargo.toml`
- Create: `.gitignore`
- Create: `config.example.yaml`
- Create: `src/lib.rs`
- Create: `src/config.rs`
- Create: `src/error.rs`
- Create: `src/model.rs`
- Create: `README.md`
- Create: `LICENSES/Apache-2.0.txt`
- Create: `LICENSES/THIRD_PARTY_NOTICES.md`
- Copy: `../ermis-nvr/docs/superpowers/specs/2026-08-13-hikconnect-shared-stream-pull-design.md` to `docs/source-material/hik-connect-shared-stream-pull-design.md`
- Copy: `../ermis-nvr/docs/superpowers/specs/2026-07-06-ezviz-real-service-design.md` to `docs/source-material/ezviz-real-service-design.md`
- Create: `docs/source-material/VTM_PORTING_NOTES.md`
- Test: unit tests inside `src/config.rs` and `src/error.rs`

**Interfaces:**
- Produces: `AppConfig::load(path) -> Result<AppConfig, AdapterError>`
- Produces: `Credentials::from_config(&HikConnectConfig) -> Result<Credentials, AdapterError>`
- Produces: `AdapterError { category: ErrorCategory, message: String }`
- Produces: serializable `Camera`, `CameraKey`, `CameraStatus`, `RecordingInfo`, and `PublicError`

- [ ] **Step 1: Write failing configuration tests**

Add tests that pin bounds and secret handling:

```rust
#[test]
fn rejects_more_than_ten_cameras() {
    let mut config = AppConfig::default();
    config.recording.max_cameras = 11;
    let error = config.validate().unwrap_err();
    assert_eq!(error.category, ErrorCategory::ConfigurationInvalid);
}

#[test]
fn credentials_come_from_named_environment_variables() {
    let _guard = ENV_LOCK.lock().unwrap();
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
}
```

- [ ] **Step 2: Run the tests to verify scaffold symbols are missing**

Run: `cargo test config::tests -- --nocapture`

Expected: compilation fails because `AppConfig`, `Credentials`, and `ErrorCategory` do not exist.

- [ ] **Step 3: Add package dependencies and minimal module surface**

Use this dependency baseline in `Cargo.toml`:

```toml
[package]
name = "hik-adapter"
version = "0.1.0"
edition = "2024"
license = "UNLICENSED"

[dependencies]
anyhow = "1"
axum = "0.8"
base64 = "0.22"
chrono = { version = "0.4", features = ["serde"] }
clap = { version = "4.5", features = ["derive"] }
ctrlc = "3"
http = "1"
httpdate = "1"
md-5 = "0.10"
rand = "0.8"
reqwest = { version = "0.12", default-features = false, features = ["blocking", "json", "rustls-tls"] }
secrecy = "0.10"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml = "0.9"
thiserror = "2"
tokio = { version = "1", features = ["full"] }
tokio-util = { version = "0.7", features = ["io"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }
url = "2"

[dev-dependencies]
assert_cmd = "2"
httpmock = "0.7"
predicates = "3"
tempfile = "3"
tower = { version = "0.5", features = ["util"] }
```

Export modules from `src/lib.rs`:

```rust
pub mod api;
pub mod config;
pub mod error;
pub mod model;
pub mod recording;
pub mod runtime;
pub mod server;
pub mod vtm;
```

- [ ] **Step 4: Implement validated configuration and secret wrappers**

Define the exact configuration shape:

```rust
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AppConfig {
    pub hik_connect: HikConnectConfig,
    pub recording: RecordingConfig,
    pub server: ServerConfig,
}

impl AppConfig {
    pub fn load(path: &Path) -> Result<Self, AdapterError> {
        let raw = std::fs::read_to_string(path).map_err(AdapterError::configuration)?;
        let config: Self = serde_yaml::from_str(&raw).map_err(AdapterError::configuration)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), AdapterError> {
        if !(1..=10).contains(&self.recording.max_cameras) {
            return Err(AdapterError::new(ErrorCategory::ConfigurationInvalid, "max_cameras must be 1..=10"));
        }
        if self.recording.duration_seconds == 0 {
            return Err(AdapterError::new(ErrorCategory::ConfigurationInvalid, "duration_seconds must be positive"));
        }
        Ok(())
    }
}
```

Use `secrecy::SecretString` for `Credentials` and session/token material. Implement a custom `Debug` that prints only `Credentials { username: [REDACTED], password: [REDACTED] }`.

- [ ] **Step 5: Define stable public models and error categories**

Create these exact model contracts:

```rust
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
pub struct CameraKey { pub serial: String, pub channel: u16 }

#[derive(Clone, Debug, Serialize)]
pub struct Camera {
    pub index: usize,
    pub name: String,
    pub device_name: String,
    pub serial: String,
    pub channel: u16,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraStatus { Pending, Resolving, Connecting, Recording, Validating, Completed, Failed }

#[derive(Clone, Debug, Serialize)]
pub struct RecordingInfo {
    pub file_name: String,
    pub playback_url: String,
    pub duration_seconds: f64,
    pub size_bytes: u64,
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub frame_rate: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct PublicError {
    pub category: ErrorCategory,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    ConfigurationInvalid, AuthenticationFailed, CaptchaRequired, ApiFailed,
    CameraOffline, PermissionDenied, VtmResourceMissing, VtmTokenFailed,
    VtmNegotiationFailed, VtmRedirectLimit, EncryptedStream, NoMedia,
    UnsupportedTransport, FfmpegFailed, ValidationFailed, Cancelled,
}
```

- [ ] **Step 6: Add source material and initial README**

Copy only the two approved Markdown specs. Add `VTM_PORTING_NOTES.md` naming the ported Apache-2.0 source files and constants, without copying credentials, packet captures, wheels, APK content, or virtual environments. README must already show prerequisites, environment variables, `list`, `run`, and the trusted-LAN warning.

- [ ] **Step 7: Run formatting and unit tests**

Run: `cargo fmt --check && cargo test config::tests error::tests`

Expected: all configuration and redaction tests pass.

- [ ] **Step 8: Commit the foundation**

```bash
git add Cargo.toml .gitignore config.example.yaml README.md LICENSES docs/source-material src/lib.rs src/config.rs src/error.rs src/model.rs
git commit -m "feat: scaffold Hik-Connect VTM adapter"
```

---

### Task 2: Hik-Connect Login and Camera Inventory

**Files:**
- Create: `src/api/mod.rs`
- Create: `src/api/auth.rs`
- Create: `src/api/cameras.rs`
- Create: `tests/fixtures/login_success.json`
- Create: `tests/fixtures/login_redirect.json`
- Create: `tests/fixtures/device_page.json`
- Create: `tests/fixtures/camera_info.json`
- Create: `tests/api_fixtures.rs`

**Interfaces:**
- Consumes: `Credentials`, `Camera`, `AdapterError`
- Produces: `ApiClient::login(&Credentials) -> Result<AuthSession, AdapterError>`
- Produces: `ApiClient::list_cameras(&AuthSession) -> Result<Vec<Camera>, AdapterError>`
- Produces: `ApiClient::clone_for_worker() -> Result<ApiClient, AdapterError>` with a distinct blocking HTTP client and the same non-secret client identity
- Produces: `AuthSession` with redacted `session_id`, `refresh_session_id`, `api_base_url`, and stable client headers

- [ ] **Step 1: Write login request and regional redirect tests**

Use `httpmock::MockServer` to assert lowercase MD5 form data and a single redirect:

```rust
#[test]
fn login_hashes_password_and_follows_one_region_redirect() {
    let server = MockServer::start();
    let first = server.mock(|when, then| {
        when.method(POST).path("/v3/users/login/v2")
            .body_contains("password=5f4dcc3b5aa765d61d8327deb882cf99");
        then.status(200).json_body_obj(&json!({
            "meta": {"code": 1100},
            "loginArea": {"apiDomain": server.host()}
        }));
    });
    // A second mock returns meta.code=200 and a redacted fixture JWT.
    let session = ApiClient::new(server.url(""), Duration::from_secs(2))
        .unwrap().login(&credentials("user", "password")).unwrap();
    assert_eq!(first.hits(), 1);
    assert!(!format!("{session:?}").contains("session-value"));
}
```

Add separate tests for CAPTCHA classification, missing session ID, and a second redirect returning `ApiFailed`.

- [ ] **Step 2: Run login tests and confirm failure**

Run: `cargo test --test api_fixtures login -- --nocapture`

Expected: compilation fails because `ApiClient` and `AuthSession` are missing.

- [ ] **Step 3: Implement blocking API client and login**

Implement one feature code per `ApiClient`, client identity headers, and recursive login bounded by a redirect counter:

```rust
pub struct ApiClient {
    http: reqwest::blocking::Client,
    base_url: Url,
    feature_code: String,
}

pub struct AuthSession {
    pub api_base_url: Url,
    pub session_id: SecretString,
    pub refresh_session_id: Option<SecretString>,
    pub feature_code: String,
}

impl ApiClient {
    pub fn login(&self, credentials: &Credentials) -> Result<AuthSession, AdapterError> {
        self.login_at(self.base_url.clone(), credentials, 0)
    }
}
```

Map known verification/CAPTCHA API codes to `CaptchaRequired`; map invalid account/password to `AuthenticationFailed`; never include raw response bodies in errors.

- [ ] **Step 4: Write paginated inventory and stable ordering tests**

Fixtures must contain two device pages and unsorted channels. Assert the result is indexed from 1 and sorted by device-page order then numeric channel:

```rust
assert_eq!(cameras.iter().map(|c| (c.serial.as_str(), c.channel)).collect::<Vec<_>>(), vec![
    ("SERIAL-A", 1),
    ("SERIAL-A", 2),
    ("SERIAL-B", 1),
]);
```

- [ ] **Step 5: Implement camera inventory**

Page `/v3/userdevices/v1/devices/pagelist` with limit 50 and the exact device filter from the spec. For every returned serial, call `/v3/userdevices/v1/cameras/info`, skip malformed entries without serial/channel, sort channels, and reindex the flattened vector.

- [ ] **Step 6: Run API tests**

Run: `cargo test --test api_fixtures`

Expected: login, redirect, error classification, pagination, and channel ordering pass.

- [ ] **Step 7: Commit API bootstrap**

```bash
git add src/api tests/fixtures tests/api_fixtures.rs
git commit -m "feat: add Hik-Connect login and camera inventory"
```

---

### Task 3: Recipient VTM Resource, Relay, Token, and URL Bootstrap

**Files:**
- Create: `src/api/vtm.rs`
- Create: `tests/fixtures/vtm_resource.json`
- Create: `tests/fixtures/vtm_current.json`
- Create: `tests/fixtures/vtdu_tokens.json`
- Modify: `tests/api_fixtures.rs`
- Modify: `src/api/mod.rs`

**Interfaces:**
- Consumes: `ApiClient`, `AuthSession`, `Camera`
- Produces: `ApiClient::resolve_vtm(&AuthSession, &Camera) -> Result<VtmContext, AdapterError>`
- Produces: `VtmContext { camera, relay_host, relay_port, stream_url: SecretString }`
- Produces: `build_vtm_url(...) -> Result<SecretString, AdapterError>`

- [ ] **Step 1: Write direct-resource selection tests**

Fixture `vtm_resource.json` must include two `resourceInfos` entries for the same serial with `localIndex` 1 and 2. Assert channel 2 selects only its matching `resourceId` and `streamBizUrl`. Assert missing channel maps to `VtmResourceMissing`.

- [ ] **Step 2: Write token and URL redaction tests**

```rust
#[test]
fn vtm_url_contains_required_query_but_debug_is_redacted() {
    let url = build_vtm_url("relay.test", 8555, "SERIAL", 2,
        "?foo=bar", SecretString::from("token-value"), 55, 1_700_000_000_000).unwrap();
    let exposed = url.expose_secret();
    assert!(exposed.starts_with("ysproto://relay.test:8555/live?"));
    assert!(exposed.contains("dev=SERIAL"));
    assert!(exposed.contains("chn=2"));
    assert!(exposed.contains("stream=2"));
    assert!(exposed.contains("ssn=token-value"));
    assert!(!format!("{url:?}").contains("token-value"));
}
```

- [ ] **Step 3: Run VTM bootstrap tests and confirm failure**

Run: `cargo test --test api_fixtures vtm -- --nocapture`

Expected: compilation fails because `VtmContext` and `resolve_vtm` are missing.

- [ ] **Step 4: Implement primary recipient VTM bootstrap**

Implement the exact request order:

```text
GET  /v3/userdevices/v1/resources/{serial}?filter=VTM
GET  /v3/streaming/vtm/{serial}/{channel}
POST /api/user/token/get  form sessionId=<secret>&count=5
```

Select relay host in `externalIp`, `domain`, `internalIp` order; validate port in `1..=65535`; select the first non-empty token; merge `streamBizUrl` query parameters into the generated `ysproto` URL; force stream type 2 and client type 55.

- [ ] **Step 5: Implement regional auth token fallback**

When the primary endpoint returns no tokens, call `/v3/configurations/system/info`, read `authAddr`, decode JWT claim `s` without logging the JWT, and request the auth-service endpoint `/vtdutoken2` with `ssid` and `sign`. Tests cover missing `authAddr`, missing sign claim, non-zero retcode, and a valid token array.

- [ ] **Step 6: Run bootstrap tests**

Run: `cargo test --test api_fixtures vtm`

Expected: direct shared-resource resolution, current relay merge, primary tokens, fallback tokens, and secret redaction pass.

- [ ] **Step 7: Commit VTM cloud bootstrap**

```bash
git add src/api/vtm.rs src/api/mod.rs tests/fixtures tests/api_fixtures.rs
git commit -m "feat: resolve Hik-Connect VTM sessions"
```

---

### Task 4: VTM Header and Minimal Protobuf Protocol

**Files:**
- Create: `src/vtm/mod.rs`
- Create: `src/vtm/protocol.rs`
- Create: `src/vtm/transport.rs`
- Create: `tests/vtm_protocol.rs`

**Interfaces:**
- Produces: `VtmPacket::encode() -> Result<Vec<u8>, AdapterError>`
- Produces: `decode_header([u8; 8]) -> Result<VtmHeader, AdapterError>`
- Produces: `build_stream_info_request(&SecretString, Option<&SecretString>) -> Vec<u8>`
- Produces: `parse_stream_info_response(&[u8]) -> Result<StreamInfoResponse, AdapterError>`
- Produces: `TransportLock::accept(&[u8]) -> Result<(), AdapterError>`

- [ ] **Step 1: Write exact VTM header vectors**

```rust
#[test]
fn encodes_vtm_header_big_endian() {
    let packet = VtmPacket::new(Channel::Message, MessageCode::StreamInfoReq, 0, vec![0x0a, 0x01, b'x']);
    assert_eq!(packet.encode().unwrap(), vec![
        0x24, 0x00, 0x00, 0x03, 0x00, 0x00, 0x01, 0x3b, 0x0a, 0x01, b'x'
    ]);
}

#[test]
fn rejects_unknown_channel_and_bad_body_length() {
    assert!(decode_header([0x24, 0x7f, 0, 0, 0, 0, 1, 0x3b]).is_err());
    assert!(VtmPacket::decode(&[0x24, 0, 0, 2, 0, 0, 1, 0x3b, 0]).is_err());
}
```

- [ ] **Step 2: Write minimal protobuf vectors**

Pin varint and length-delimited fields without adding a generated protobuf dependency:

```rust
#[test]
fn parses_stream_info_response_fields() {
    fn hex_bytes(input: &str) -> Vec<u8> {
        input.split_whitespace()
            .map(|byte| u8::from_str_radix(byte, 16).unwrap())
            .collect()
    }

    let body = hex_bytes("08 00 22 03 73 73 6e 2a 03 6b 65 79");
    let response = parse_stream_info_response(&body).unwrap();
    assert_eq!(response.result, Some(0));
    assert_eq!(response.stream_session.as_deref(), Some("ssn"));
    assert_eq!(response.redirect_key.as_ref().map(|v| v.expose_secret()), Some("key"));
}
```

Also test malformed varints, unsupported wire types, fields exceeding payload, and negative/oversized lengths.

- [ ] **Step 3: Run protocol tests and confirm failure**

Run: `cargo test --test vtm_protocol`

Expected: compilation fails because VTM protocol types are missing.

- [ ] **Step 4: Implement the exact 8-byte framing subset**

Use constants:

```rust
pub const VTM_MAGIC: u8 = 0x24;
pub const VTM_HEADER_SIZE: usize = 8;

#[repr(u8)]
pub enum Channel { Message = 0x00, Stream = 0x01, EncryptedMessage = 0x0a, EncryptedStream = 0x0b }

#[repr(u16)]
pub enum MessageCode {
    KeepAliveReq = 0x0132,
    KeepAliveRsp = 0x0133,
    StreamInfoReq = 0x013b,
    StreamInfoRsp = 0x013c,
    StreamInfoNotify = 0x013d,
}
```

Reject bodies larger than `u16::MAX`. Keep packet bodies out of `Debug`.

- [ ] **Step 5: Implement minimal protobuf helpers**

Implement only wire types 0 and 2, `encode_varint`, `read_varint`, string/bytes fields, and known response fields 1, 4, 5, and 7. `build_stream_info_request` emits fields: URL=1, optional redirect key=2, client version=3, zero varint=4, client version=6. `build_keepalive_request` emits stream session as bytes field 1.

- [ ] **Step 6: Implement MPEG-PS transport lock**

```rust
#[derive(Default)]
pub struct TransportLock { locked: bool }

impl TransportLock {
    pub fn accept(&mut self, body: &[u8]) -> Result<(), AdapterError> {
        if !self.locked {
            if !body.starts_with(&[0x00, 0x00, 0x01, 0xba]) {
                return Err(AdapterError::new(ErrorCategory::UnsupportedTransport, "first media packet is not MPEG-PS"));
            }
            self.locked = true;
        }
        Ok(())
    }
}
```

Test that a later fragment without a pack header is accepted after locking and rejected before locking.

- [ ] **Step 7: Run protocol tests**

Run: `cargo test --test vtm_protocol`

Expected: all header, protobuf, redaction, malformed input, and transport-lock cases pass.

- [ ] **Step 8: Commit wire protocol**

```bash
git add src/vtm tests/vtm_protocol.rs
git commit -m "feat: implement VTM framing protocol"
```

---

### Task 5: Blocking VTM TCP Session and Mock Relay

**Files:**
- Create: `src/vtm/session.rs`
- Create: `tests/vtm_mock_server.rs`
- Modify: `src/vtm/mod.rs`

**Interfaces:**
- Consumes: `VtmContext`, VTM packet/protobuf helpers
- Produces: `VtmSession::connect(VtmContext, Duration) -> Result<VtmSession, AdapterError>`
- Produces: `VtmSession::start(max_redirects) -> Result<StreamInfoResponse, AdapterError>`
- Produces: `VtmSession::for_each_media(deadline, callback) -> Result<MediaStats, AdapterError>`

- [ ] **Step 1: Build a local mock VTM relay fixture**

The fixture binds `127.0.0.1:0`, accepts one TCP client, reads exactly one 8-byte header and body, verifies message code `0x013b`, then sends:

```rust
let response_body = proto_fields(&[(1, Varint(0)), (4, Bytes(b"session"))]);
write_packet(&mut socket, Channel::Message, MessageCode::StreamInfoRsp, 0, &response_body);
write_packet(&mut socket, Channel::Stream, MessageCode::StreamInfoNotify, 1,
    b"\x00\x00\x01\xba...fixture-ps...");
```

Return a join handle so tests fail if the expected request is not received.

- [ ] **Step 2: Write session success, redirect, encrypted, and timeout tests**

Tests must assert:

- negotiation result 0 yields media;
- a non-zero response with redirect URL/key reconnects and stops after three redirects;
- channel `0x0b` maps to `EncryptedStream` without exposing the body;
- a successful negotiation with no media for the configured timeout maps to `NoMedia`;
- server keepalive request receives `KeepAliveRsp`.

- [ ] **Step 3: Run mock relay tests and confirm failure**

Run: `cargo test --test vtm_mock_server -- --nocapture`

Expected: compilation fails because `VtmSession` is missing.

- [ ] **Step 4: Implement blocking socket lifecycle**

`connect` parses the secret `ysproto` URL, resolves host/port, opens `TcpStream::connect_timeout`, and applies read/write timeouts. `read_packet` uses `read_exact` for header and body. `send_packet` wraps sequence at `u16::MAX`.

`start` sends one stream-info request, reads at most 20 control packets, follows only a response containing all of non-zero result, redirect URL, and redirect key, and returns `VtmRedirectLimit` on a fourth redirect. Result other than zero without a valid redirect maps to `VtmNegotiationFailed`.

- [ ] **Step 5: Implement media loop and keepalive**

Expose media through a callback so streams are never buffered in memory:

```rust
pub fn for_each_media<F>(
    &mut self,
    media_duration: Duration,
    no_media_timeout: Duration,
    shutdown: &AtomicBool,
    mut sink: F,
) -> Result<MediaStats, AdapterError>
where F: FnMut(&[u8]) -> Result<(), AdapterError>;
```

Return exact counters without retaining packet bodies:

```rust
#[derive(Clone, Copy, Debug, Default)]
pub struct MediaStats {
    pub packets: u64,
    pub bytes: u64,
    pub elapsed: Duration,
}
```

Start the media duration clock only after the first channel-1 packet passes `TransportLock`. Send keepalive every five seconds using the stream session. Respond immediately to server keepalive requests. Reject encrypted stream packets.

- [ ] **Step 6: Run session tests**

Run: `cargo test --test vtm_mock_server`

Expected: success, redirects, redirect limit, keepalive, encrypted channel, socket close, and timeout tests pass.

- [ ] **Step 7: Commit VTM session**

```bash
git add src/vtm/session.rs src/vtm/mod.rs tests/vtm_mock_server.rs
git commit -m "feat: stream media from VTM TCP relay"
```

---

### Task 6: FFmpeg Recording and Validation

**Files:**
- Create: `src/recording/mod.rs`
- Create: `src/recording/ffmpeg.rs`
- Create: `src/recording/validation.rs`
- Create: `tests/fixtures/ffprobe_hevc.json`
- Test: unit tests inside recording modules

**Interfaces:**
- Produces: `FfmpegRecorder::start(config, camera) -> Result<FfmpegRecorder, AdapterError>`
- Produces: `FfmpegRecorder::write_media(&[u8]) -> Result<(), AdapterError>`
- Produces: `FfmpegRecorder::finish(timeout) -> Result<PathBuf, AdapterError>`
- Produces: `validate_recording(path, ffprobe, ffmpeg) -> Result<RecordingInfo, AdapterError>`

- [ ] **Step 1: Write command construction and safe filename tests**

```rust
#[test]
fn ffmpeg_command_preserves_video_and_drops_audio() {
    let args = ffmpeg_args(Path::new("SERIAL-1.part.mp4"));
    assert_eq!(args, vec![
        "-hide_banner", "-loglevel", "error", "-y", "-f", "mpeg", "-i", "pipe:0",
        "-map", "0:v:0", "-c:v", "copy", "-an", "-movflags", "+faststart",
        "SERIAL-1.part.mp4",
    ]);
}

#[test]
fn output_name_sanitizes_serial() {
    assert_eq!(recording_name("../GK/8814", 1), "GK_8814-1.mp4");
}
```

- [ ] **Step 2: Write ffprobe parsing tests**

Parse the redacted fixture into `ProbeOutput`; accept HEVC Main 1280x720 at 20/1 and 30 seconds. Reject H.264, zero dimensions, missing frame rate, and durations outside 25..=40 seconds.

- [ ] **Step 3: Run recording tests and confirm failure**

Run: `cargo test recording:: -- --nocapture`

Expected: compilation fails because recorder and validator modules are missing.

- [ ] **Step 4: Implement FFmpeg child ownership**

Spawn with piped stdin, null stdout, and piped stderr. Never include a stream URL or secret in arguments. Keep stderr capped at 8 KiB for sanitized failure diagnostics. On finish, drop/close stdin, poll `try_wait` until 15 seconds, then kill and wait only that worker's child.

Write to `<final>.part.mp4`; remove no pre-existing final file. Refuse to start if either final or part path already exists unless the CLI explicitly selected `--overwrite` in a later task.

- [ ] **Step 5: Implement validation and atomic publication**

Run ffprobe with:

```text
ffprobe -v error -show_entries
format=duration,size,format_name:stream=codec_name,codec_type,width,height,r_frame_rate
-of json <part-file>
```

Then run one decode check:

```text
ffmpeg -hide_banner -loglevel error -i <part-file> -frames:v 1 -f null -
```

After both pass, rename `.part.mp4` atomically to `.mp4`, set mode `0600` on Unix, and return non-secret metadata.

- [ ] **Step 6: Run recording tests and a local synthetic FFmpeg smoke test**

Run: `cargo test recording::`

Create a local generated MPEG-PS/HEVC fixture only if the installed FFmpeg supports `libx265`; otherwise skip the smoke test with an explicit message. The unit tests must remain independent of codec availability.

- [ ] **Step 7: Commit recording support**

```bash
git add src/recording tests/fixtures/ffprobe_hevc.json
git commit -m "feat: record and validate VTM MP4 files"
```

---

### Task 7: Ten-Thread Camera Runtime and Shared Status

**Files:**
- Create: `src/runtime/mod.rs`
- Create: `src/runtime/state.rs`
- Create: `src/runtime/camera_worker.rs`
- Modify: `src/model.rs`
- Test: unit tests inside runtime modules

**Interfaces:**
- Consumes: `ApiClient`, `AuthSession`, selected `Camera` values, `VtmSession`, `FfmpegRecorder`
- Produces: `AdapterState` snapshot methods for HTTP
- Produces: `spawn_camera_workers(...) -> Vec<JoinHandle<WorkerResult>>`
- Produces: injectable `CameraJob` trait for thread-isolation tests

Use these concrete worker contracts:

```rust
pub trait CameraJob: Send + Sync + 'static {
    fn run(&self, camera: Camera, shutdown: Arc<AtomicBool>) -> Result<RecordingInfo, AdapterError>;
}

pub struct WorkerResult {
    pub camera: CameraKey,
    pub thread_id: ThreadId,
    pub result: Result<RecordingInfo, PublicError>,
}
```

- [ ] **Step 1: Write thread identity and failure-isolation tests**

Use a fake job that records `std::thread::current().id()` and optionally fails:

```rust
let cameras = fake_cameras(10);
let result = spawn_camera_workers(cameras, state.clone(), FakeJob::one_failure(4), shutdown.clone());
let joined = join_workers(result);
assert_eq!(joined.len(), 10);
assert_eq!(joined.iter().map(|r| r.thread_id).collect::<HashSet<_>>().len(), 10);
assert_eq!(state.snapshot().completed, 9);
assert_eq!(state.snapshot().failed, 1);
```

Assert thread names begin with `hik-vtm-` and sanitized errors contain no supplied token sentinel.

- [ ] **Step 2: Run runtime tests and confirm failure**

Run: `cargo test runtime:: -- --nocapture`

Expected: compilation fails because runtime types are missing.

- [ ] **Step 3: Implement shared state transitions**

Store camera entries in `Arc<RwLock<BTreeMap<CameraKey, CameraRuntimeState>>>`. Implement one method per legal transition and reject transitions out of `Completed`/`Failed`. Public snapshots contain only camera identity, timestamps, packet/byte counts, recording metadata, error category, and sanitized message.

- [ ] **Step 4: Implement named OS worker spawning**

Use `std::thread::Builder::new().name(...).spawn(...)`; never use Tokio tasks for camera workers. Wrap each job in `std::panic::catch_unwind`, update `Failed` on panic, and retain the other handles. Enforce `selected.len() <= 10` before spawning.

- [ ] **Step 5: Implement the real camera job**

The job performs:

```text
state Resolving
ApiClient::resolve_vtm
state Connecting
VtmSession::connect + start(result=0)
FfmpegRecorder::start
state Recording
VtmSession::for_each_media -> FfmpegRecorder::write_media
FfmpegRecorder::finish
state Validating
validate_recording + atomic rename
state Completed
```

Every error maps to `Failed`; child/session cleanup happens through `Drop` guards.

- [ ] **Step 6: Run runtime tests**

Run: `cargo test runtime::`

Expected: ten distinct thread IDs, stable state transitions, panic capture, partial failure, cancellation, and sanitized status pass.

- [ ] **Step 7: Commit concurrent runtime**

```bash
git add src/runtime src/model.rs
git commit -m "feat: run isolated VTM camera workers"
```

---

### Task 8: HTTP Status, Playback Page, and Byte Ranges

**Files:**
- Create: `src/server/mod.rs`
- Create: `src/server/handlers.rs`
- Create: `src/server/range.rs`
- Create: `tests/http_range.rs`

**Interfaces:**
- Consumes: `Arc<AdapterState>` and completed-recording registry
- Produces: `server::router(state) -> axum::Router`
- Produces: `parse_single_range(header, file_len) -> Result<Option<ByteRange>, RangeError>`

The internal registry stores paths separately from public JSON:

```rust
#[derive(Clone)]
pub struct CompletedRecording {
    pub path: PathBuf,
    pub public: RecordingInfo,
    pub modified: SystemTime,
}
```

- [ ] **Step 1: Write complete, prefix, open-ended, suffix, and invalid range tests**

```rust
assert_eq!(parse_single_range(None, 100).unwrap(), None);
assert_eq!(parse_single_range(Some("bytes=10-19"), 100).unwrap(), Some(ByteRange { start: 10, end: 19 }));
assert_eq!(parse_single_range(Some("bytes=90-"), 100).unwrap(), Some(ByteRange { start: 90, end: 99 }));
assert_eq!(parse_single_range(Some("bytes=-10"), 100).unwrap(), Some(ByteRange { start: 90, end: 99 }));
assert!(parse_single_range(Some("bytes=0-1,3-4"), 100).is_err());
```

- [ ] **Step 2: Write router integration tests**

Register a temp MP4 as completed and assert:

- `/health` and `/api/cameras` never contain secret sentinels;
- `/api/recordings` lists only the final MP4;
- full GET returns 200, `Accept-Ranges: bytes`, and exact bytes;
- partial GET returns 206 and correct `Content-Range`;
- unsatisfiable/multiple ranges return 416;
- unknown and traversal-shaped file names return 404.

- [ ] **Step 3: Run HTTP tests and confirm failure**

Run: `cargo test --test http_range`

Expected: compilation fails because the server modules are missing.

- [ ] **Step 4: Implement streaming range responses**

Resolve file names only through the completed-recording registry. Open with `tokio::fs::File`, seek with `AsyncSeekExt`, limit reads with `AsyncReadExt::take`, wrap with `tokio_util::io::ReaderStream`, and respond with `Body::from_stream`. Do not read the whole MP4 into memory.

Build ETag from stable file size and modified time. Include `Last-Modified`, `Content-Type: video/mp4`, and `Cache-Control: private, max-age=0`.

- [ ] **Step 5: Implement JSON endpoints and HTML page**

The HTML is a static string that fetches `/api/recordings`, creates one safe DOM entry per returned URL, and uses native `<video controls preload="metadata">`. It displays an HEVC compatibility note and a download link.

- [ ] **Step 6: Run HTTP tests**

Run: `cargo test --test http_range`

Expected: all JSON, range, streaming, traversal, and redaction tests pass.

- [ ] **Step 7: Commit HTTP playback server**

```bash
git add src/server tests/http_range.rs
git commit -m "feat: serve validated MP4 recordings"
```

---

### Task 9: CLI Orchestration, Documentation, and Offline Verification

**Files:**
- Create: `src/main.rs`
- Modify: `README.md`
- Modify: `config.example.yaml`
- Modify: `docs/source-material/VTM_PORTING_NOTES.md`
- Test: `tests/cli.rs`

**Interfaces:**
- Consumes: all library modules
- Produces: `hik-adapter list --config <path>`
- Produces: `hik-adapter run --config <path> [--overwrite]`

- [ ] **Step 1: Write CLI argument and missing-secret tests**

Use `assert_cmd` to verify `--help`, missing config, invalid camera limit, missing credential environment variables, and that stderr never contains a password sentinel.

- [ ] **Step 2: Run CLI tests and confirm failure**

Run: `cargo test --test cli`

Expected: compilation or command lookup fails because the binary is missing.

- [ ] **Step 3: Implement `list` command**

Load config and credentials, login, call `list_cameras`, and print exactly these non-secret columns: `INDEX`, `CAMERA`, `DEVICE`, `SERIAL`, `CHANNEL`. Return non-zero on login/list failure.

- [ ] **Step 4: Implement `run` command**

Perform login/inventory once, filter optional serial allowlist, take `max_cameras`, create output directory mode `0700` on Unix, create `AdapterState`, start Axum, spawn OS workers, and install Ctrl+C to set the shared shutdown flag. The server remains active after worker completion until Ctrl+C.

Print a final worker summary once all joins complete, but keep serving. Exit non-zero before worker start on global errors; after worker start, expose partial results through API and logs.

- [ ] **Step 5: Complete README and porting notes**

README must contain exact commands:

```bash
cp config.example.yaml config.yaml
export HIKCONNECT_USERNAME='recipient-account'
export HIKCONNECT_PASSWORD='recipient-password'
cargo run --release -- list --config config.yaml
cargo run --release -- run --config config.yaml
```

Document `server.bind: 0.0.0.0:8787`, trusted LAN/VPN warning, `http://HOST:8787/`, HEVC browser support, FFmpeg/ffprobe prerequisites, expected partial camera failures, output permissions, and cleanup. `VTM_PORTING_NOTES.md` maps every Rust protocol function to the Apache-2.0 reference function and records the VTM constants.

- [ ] **Step 6: Run full offline verification**

Run:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
```

Expected: formatting clean, no Clippy warnings, and all offline tests pass without network or credentials.

- [ ] **Step 7: Commit CLI and documentation**

```bash
git add src/main.rs tests/cli.rs README.md config.example.yaml docs/source-material/VTM_PORTING_NOTES.md
git commit -m "feat: add VTM feasibility CLI"
```

---

### Task 10: Live Hik-Connect Feasibility Run

**Files:**
- Create during run only: `config.live.yaml` (gitignored, no credentials)
- Create during run only: `recordings/*.mp4`, `recordings/*.part.mp4`, `recordings/live-summary.json` (gitignored)
- Modify after results: `README.md` only if observed behavior changes documented prerequisites or limitations

**Interfaces:**
- Consumes: ignored credentials from `../ermis-nvr/private/camlist/hikconnect.env`
- Produces: per-camera 30-second MP4s and a non-secret feasibility summary

- [ ] **Step 1: Verify tools and ignored paths before network access**

Run:

```bash
ffmpeg -version
ffprobe -version
git check-ignore config.live.yaml recordings/
```

Expected: FFmpeg and ffprobe are installed; live config and recordings are ignored.

- [ ] **Step 2: Run real account inventory without exposing credentials**

Load the ignored env file into the process environment without printing it, then run:

```bash
cargo run --release -- list --config config.live.yaml
```

Expected: login succeeds and a non-secret camera/channel table is printed.

- [ ] **Step 3: Start the ten-thread live run**

Run `cargo run --release -- run --config config.live.yaml` in a managed PTY/session. Confirm the process reports up to ten distinct worker thread names and the HTTP server address. Do not pass credentials on the command line.

- [ ] **Step 4: Poll status and recordings**

Request `/health`, `/api/cameras`, and `/api/recordings` until all selected workers reach `completed` or `failed`. Assert no response contains known credential/session/token sentinel values.

- [ ] **Step 5: Validate every successful MP4 externally**

For each completed file run:

```bash
ffprobe -v error -show_entries format=duration,size:stream=codec_name,width,height,r_frame_rate -of json recordings/<file>.mp4
ffmpeg -hide_banner -loglevel error -i recordings/<file>.mp4 -frames:v 1 -f null -
curl -I -H 'Range: bytes=0-1023' http://127.0.0.1:8787/recordings/<file>.mp4
```

Expected: HEVC, positive dimensions/frame rate, duration 25-40 seconds, decodable frame, and HTTP 206 with a correct `Content-Range`.

- [ ] **Step 6: Stop cleanly and capture a non-secret summary**

Send Ctrl+C through the managed session. Confirm sockets and child FFmpeg processes exit and no credential/token appears in `live-summary.json` or logs. Report attempted/completed/failed counts and sanitized per-camera reasons.

- [ ] **Step 7: Fix only live-discovered adapter defects and rerun proportional tests**

For each defect, first add a failing redacted fixture or local mock test, then implement the minimal fix, run the focused test, and repeat the affected live camera. Do not weaken redaction, timeouts, MP4 validation, or thread isolation to make the live run pass.

- [ ] **Step 8: Run final verification and commit any live-derived fixes**

Run:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
git status --short
```

Expected: all checks pass; only intentional source/docs changes are tracked; credentials and recordings remain ignored.

Commit any fixes with focused messages, for example:

```bash
git add src tests README.md
git commit -m "fix: preserve fragmented VTM MPEG-PS media"
```

Do not commit when the live run requires no source or documentation changes.
