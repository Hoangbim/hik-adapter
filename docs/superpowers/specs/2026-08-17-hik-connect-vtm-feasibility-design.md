# Hik-Connect VTM Feasibility Adapter Design

## Objective

Create an independent Rust repository named `hik-adapter`, beside `ermis-nvr`,
that proves Hik-Connect VTM relay access is viable without Python at runtime.
The adapter must log in, enumerate cameras, select up to ten cameras, record all
selected cameras concurrently on distinct operating-system threads for 30
seconds, validate one MP4 per successful camera, and serve completed recordings
over HTTP with byte-range support.

This is a feasibility module, not an Ermis NVR integration. It must produce a
clear per-camera result report so a partial result remains useful when shared or
offline cameras cannot stream.

## Confirmed Baseline

The existing experiment successfully pulled shared camera `GK8814993`, channel
1, through the Hik-Connect VTM TCP relay. A reconfirmation on 2026-08-17 returned
negotiation result `0`, 790 media packets, 641,520 raw bytes, and an unencrypted
MPEG-PS stream beginning with `00 00 01 BA`. Remuxing only the video yielded a
9.949-second HEVC Main MPEG-TS sample at 1280x720 and 20 fps.

The baseline implementation uses Python package `hikcloudstream` 0.1.4 and
`pyezvizapi`. The new repository ports the required login, inventory, VTM
resource, VTDU token, VTM wire protocol, and media iteration behavior to Rust.
FFmpeg and ffprobe remain external runtime tools for remuxing and validation.

## Scope

### In scope

- Hik-Connect consumer-account login, including one regional redirect.
- Paginated device inventory and per-device camera/channel enumeration.
- Optional serial allowlist; otherwise select the first ten enumerated cameras.
- Recipient-scoped VTM resource lookup for owned or shared cameras.
- VTDU token acquisition and current VTM relay lookup.
- VTM TCP negotiation, bounded redirects, and media packet iteration.
- Ten selected cameras recorded concurrently on ten distinct OS threads.
- Thirty seconds of media per camera, measured from the first accepted packet.
- MPEG-PS transport lock from the initial pack header.
- One FFmpeg process per camera, HEVC passthrough, audio exclusion, MP4 faststart.
- ffprobe validation and atomic publication of successful MP4 files.
- HTTP inventory and playback endpoints, including HTTP Range requests.
- Offline tests and an opt-in live feasibility run.

### Out of scope

- UDP P2P, NAT hole punching, or the proprietary SRT dialect.
- Live browser streaming, HLS, WebRTC, RTSP output, or Ermis MediaBus integration.
- PTZ, talkback, snapshots, playback from Hik-Connect cloud storage, or events.
- A custom MP4 muxer or bundled FFmpeg binary.
- Long-running session refresh beyond one 30-second recording attempt.
- Public-internet authentication, TLS termination, or authorization policy.

## Architecture Decision

Use a pure Rust control and transport implementation with one external FFmpeg
child process per camera. This proves that Python is not required for Hik-Connect
or VTM while avoiding an unrelated MP4 muxer implementation.

The rejected alternatives are:

1. A Rust wrapper around Python, because it would not prove the VTM protocol can
   run natively in Rust.
2. A fully native MP4 muxer, because it adds codec/container risk without adding
   evidence about Hik-Connect feasibility.

## Repository Layout

```text
hik-adapter/
  Cargo.toml
  README.md
  LICENSES/
    Apache-2.0.txt
    THIRD_PARTY_NOTICES.md
  config.example.yaml
  docs/
    source-material/
      hik-connect-shared-stream-pull-design.md
      ezviz-real-service-design.md
      VTM_PORTING_NOTES.md
    superpowers/specs/
      2026-08-17-hik-connect-vtm-feasibility-design.md
    superpowers/plans/
      2026-08-17-hik-connect-vtm-feasibility.md
  src/
    lib.rs
    main.rs
    config.rs
    error.rs
    model.rs
    api/
      mod.rs
      auth.rs
      cameras.rs
      vtm.rs
    vtm/
      mod.rs
      protocol.rs
      session.rs
      transport.rs
    recording/
      mod.rs
      ffmpeg.rs
      validation.rs
    runtime/
      mod.rs
      camera_worker.rs
      state.rs
    server/
      mod.rs
      handlers.rs
      range.rs
  tests/
    api_fixtures.rs
    vtm_protocol.rs
    vtm_mock_server.rs
    http_range.rs
```

