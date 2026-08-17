# EZVIZ/Hik-Connect Real Test Service Design

## Goal

Create a runnable Rust test project under `test/ezviz_real_service/` that connects to a real EZVIZ/Hik-Connect camera using the private P2P protocol, driven by a clear `config.yaml`.

The service is a test harness, not a production Media Node integration. It should prove that the Rust implementation can:

- Log in to the Hik-Connect/EZVIZ account API.
- Resolve per-device P2P/KMS/CONNECTION details.
- Establish the UDP P2P session.
- Complete the device SRT-style handshake.
- Read live media frames.
- Extract H.265 Annex-B video and PCMA audio into local output files or frame stats.

## References

- `/home/hoangbim/go2rtc/pkg/ezviz`: working Go implementation to port.
- `ezviz_mock.rs`: current Rust mock/extractor sketch in this repo.

The Rust project should follow the Go protocol behavior closely instead of inventing a new protocol flow.

## Project Layout

```text
test/ezviz_real_service/
  Cargo.toml
  config.example.yaml
  src/
    main.rs
    config.rs
    error.rs
    api.rs
    v3.rs
    client.rs
    session.rs
    hikrtp.rs
    frame.rs
  tests/
    hikrtp_tests.rs
    v3_tests.rs
```

This crate should stay independent from the root workspace so it can be run from `test/ezviz_real_service` without affecting existing service builds.

## Config

`config.example.yaml` must be explicit and copyable:

```yaml
source:
  account: "your-account"
  password: "your-password"
  api_host: "api.hik-connect.com"
  serial: "CAMERA_SERIAL"
  channel: 1
  subtype: "main"

runtime:
  connect_timeout_secs: 15
  frame_limit: 200
  output_dir: "out"
  output_h265: "stream.h265"
  output_pcma: "audio.pcma"
  log_packets: false
```

The binary reads `--config config.yaml`. It should fail with a clear error if credential, serial, channel, or subtype fields are missing or invalid.

## Architecture

`api.rs` ports Go `api.go`:

- `POST /v3/users/login/v2` with lowercase MD5 password.
- `GET /v3/userdevices/v1/resources/pagelist?groupId=-1&limit=50&offset=N&filter=P2P,KMS,CONNECTION`.
- `POST /api/p2p/configurations`.
- Parse `sessionId`, regional `apiDomain`, P2P servers, KMS secret, key version, WAN/NET stream endpoint, rotating P2P key, salt index, and salt version.
- Extract user id from the `aud` claim of the session JWT.

`v3.rs` ports Go `v3.go`:

- V3 frame header.
- TLV encode/decode.
- CRC8.
- AES-128-CBC with fixed IV and PKCS#7 padding.
- Expand-header PLAY_REQUEST support.

`session.rs` ports Go `session.go`:

- UDP bind.
- P2P_SETUP to cloud P2P servers.
- Hole punch handling: `0x0C00` request and repeated `0x0C01` response.
- Direct and relayed PLAY_REQUEST.
- Session setup `0x7534`.
- SRT induction/conclusion handshake.
- SRT ACK and keepalive ticker.
- Separate routing for control keepalives `0x807f` and video data sequence numbers.
- Reorder buffer before feeding Hik-RTP data to the extractor.

`hikrtp.rs` ports Go `hikrtp.go` and keeps the useful Rust extractor behavior from `ezviz_mock.rs`:

- Strip Hik-RTP 12-byte header.
- Strip optional 13-byte sub-header.
- Extract PCMA audio when sub-header byte 2 is `0x88`.
- Reassemble HEVC FU type 49 into complete Annex-B NAL units.
- Handle length-prefixed Hikvision NAL payloads.

`main.rs` is the runnable harness:

- Load config.
- Connect to camera.
- Read frames until `frame_limit` is reached or the session closes.
- Write H.265 frames to `output_dir/output_h265`.
- Write PCMA frames to `output_dir/output_pcma`.
- Print concise frame counters and remote endpoint.

## Error Handling

Errors should include the failed protocol stage:

- `login`
- `p2p config`
- `p2p secret`
- `p2p setup`
- `hole punch`
- `play request`
- `srt handshake`
- `read frame`

Network timeouts should be configurable only where useful. The first implementation should use the Go defaults: 15 seconds for HTTP and data-session startup, 10 seconds for punch wait, and 100 ms reorder flush.

## Testing

Offline tests should cover:

- URL/config validation.
- P2P secret byte-array parsing, including signed byte values.
- JWT `aud` extraction.
- V3 CRC8, TLV encode/decode, AES encrypt/decrypt round-trip.
- Hik-RTP extraction for VPS/SPS/PPS, length-prefixed NALs, HEVC FU start/continue/end, incomplete FU discard, and PCMA audio extraction.

Live camera testing is manual:

```sh
cd test/ezviz_real_service
cp config.example.yaml config.yaml
# edit config.yaml
cargo test
cargo run -- --config config.yaml
```

Success means the process connects, receives frames, writes non-empty `.h265` and optionally `.pcma` outputs, and exits after `frame_limit`.

## Out Of Scope

- Integrating this into `services/media-node`.
- Publishing RTSP/HLS/WebSocket output.
- Playback/recording bus type support.
- Camera control, PTZ, talkback, or snapshot.
- Storing real secrets in the repo.
