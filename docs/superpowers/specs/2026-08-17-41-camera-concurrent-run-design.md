# 41-Camera Concurrent VTM Run Design

## Goal

Reuse the existing `hik-adapter run` pipeline to test every camera/channel
returned by the current Hik-Connect recipient inventory in one concurrent run.

## Scope

- Raise the configured and runtime worker limit from 10 to 41.
- Keep one named OS thread per camera/channel.
- Login and fetch inventory once, then select all 41 current entries.
- Keep the existing per-worker pipeline unchanged: VTM resource, VTDU token,
  TCP negotiation, MPEG-PS transport lock, FFmpeg MP4 recording, validation,
  and HTTP publication.
- A camera that reaches unencrypted MPEG-PS records for 30 seconds.
- Resource, token, negotiation, encrypted-channel, unsupported-transport, or
  no-media failures terminate that camera worker immediately.
- One camera failure must not cancel other workers.

## Evidence and Classification

After all workers finish, capture the `/api/cameras` snapshot and sanitized
worker logs into ignored files under `recordings/`. The report separates:

- `completed`: validated 30-second MP4 with media counters/metadata;
- `encrypted_stream`: encrypted VTM control or media channel observed;
- `no_media`: negotiation succeeded but no media arrived in time;
- `vtm_negotiation_failed`: includes the relay result code when available;
- resource, token, transport, FFmpeg, validation, cancellation, and global API
  failures as their existing stable error categories.

Relay codes such as `5404` remain evidence, not an inferred permission verdict.
The final analysis may state plausible causes (offline camera, share permission,
or relay policy) but must distinguish inference from observed protocol state.

## Safety

Credential values, session IDs, VTDU tokens, stream URLs, and encrypted packet
bodies remain excluded from logs and reports. Successful MP4s stay mode `0600`
inside the ignored `recordings/` directory. The HTTP server remains bound to
the configured port after worker completion for review.

## Verification

Before the live run, formatting, Clippy with warnings denied, unit tests, and
integration tests must pass. The live run is successful as an experiment when
all 41 inventory entries reach a terminal state and a non-secret report records
the outcome of every entry, regardless of how many streams complete.
