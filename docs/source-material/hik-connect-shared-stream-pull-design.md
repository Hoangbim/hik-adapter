# Hik-Connect Shared Stream Pull Design

## Objective

Pull and verify a 5–10 second video sample from the Hik-Connect shared device
`GK8814993` (`shareId=128971859`) using the credentials stored in the ignored
file `private/camlist/hikconnect.env`.

## Scope

- One-off diagnostic only; do not integrate Hik-Connect into Ermis NVR.
- The device is shared with the account, not owned by it.
- Prefer Hik-Connect P2P, but use the consumer-app VTM relay when UDP P2P/SRT
  cannot establish a media session. `natPort` and the advertised WAN RTSP port
  are both unavailable for this device.
- Run experimental third-party code outside the repository under `/tmp`.
- Never print credentials, session tokens, cookies, or verification codes.

## Recipient Bootstrap

Authenticate with the Hik-Connect consumer account that received the share.
Do not use Hik-Partner Pro credentials or owner-only device pagination.

Fetch data directly by serial with the recipient session token:

- P2P: `GET /v3/userdevices/v1/devices/p2p?deviceSerials=GK8814993`
- KMS: `GET /v3/userdevices/v1/devices/kms?deviceSerials=GK8814993`
- Combined recipient resource: `GET /v3/userdevices/v1/resources/GK8814993?filter=P2P,KMS,CONNECTION`
- VTM resource mapping: `GET /v3/userdevices/v1/resources/GK8814993?filter=VTM`
- Current VTM relay: `GET /v3/streaming/vtm/GK8814993/1`
- Stream ticket: `GET /v3/streaming/ticket/GK8814993/1`

The direct `devices/connection` endpoint returns an HTTP error for this shared
device, while the direct resource endpoint returns the same CONNECTION data as
part of its successful payload. Treat P2P/KMS/tickets as short-lived secrets
and never persist them in logs or repository files.

## Flow

1. Authenticate from `hikconnect.env` as the Hik-Connect recipient account.
2. Address shared serial `GK8814993`, channel 1, directly rather than relying
   on the owned-device page list.
3. Try the experimental go2rtc P2P transport using recipient P2P/KMS/CONNECTION
   data.
4. When P2P/SRT does not start, request the recipient VTM resource mapping,
   current relay and VTDU token, then open the VTM TCP media session.
5. Lock the media transport to MPEG-PS from its initial pack header and preserve
   subsequent fragments verbatim. Per-fragment transport detection can mistake
   PS fragments for RTP/TS and corrupt the stream.
6. Remux only the HEVC video program to MPEG-TS, excluding the malformed/unknown
   audio program.
7. Record 10 seconds to `private/camlist/pull_samples/` and validate it with
   `ffprobe` and a decoded frame.

## Failure Handling

- Stop and report if the account cannot access the share, a CAPTCHA or
  verification code is required, UDP P2P negotiation fails, or the device is
  offline.
- Do not fall back to password guessing, broad port scanning, or changes to the
  remote router/device.

## Result

Completed on 2026-08-13 using the VTM relay path available to the recipient:

- sample: `private/camlist/pull_samples/GK8814993-vtm-20260813.ts`
- validation frame: `private/camlist/pull_samples/GK8814993-vtm-20260813-frame.jpg`
- container: MPEG-TS
- video: HEVC Main, 1280x720, 20 fps
- duration: 9.9 seconds
- file mode: `0600`

The P2P/KMS/CONNECTION bootstrap succeeded, but the tested P2P implementation
did not receive an SRT media session within 15 seconds. VTM negotiation returned
result 0 and delivered an unencrypted MPEG-PS video stream.

## Detailed Development Flow

### 1. Load configuration

Read the ignored environment file `private/camlist/hikconnect.env` at process
startup. Required values are the Hik-Connect consumer username and password.
Do not pass either value in command-line arguments, URLs, logs, exceptions or
metrics. Keep the values in memory only for the login request.

Input camera identity is a serial/channel pair:

```text
deviceSerial = GK8814993
channelNo    = 1
shareId      = 128971859     # inventory/debug metadata only
```

`shareId` is not a replacement for `deviceSerial` in streaming endpoints.

### 2. Login as the recipient account

Send the password as the MD5 hex digest expected by the consumer API:

```text
POST https://api.hik-connect.com/v3/users/login/v2
form account, password=md5(plain_password), featureCode
```

Preserve the returned `sessionId`, `rfSessionId` and `loginArea.apiDomain` in a
short-lived session object. If `meta.code == 1100`, replace the API base URL
with `https://{loginArea.apiDomain}` and retry login once. Every subsequent
request uses the recipient `sessionId` and the same client identity headers.

### 3. Resolve shared-device authorization

