# Hướng dẫn triển khai xem live Hik-Connect trên app Android / iOS

Tài liệu này mô tả cách đưa pipeline VTM của `hik-adapter` vào một **app nội bộ
Android và iOS** để xem trực tiếp camera Hik-Connect, không cần server trung gian.

> Phạm vi: xem live (live view). Ghi hình/phát lại vẫn dùng adapter hiện tại
> (FFmpeg + HTTP Range). Đây là kế hoạch triển khai, chưa có code mobile nào.

---

## 1. Kiến trúc mục tiêu

```
┌──────────────────────── App (Android / iOS) ────────────────────────┐
│  UI: Kotlin (Jetpack Compose) / Swift (SwiftUI)                     │
│     │  login / listCameras / startLive / stopLive                   │
│     ▼                                                               │
│  Binding sinh tự động bởi UniFFI (Kotlin + Swift)                   │
│     │                                                               │
│     ▼                                                               │
│  hik-core (Rust, .so / .xcframework)                                │
│     ApiClient.login → list_cameras_with_shared → resolve_vtm        │
│     → VtmSession.connect/start → for_each_media → MediaPipe         │
│     → callback(frame Annex-B, codec, pts)                           │
│     │                                                               │
│     ▼                                                               │
│  Decoder phần cứng                                                  │
│     Android: MediaCodec (video/hevc | video/avc) → SurfaceView      │
│     iOS:     CMSampleBuffer → AVSampleBufferDisplayLayer            │
└─────────────────────────────────────────────────────────────────────┘
          │ HTTPS (API Hik-Connect)          │ TCP (relay VTM)
          ▼                                  ▼
     api.hik-connect.com …              VTM/VTDU relay
```

Vì sao chọn hướng này:

- `MediaPipe::accept` (`src/vtm/media.rs`) đã trả ra **từng NAL unit H.265/H.264
  có start code Annex-B** (`00 00 00 01`). Đây chính là đầu vào của decoder phần
  cứng, nên **không cần FFmpeg trên mobile**.
- Android và iOS đều giải mã HEVC bằng phần cứng, nên không vướng giới hạn HEVC
  của trình duyệt.
- `reqwest` đang dùng `rustls`, không phụ thuộc OpenSSL, nên cross-compile cho
  mobile dễ.
- Code mạng hiện tại là **blocking** (`reqwest::blocking` + TCP đồng bộ), chạy tốt
  trên một thread nền riêng. Không cần chuyển sang async.

Hướng thay thế (không chọn): giữ server Rust, thêm đầu ra LL-HLS/RTSP, app chỉ là
player. Nhanh hơn nhưng phải có server chạy 24/7 và độ trễ cao (2–6 s). Chỉ nên
dùng nếu không muốn để credential Hik-Connect trên điện thoại.

---

## 2. Hiện trạng code cần biết

| Thành phần | File | Ghi chú cho mobile |
|---|---|---|
| Login mobile/web | `src/api/auth.rs`, `src/api/mod.rs` | `ApiClient::new` mặc định client type `55` (mobile). Giữ nguyên. |
| Danh sách camera | `src/api/cameras.rs` | `list_cameras_with_shared` gộp camera sở hữu + được share. `device_encrypt_flags` trả cờ `isEncrypt` (chỉ có với camera sở hữu). |
| Resolve VTM | `src/api/vtm.rs` | `resolve_vtm` → `VtmContext { relay_host, relay_port, stream_url }`. **`build_vtm_url` đang cố định `stream=2` (sub-stream)** — cần thêm tham số chọn main/sub. |
| Phiên VTM | `src/vtm/session.rs` | `VtmSession::connect` → `start(3)` → `for_each_media(duration, no_media_timeout, …, shutdown, sink)`. Đã tự gửi keepalive mỗi 5 s. |
| Tách media | `src/vtm/media.rs` | `MediaPipe` tự nhận transport: `RtpH265`, `RtpH264`, `MpegPs`. Gặp stream mã hóa thì trả lỗi `EncryptedStream`. |
| Phân loại stream | `src/runtime/probe.rs` | Dùng để báo trước cho UI là camera nào xem được. |
| Ghi MP4, HTTP server | `src/recording/`, `src/server/`, `src/runtime/` | **Không đưa vào mobile.** |

