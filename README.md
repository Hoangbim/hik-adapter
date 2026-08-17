# hik-adapter

Rust feasibility adapter for Hik-Connect VTM relay streams. It logs in with a
recipient account, lists camera channels, records up to ten cameras on distinct
OS threads, validates MP4 output, and serves completed files over HTTP Range.

## Prerequisites

- Rust 1.96 or newer
- FFmpeg and ffprobe on `PATH`
- A Hik-Connect consumer account with live-view permission

## Configuration

```bash
cp config.example.yaml config.yaml
export HIKCONNECT_USERNAME='recipient-account'
export HIKCONNECT_PASSWORD='recipient-password'
```

Credentials are read from environment variables only. Never place them in YAML,
CLI arguments, URLs, logs, or committed files.

## Commands

```bash
cargo run --release -- list --config config.yaml
cargo run --release -- run --config config.yaml
```

The example binds `server.bind: 0.0.0.0:8787`; open `http://HOST:8787/` from
another machine. Expose it only on a trusted LAN/VPN or behind an authenticated
reverse proxy. Public authentication and TLS are outside this feasibility phase.

The output remains HEVC. Browser playback depends on platform HEVC support;
VLC and other HEVC-capable clients can use the same Range endpoint.

Each selected camera gets one named OS thread. A camera failure does not stop
the other recordings, so partial success is expected during feasibility runs.
Completed files are mode `0600` inside a mode `0700` output directory. Remove
the `recordings/` directory when the feasibility evidence is no longer needed;
use `run --overwrite` only when intentionally replacing prior outputs.

## Live feasibility result

On 2026-08-17, direct target `GK8814993` channel 1 completed through the Rust
pipeline: Hik-Connect login, direct VTM resource, VTDU token, TCP negotiation,
MPEG-PS ingestion, FFmpeg remux, ffprobe/decode validation, and HTTP Range.
The resulting MP4 was 29.998 seconds, HEVC 1280x720 at 20 fps, and 1,569,009
bytes. This is feasibility evidence, not a production availability guarantee.
