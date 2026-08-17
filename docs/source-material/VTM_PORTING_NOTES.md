# VTM porting notes

This Rust port implements only the unencrypted VTM live-stream subset required
by the feasibility design.

| Rust behavior | Apache-2.0 reference |
|---|---|
| 8-byte VTM header | `pyezvizapi.stream.encode_vtm_packet` |
| Minimal protobuf fields | `build_stream_info_request`, `parse_stream_info_response` |
| TCP negotiation and redirect | `VtmStreamClient.start` |
| Keepalive and media iteration | `VtmStreamClient.iter_packets` |
| VTM REST bootstrap | `hikcloudstream.stream.session.build_cloud_stream_info` |
| VTDU token fallback | `hikcloudstream.stream.tokens.get_vtdu_tokens` |
| `vtm::build_stream_info_request` | `pyezvizapi.stream.build_stream_info_request` |
| `vtm::parse_stream_info_response` | `pyezvizapi.stream.parse_stream_info_response` |
| `vtm::TransportLock::accept` | `pyezvizapi.stream.VtmStreamClient.iter_media` |
| `api::build_vtm_url` | `pyezvizapi.stream.build_vtm_url` |

Constants: magic `0x24`; message channel `0x00`; stream channel `0x01`;
encrypted channels `0x0a`/`0x0b`; keepalive `0x132`/`0x133`; stream-info
request/response `0x13b`/`0x13c`; MPEG-PS start `00 00 01 BA`.