Các loại stream thực tế (đo trên 180 kênh, 08/2026):

| Loại | Tỉ lệ ước tính | Xem trên app được không |
|---|---|---|
| H.265 thô qua RTP PT96 (`clear_h265`) | ~40/180 | Được — đưa thẳng vào decoder |
| H.264 thô qua RTP PT96 (`h264`) | một phần | Được |
| MPEG-PS (`mpeg_ps`, thiết bị cũ) | ít | Cần thêm demuxer PS → ES (xem mục 4.3) |
| Mã hóa (`encrypted`, `isEncrypt=1`) | ~88/180 | **Chưa** — cần làm ECDH + giải mã (mục 7) |
| Relay từ chối (5404/5405/5452) | tùy tải | Retry/backoff, giảm số kết nối đồng thời |

RTP PT112 (metadata) và PT0 (filler/audio) đã bị bỏ qua. Giữ nguyên hành vi này.

---

## 3. Bước 1 — Tách crate `hik-core`

Chuyển repo thành Cargo workspace:

```
hik-adapter/
├── Cargo.toml              # [workspace] members = ["crates/*"]
└── crates/
    ├── hik-core/           # lib: api/, vtm/, model.rs, error.rs  (KHÔNG có axum, clap, ffmpeg)
    ├── hik-adapter/        # bin hiện tại: CLI + recording + server, phụ thuộc hik-core
    └── hik-mobile/         # cdylib/staticlib + UniFFI, phụ thuộc hik-core
```

Việc cần làm:

1. Chuyển `src/api/`, `src/vtm/`, `src/model.rs`, `src/error.rs` sang
   `crates/hik-core`. Bỏ phụ thuộc `axum`, `clap`, `ctrlc`, `tokio` khỏi
   core nếu không dùng.
2. **Thêm timestamp vào callback media.** Hiện tại `for_each_media` gọi
   `sink(transport, &[u8])` và bỏ mất RTP timestamp. Decoder trên mobile cần PTS.
   Đổi thành dạng:

   ```rust
   pub struct MediaFrame<'a> {
       pub transport: MediaTransport,
       pub data: &'a [u8],        // 1 NAL unit, có start code Annex-B
       pub rtp_timestamp: Option<u32>, // clock 90 kHz; None với MPEG-PS
       pub marker: bool,          // bit M của RTP = NAL cuối của access unit
   }
   ```

   Đọc timestamp ở `body[4..8]` (big-endian) và marker ở `body[1] & 0x80` của
   header RTP, cùng chỗ `rtp_payload()` đang cắt 12 byte header.
3. **Cho phép live không giới hạn thời gian:** truyền `media_duration =
   Duration::MAX`, chỉ dừng qua cờ `shutdown`. Kiểm tra lại phép so sánh
   `started.elapsed() >= media_duration` có hoạt động đúng với giá trị này.
4. **Tham số chọn luồng:** thêm `stream: u8` (1 = main, 2 = sub) vào
   `resolve_vtm` / `build_vtm_url` thay cho giá trị cố định `stream=2`.
5. Chạy lại toàn bộ test hiện có (`cargo test --release`) sau khi tách.

### Kiểm thử trên desktop trước khi làm mobile

Thêm lệnh CLI `live` vào `hik-adapter`, xuất Annex-B ra stdout:

```bash
cargo run --release -- live --config config.yaml --serial GF4563623 --channel 1 \
  | ffplay -f hevc -fflags nobuffer -flags low_delay -
```

Nếu `ffplay` xem được mượt thì phía Rust đã đúng. Mọi lỗi sau này chỉ nằm ở
binding hoặc decoder mobile.

---

## 4. Bước 2 — Lớp `hik-mobile` (UniFFI)

### 4.1 API đề xuất

