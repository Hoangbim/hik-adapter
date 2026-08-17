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

The HTTP server binds to `127.0.0.1:8787` by default. Binding to
`0.0.0.0:8787` is intended only for a trusted LAN/VPN or an authenticated
reverse proxy. Public authentication and TLS are outside this feasibility phase.

The output remains HEVC. Browser playback depends on platform HEVC support;
VLC and other HEVC-capable clients can use the same Range endpoint.