Each file has one responsibility. API response parsing is separate from VTM
wire framing; recording is separate from relay transport; the HTTP server reads
shared status and completed files but never owns camera sessions.

## Configuration

`config.example.yaml` contains non-secret settings:

```yaml
hik_connect:
  api_base_url: "https://api.hik-connect.com"
  username_env: "HIKCONNECT_USERNAME"
  password_env: "HIKCONNECT_PASSWORD"
  request_timeout_seconds: 20

recording:
  max_cameras: 10
  duration_seconds: 30
  output_dir: "recordings"
  ffmpeg_path: "ffmpeg"
  ffprobe_path: "ffprobe"
  camera_serials: []

server:
  bind: "127.0.0.1:8787"
  public_base_url: null
```

`max_cameras` must be between 1 and 10 for this phase. `duration_seconds` must be
positive and defaults to exactly 30. An empty `camera_serials` selects the first
ten enumerated cameras in stable device/channel order. A non-empty allowlist
filters by device serial before applying the limit and includes all matching
channels until the limit is reached.

Credentials are read only from the named environment variables. They are not
accepted as CLI arguments, included in YAML, serialized, logged, placed in URLs,
or exposed by HTTP responses.

## Command-Line Interface

```text
hik-adapter list --config config.yaml
hik-adapter run --config config.yaml
```

`list` logs in, fetches all pages, prints a non-secret table of index, camera
name, device name, serial, and channel, then exits.

`run` logs in, enumerates and selects cameras, starts the HTTP server, spawns one
named OS thread per selected camera, and keeps serving after all recording
threads finish. Ctrl+C shuts down the server and joins or cancels remaining
workers without deleting completed recordings.

## REST Bootstrap and Inventory

The client generates one random feature code and sends the same Hik-Connect
client identity headers for the login and authenticated requests.

1. `POST /v3/users/login/v2` with `account`, lowercase MD5 password digest, and
   `featureCode`.
2. If `meta.code == 1100`, switch to `loginArea.apiDomain` and retry exactly
   once.
3. Preserve `sessionId`, `rfSessionId`, regional API base URL, and the session
   JWT sign claim in memory only.
4. Page `GET /v3/userdevices/v1/devices/pagelist` using `groupId=-1`, `limit=50`,
   `offset`, and filter
   `TIME_PLAN,CONNECTION,SWITCH,STATUS,STATUS_EXT,WIFI,NODISTURB,P2P,KMS,HIDDNS`.
5. For each device, call `GET /v3/userdevices/v1/cameras/info?deviceSerial=...`
   and sort channels numerically.

The implementation accepts owned and shared devices returned to the recipient
account. It does not query owner-only credentials.

## VTM Session Bootstrap

Each camera worker receives an immutable clone of the authenticated session
metadata and owns its own HTTP client, VTDU token, TCP relay connection, FFmpeg
process, and output files.

For each selected camera:

1. Fetch `GET /v3/userdevices/v1/resources/{serial}?filter=VTM`. Direct-resource
   lookup is required because the tested camera is shared and generic page-list
   behavior is not reliable for recipient accounts.
2. Select the `resourceInfos[]` entry matching serial and `localIndex == channel`.
3. Request current relay metadata from
   `GET /v3/streaming/vtm/{serial}/{channel}`.
4. Request five short-lived VTDU tokens from `POST /api/user/token/get` using
   the authenticated `sessionId`. The regional auth-service endpoint is a
   fallback only when this endpoint returns no token.