```rust
// crates/hik-mobile/src/lib.rs
uniffi::setup_scaffolding!();

#[derive(uniffi::Record)]
pub struct CameraInfo {
    pub serial: String,
    pub channel: u16,
    pub name: String,
    pub online: bool,
    pub stream_class: String,       // clear_h265 | h264 | mpeg_ps | encrypted | unknown
    pub device_encrypted: Option<bool>,
}

#[derive(uniffi::Enum)]
pub enum VideoCodec { H265, H264 }

#[uniffi::export(with_foreign)]
pub trait LiveListener: Send + Sync {
    fn on_frame(&self, codec: VideoCodec, nal: Vec<u8>, pts_us: i64, end_of_au: bool);
    fn on_error(&self, category: String, message: String);
    fn on_stopped(&self);
}

#[derive(uniffi::Object)]
pub struct HikClient { /* ApiClient + AuthSession */ }

#[uniffi::export]
impl HikClient {
    #[uniffi::constructor]
    pub fn login(base_url: String, username: String, password: String) -> Result<Arc<Self>, HikError>;
    pub fn list_cameras(&self) -> Result<Vec<CameraInfo>, HikError>;
    /// Chạy trên thread nền riêng, trả handle để dừng.
    pub fn start_live(&self, serial: String, channel: u16, main_stream: bool,
                      listener: Arc<dyn LiveListener>) -> Result<Arc<LiveHandle>, HikError>;
}

#[derive(uniffi::Object)]
pub struct LiveHandle { shutdown: Arc<AtomicBool>, /* JoinHandle */ }

#[uniffi::export]
impl LiveHandle {
    pub fn stop(&self);   // set shutdown = true, join thread
}
```

Quy tắc:

- `start_live` tạo một `std::thread` riêng cho mỗi camera, giống
  `runtime/camera_worker.rs`. Tuyệt đối không chạy trên main/UI thread.
- Mỗi lần xem live gọi lại `resolve_vtm`, vì token VTDU chỉ dùng được ngắn hạn.
- `pts_us` tính từ RTP timestamp: `(ts - ts_first) * 1_000_000 / 90_000`, có xử
  lý tràn số 32-bit.
- Callback `on_frame` phải trả về nhanh. Phía Kotlin/Swift chỉ đưa frame vào hàng
  đợi, không decode ngay trong callback.
- Lỗi trả ra ngoài dùng `ErrorCategory` có sẵn (`CameraOffline`,
  `PermissionDenied`, `EncryptedStream`, `VtmNegotiationFailed`, …) và **không bao
  giờ** đưa token, session id hay URL VTM vào message.

### 4.2 Build

**Android** (cần NDK r26+):

```bash
cargo install cargo-ndk
rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 \
  -o android/app/src/main/jniLibs build --release -p hik-mobile
cargo run -p uniffi-bindgen -- generate --library \
  target/aarch64-linux-android/release/libhik_mobile.so \
  --language kotlin --out-dir android/app/src/main/java
```

Kotlin binding cần dependency `net.java.dev.jna:jna:<version>@aar`.

**iOS** (cần macOS + Xcode):

```bash
rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
cargo build --release -p hik-mobile --target aarch64-apple-ios
cargo build --release -p hik-mobile --target aarch64-apple-ios-sim
cargo run -p uniffi-bindgen -- generate --library \
  target/aarch64-apple-ios/release/libhik_mobile.a --language swift --out-dir ios/Generated
xcodebuild -create-xcframework \
  -library target/aarch64-apple-ios/release/libhik_mobile.a -headers ios/Generated \
  -library target/aarch64-apple-ios-sim/release/libhik_mobile.a -headers ios/Generated \
  -output ios/HikCore.xcframework
```

Cấu hình `crate-type = ["cdylib", "staticlib"]` trong `crates/hik-mobile/Cargo.toml`.
Luôn build **release**: bản debug từng bị panic trong `reqwest`.

### 4.3 MPEG-PS

Với transport `MpegPs`, `MediaPipe` trả ra nguyên gói PS, không phải NAL. Trên
mobile phải tự tách PS → PES → elementary stream (đọc pack header `00 00 01 BA`,
system/PSM, rồi PES `00 00 01 E0`, lấy PTS từ PES header) trong `hik-core`, để
output vẫn là Annex-B. Đây là phần ít camera dùng, có thể làm sau và tạm báo
"không hỗ trợ" trên UI.