Never infer ownership from the device list. Query the serial directly:

```text
GET /v3/userdevices/v1/resources/{deviceSerial}?filter=P2P,KMS,CONNECTION
```

Require all of the following before opening a stream:

- HTTP/meta success (`meta.code == 200`)
- one matching `deviceInfos[]` item
- `deviceShareInfo.isShared == 2` or equivalent shared-recipient state
- `isOwner == -1` for the known shared case
- a matching camera channel in `cameraInfos[]` or `resourceInfos[]`
- live-view permission in the device/camera share permission fields

The response provides the recipient-scoped `P2P`, `KMS` and `CONNECTION`
objects. Treat the KMS `secretKey`, stream tickets and session IDs as secrets;
they are runtime material, not persistent camera configuration.

### 4. Optional P2P attempt

Use the recipient-scoped P2P/KMS/CONNECTION data, not owner data. The tested
P2P path is:

```text
P2P server list → KMS key/version → device connection metadata → SRT session
```

Attempt P2P only for a bounded timeout (15 seconds in the experiment). Mark it
as unavailable when the device does not start media, `netStreamPort == 0`, or
the UDP/NAT path is blocked. Do not retry indefinitely and do not scan the
advertised public IP for unrelated ports.

### 5. VTM fallback for remote/shared viewing

The VTM path matches the Hik-Connect consumer mobile app and is the primary
working path for this camera:

```text
GET /v3/userdevices/v1/resources/{serial}?filter=VTM
GET /v3/streaming/vtm/{serial}/{channelNo}
POST /api/user/token/get                 # VTDU token(s)
GET /v3/streaming/ticket/{serial}/{channelNo}   # if the client requires ticket
```

Use `resourceInfos[]` to map the camera channel to `resourceId`,
`streamBizUrl` and the VTM resource. The current VTM response supplies relay
host/port and a public key. The client then builds the VTM stream URL with:

```text
device serial + streamBizUrl + VTDU token + channel + client type + stream type
```

The VTM TCP session lifecycle is:

1. connect to relay;
2. send stream-info request;
3. accept `STREAMINFO_RSP` and follow a bounded redirect if returned;
4. require negotiation result `0` (or the library's successful null result);
5. iterate media packets until cancellation/deadline;
6. close the socket and invalidate runtime tokens.

### 6. Preserve the media transport

The first media packet for this camera begins with an MPEG-PS pack header
(`00 00 01 BA`). Lock the session transport from this initial header. Do not
run independent transport detection on every fragment: PS fragments can be
misclassified as RTP or MPEG-TS, which corrupts the video.

For this camera the media characteristics are:

```text
transport = MPEG-PS
video     = HEVC Main
size      = 1280x720
rate      = 20 fps
audio     = inconsistent/invalid MP2 packets; exclude from output
```

The safe first implementation is to preserve PS packets, feed them to FFmpeg
with `-f mpeg`, select `0:v:0`, and remux to MPEG-TS or the NVR's native H.265
ingest format. Do not fail the video stream because of malformed audio packets.

### 7. NVR integration boundary

Keep Hik-Connect protocol code behind a source adapter with these operations:

```text
login(credentials) -> session
resolve_recipient_device(serial, channel) -> SharedCameraContext
open_stream(context, profile) -> AsyncPacketStream
close_stream(stream)
```

`SharedCameraContext` should contain only non-secret identity/transport fields
plus in-memory runtime handles. It should not be serialized to camera config or
database. The adapter should expose already-normalized H.265 access units to
the existing media-node ingest pipeline, or expose a local RTSP/HLS bridge if
the ingest boundary requires a network URL.

### 8. Refresh and failure rules

- On HTTP 401/session expiry: login again, re-resolve the recipient resource,
  request fresh VTM metadata/token, then reopen the stream.
- On VTM redirect: follow at most three redirects per session.
- On no media packets: close the session and report `device offline`,
  `permission denied`, or `relay negotiation timeout` distinctly.
- On stream-type failure: try substream first (`stream=2`), then main stream
  (`stream=1`) only when the camera advertises it.
- On process shutdown: close sockets, terminate FFmpeg, and remove only
  short-lived ticket/token files. Never delete the user's credential file.

### 9. Regression checks for future development

Every implementation change should verify:

1. recipient login succeeds without exposing secrets;
2. direct-resource lookup returns the shared camera and channel 1;
3. P2P failure falls back to VTM within the configured timeout;
4. VTM negotiation returns success;
5. at least five seconds of non-empty H.265 video is produced;
6. `ffprobe` reports HEVC and a valid width/height/frame rate;
7. the generated output contains no KMS key, ticket, password or session ID;
8. reconnect after token/session refresh works without process restart.