5. Build the VTM stream URL from relay host/port, serial, `streamBizUrl`, VTDU
   token, channel, client type 55, and stream type 2 (substream).
6. Connect with a 15-second timeout, send the VTM stream-info request, accept
   `STREAMINFO_RSP`, follow at most three redirects, and require negotiation
   result `0`.
7. Accept only stream and encrypted-stream channel packets with non-empty bodies.
   Encrypted packets fail this feasibility run with `encrypted_stream`; no
   validation-code support is included.

The VTM protocol implementation is ported from the Apache-2.0 implementations in
`hikcloudstream` and `pyezvizapi`. Copied behavior and constants are documented
in `LICENSES/THIRD_PARTY_NOTICES.md`; Python is not a build or runtime dependency.

## Concurrency Model

The coordinating runtime performs login and inventory once. It then calls
`std::thread::Builder::spawn` exactly once per selected camera, up to ten. Thread
names use `hik-vtm-{sanitized_serial}-{channel}`. Each worker uses blocking HTTPS,
blocking TCP, and blocking FFmpeg stdin so the OS-thread requirement is explicit
and observable.

Workers share only `Arc<RwLock<AdapterState>>` for public status snapshots and a
shutdown flag. A worker never shares a VTM socket, VTDU token, FFmpeg child, or
temporary output file with another worker. Panic capture converts a worker panic
to a per-camera failure and does not unwind the coordinator.

The state progression is:

```text
Pending -> Resolving -> Connecting -> Recording -> Validating -> Completed
                |             |            |             |
                +-------------+------------+-------------+-> Failed
```

Thirty seconds starts at the first accepted MPEG-PS media body, not at thread
creation or TCP connection time. No-media timeout is 15 seconds after successful
negotiation.

## Media Preservation and MP4 Recording

The first accepted media body must begin with an MPEG-PS pack header
`00 00 01 BA`. Once detected, transport is locked to MPEG-PS for the whole
session. Later fragments are passed through verbatim; per-fragment transport
detection is prohibited because it previously corrupted fragmented PS data.

Each worker launches:

```text
ffmpeg -hide_banner -loglevel error -y \
  -f mpeg -i pipe:0 \
  -map 0:v:0 -c:v copy -an \
  -movflags +faststart \
  <serial>-<channel>.part.mp4
```

After 30 seconds the worker closes FFmpeg stdin, waits up to 15 seconds for a
clean process exit, and kills only its own child if that deadline expires.
Malformed or unknown audio does not fail video recording because audio is never
mapped.

Validation runs ffprobe and requires:

- container format includes `mov` or `mp4`;
- one HEVC video stream;
- positive width and height;
- positive frame rate;
- duration between 25 and 40 seconds;
- at least one decodable frame using FFmpeg.

Only a validated file is atomically renamed to `{serial}-{channel}.mp4`.
Unvalidated files retain `.part.mp4` and are excluded from playback endpoints.

## HTTP Server

The HTTP server runs on a Tokio runtime in the coordinating process and exposes:

- `GET /health`: service status, selected camera count, active worker count,
  completed count, and failed count.
- `GET /api/cameras`: non-secret inventory and per-camera runtime state.
- `GET /api/recordings`: completed MP4 metadata and playback URLs.
- `GET /recordings/{file}.mp4`: validated files only, with `Accept-Ranges`,
  `Content-Length`, `Content-Range`, `ETag`, `Last-Modified`, and
  `Content-Type: video/mp4`.
- `GET /`: a dependency-free HTML page listing completed recordings with native
  `<video controls>` playback.

The recorded codec remains HEVC. Native browser playback therefore depends on
the client operating system and browser having HEVC support. The HTTP endpoint,
Range behavior, download, ffprobe validation, and playback in an HEVC-capable
client such as VLC remain valid when a particular browser cannot decode HEVC.