---

## 5. Bước 3 — Android

1. **Lưu credential:** dùng `EncryptedSharedPreferences` hoặc Android Keystore.
   Không log, không đưa vào Intent extras.
2. **Mạng:** cần permission `INTERNET`. Gọi `login`/`listCameras` trong
   `Dispatchers.IO`.
3. **Decoder:**
   - Chờ đến khi nhận đủ VPS (NAL type 32), SPS (33), PPS (34) với H.265, hoặc
     SPS (7), PPS (8) với H.264. Kiểu NAL H.265 là `(byte[4] >> 1) & 0x3F` (sau
     start code 4 byte); H.264 là `byte[4] & 0x1F`.
   - `MediaFormat.createVideoFormat("video/hevc" | "video/avc", w, h)`. Đặt
     `csd-0` = VPS+SPS+PPS ghép liền, mỗi cái có start code (H.264: `csd-0` = SPS,
     `csd-1` = PPS). Lấy `w`/`h` từ SPS, hoặc đặt tạm 1920×1080 vì decoder sẽ báo
     `INFO_OUTPUT_FORMAT_CHANGED`.
   - `configure(format, surfaceView.holder.surface, null, 0)`, rồi `start()`.
   - Ghép các NAL của cùng một access unit (đến khi `end_of_au = true`) thành một
     buffer rồi gọi `queueInputBuffer(index, 0, size, pts_us, flags)`. Dùng
     `BUFFER_FLAG_KEY_FRAME` khi có IDR (H.265 NAL type 19/20, H.264 type 5).
   - Vòng output: `dequeueOutputBuffer` → `releaseOutputBuffer(index, true)` để
     render ngay (live, không cần đồng bộ theo PTS).
4. **Vòng đời:** `onPause`/`onStop` gọi `LiveHandle.stop()` và
   `MediaCodec.stop()/release()`. Mở lại khi `onResume`.
5. **Bỏ frame cho đến keyframe đầu tiên** để tránh màn hình xám/vỡ hình.
6. **Phân phối nội bộ:** APK ký bằng keystore riêng, cài qua MDM hoặc Firebase App
   Distribution.

## 6. Bước 4 — iOS

1. **Lưu credential:** Keychain (`kSecClassGenericPassword`,
   `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`).
2. **Format description:**
   - H.265: `CMVideoFormatDescriptionCreateFromHEVCParameterSets` với VPS, SPS, PPS
     (bỏ start code), `nalUnitHeaderLength = 4`.
   - H.264: `CMVideoFormatDescriptionCreateFromH264ParameterSets` với SPS, PPS.
3. **Chuyển Annex-B sang AVCC/HVCC:** mỗi NAL đổi start code `00 00 00 01` thành
   độ dài 4 byte big-endian. Các NAL của một access unit ghép vào một
   `CMBlockBuffer`, tạo `CMSampleBuffer` với PTS từ `pts_us`.
4. **Hiển thị:** gắn
   `kCMSampleAttachmentKey_DisplayImmediately = true` cho mỗi sample rồi
   `AVSampleBufferDisplayLayer.enqueue(_:)`. Nếu `layer.status == .failed` thì
   gọi `flush()` và chờ keyframe tiếp theo.
   Nếu cần xử lý frame (chụp ảnh, overlay) thì dùng `VTDecompressionSession`.
5. **Background:** iOS ngắt TCP khi app vào nền. Dừng live ở
   `sceneDidEnterBackground`, mở lại khi active.
6. **Phân phối nội bộ:** TestFlight (internal testers) hoặc Ad Hoc, cần tài khoản
   Apple Developer. Không cần App Store.

---

## 7. Camera mã hóa (việc chặn ~50% kênh)

Đây là việc riêng, làm song song, và **không phụ thuộc** mobile hay server.

- Nhận biết: RTP PT96 bắt đầu bằng prefix 12 byte
  `40 00 00 02 80 06 00 01 11 21 02 01` (`ENCRYPTED_CONTAINER_PREFIX` trong
  `src/vtm/media.rs`), hoặc packet đi trên `Channel::EncryptedStream` /
  `Channel::EncryptedMessage`. Chỉ các P-frame TRAIL_R là không bị mã hóa.
- Với camera sở hữu, `GET /v3/userdevices/v1/devices/pagelist` trả
  `statusInfos.<serial>.isEncrypt`. Giá trị `1` chắc chắn là stream mã hóa.
- Cần làm: trao đổi khóa ECDH của VTM (`VtmStreamEcdhNotify`, mã `0x014a`) và giải
  mã payload. Tham khảo cách làm của `pyezvizapi` để port sang Rust. Khóa ứng viên:
  `statusInfos.<serial>.encryptPwd` và `kmsInfos.<serial>.secretKey`.
- Bằng chứng: chạy adapter với `stream_capture: true` để dump packet ra
  `recordings/*/capture/*.bin` (định dạng `[u32 LE len][body]`).
- Trong lúc chưa có: UI phải hiện rõ "Camera mã hóa — chưa hỗ trợ" thay vì
  spinner vô hạn (dựa vào `stream_class` hoặc lỗi `EncryptedStream`).

---

## 8. Bảo mật

- Credential Hik-Connect chỉ nằm trong Keystore/Keychain. Không hardcode, không
  log, không gửi về server nào khác.
- `SecretString` của `secrecy` đã che token/URL VTM trong `Debug`. Giữ quy tắc đó
  ở lớp binding: không có hàm nào trả `stream_url` hoặc session id ra Kotlin/Swift.
- Bật R8/ProGuard cho Android. Chỉ build release cho bản phát hành nội bộ.
- Tài khoản dùng nên là tài khoản nhận share chỉ có quyền live view.

## 9. Giới hạn và rủi ro vận hành

- Mỗi điện thoại tự mở một kết nối relay riêng, nên nhiều người xem cùng camera
  sẽ tốn nhiều slot relay. Relay đã từng trả 5404/5405/5452 khi tải cao: cần retry
  có backoff (ví dụ 1 s, 2 s, 5 s, tối đa 3 lần) và giới hạn số ô xem đồng thời
  (khuyến nghị ≤ 4 camera/màn hình, dùng sub-stream cho lưới, main-stream khi xem
  1 camera).
- Đăng nhập trên app có thể gặp `CaptchaRequired`: hiện chưa xử lý, phải báo người
  dùng đăng nhập lại bằng app Hik-Connect chính thức.
- API/relay là giao thức không công khai của Hik-Connect, có thể đổi bất cứ lúc
  nào. Đây là bản nội bộ, không có cam kết ổn định.

## 10. Thứ tự triển khai và tiêu chí nghiệm thu

| # | Việc | Nghiệm thu |
|---|---|---|
| 1 | Tách workspace `hik-core`, thêm `MediaFrame` có PTS, tham số main/sub | `cargo test --release` pass; các lệnh CLI cũ `list` và `run` vẫn chạy |
| 2 | Lệnh CLI `live` → stdout | `ffplay -f hevc -` xem được `GF4563623` ch1 ổn định ≥ 10 phút |
| 3 | `hik-mobile` + UniFFI, build `.so` / `.xcframework` | App mẫu login và hiện danh sách camera kèm `stream_class` |
| 4 | Android MediaCodec player | Xem live H.265 và H.264; trễ < 1,5 s; xoay màn hình / vào nền không crash |
| 5 | iOS AVSampleBufferDisplayLayer player | Như mục 4 trên iPhone thật |
| 6 | Retry/backoff, lưới nhiều camera, UI báo lỗi theo `ErrorCategory` | 4 camera đồng thời trên cả hai nền tảng |
| 7 | (Song song) ECDH + giải mã stream mã hóa | Xem được ít nhất một camera `isEncrypt=1`, ví dụ `FF8803139` |
| 8 | (Tùy chọn) Demux MPEG-PS trong core | Xem được camera transport `mpeg_ps` |

Camera tham chiếu để test: `GF4563623`, `FQ9073314` (H.265 không mã hóa),
`629192394` (H.264), `FF8803139` (mã hóa).