The range handler supports a complete response, `bytes=start-end`,
`bytes=start-`, and `bytes=-suffix`. Multiple ranges are rejected with HTTP 416.
Paths are matched against the completed-recording registry rather than joined
directly from user input, preventing traversal.

The default bind is `127.0.0.1:8787`. Binding `0.0.0.0:8787` is an explicit
feasibility choice for a trusted LAN or VPN. Authentication and TLS are out of
scope; public internet exposure requires an authenticated reverse proxy.

## Error Model

Errors are classified as:

- `configuration_invalid`
- `authentication_failed`
- `captcha_required`
- `api_failed`
- `camera_offline`
- `permission_denied`
- `vtm_resource_missing`
- `vtm_token_failed`
- `vtm_negotiation_failed`
- `vtm_redirect_limit`
- `encrypted_stream`
- `no_media`
- `unsupported_transport`
- `ffmpeg_failed`
- `validation_failed`
- `cancelled`

Error messages include camera serial/channel and protocol stage but redact the
account, password, session IDs, resource URLs, VTDU tokens, tickets, and relay
query strings. HTTP state exposes only the classification and a sanitized
message.

`run` fails globally before spawning workers when configuration, login, or
camera enumeration fails. After workers start, individual failures are isolated.
The live feasibility result is considered useful when at least one camera
produces a valid MP4, while the final report always retains results for all ten
attempted cameras.

## Testing

Offline tests cover:

- configuration validation and secret exclusion;
- login success, regional redirect, CAPTCHA, and malformed API payloads;
- paginated device inventory and stable channel ordering;
- recipient direct-resource VTM selection;
- VTDU primary and regional fallback token parsing;
- VTM stream URL construction without token logging;
- VTM frame encode/decode, negotiation result, redirects, and timeout;
- MPEG-PS initial detection and transport locking;
- worker failure isolation using a local mock VTM TCP server;
- FFmpeg command construction;
- ffprobe validation parsing;
- complete and partial HTTP range responses, suffix ranges, 416 responses, and
  path traversal rejection.

The opt-in live test uses the ignored credential file from Ermis NVR only through
environment variables. It selects up to ten cameras, launches one OS thread per
camera, records for 30 seconds, validates every completed MP4, and writes a
non-secret JSON summary. Live tests never run under normal `cargo test`.

## Acceptance Criteria

The feasibility phase is complete when:

1. `cargo test` passes without network or credentials.
2. `hik-adapter list` returns the recipient account's camera/channel inventory.
3. `hik-adapter run` visibly creates one OS thread per selected camera, up to ten.
4. Every selected attempt proceeds independently and appears in the status API;
   ten attempts are required when the account exposes at least ten cameras.
5. At least one accessible camera creates a validated 25-40 second HEVC MP4.
6. Every successful recording is retrievable through HTTP byte-range requests
   and plays in an HEVC-capable browser or media client.
7. A failed/offline camera does not stop another recording or the HTTP server.
8. Logs, JSON reports, configuration, output filenames, and HTTP responses contain
   no password, session ID, VTDU token, ticket, or KMS material.
9. README documents build, environment variables, list, run, external LAN access,
   expected partial failures, and cleanup.

## Source Material to Copy

The new repository copies the following documentation, not secrets or binary
captures:

- `ermis-nvr/docs/superpowers/specs/2026-08-13-hikconnect-shared-stream-pull-design.md`
  to `docs/source-material/hik-connect-shared-stream-pull-design.md`.
- `ermis-nvr/docs/superpowers/specs/2026-07-06-ezviz-real-service-design.md`
  to `docs/source-material/ezviz-real-service-design.md`.
- A new `docs/source-material/VTM_PORTING_NOTES.md` summarizing the Apache-2.0
  Python source files and the exact behavior ported to Rust.

The repository does not copy `hikconnect.env`, raw MPEG-PS, MP4/JPEG samples,
APK contents, wheel files, or `/tmp` virtual environments.
