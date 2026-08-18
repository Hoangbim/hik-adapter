#!/usr/bin/env python3
"""Summarize VTM stream-packet dumps from the adapter diagnostic log.

Parses the `stream packet without MPEG-PS pack header` WARN lines, strips the
12-byte RTP header, and reports payload-type and H.265 NAL-type distributions
per camera/channel.
"""
import re
import sys
from collections import defaultdict

ANSI = re.compile(r"\x1b\[[0-9;]*m")
LINE = re.compile(
    r"camera=(?P<camera>\S+) channel=(?P<channel>\d+) "
    r"unsynchronized_packet=(?P<index>\d+) body_len=(?P<len>\d+) "
    r"hex=(?P<hex>[0-9a-f ]+(?:\.\.\.)?)"
)

NAL_NAMES = {
    0: "TRAIL_N", 1: "TRAIL_R", 2: "TSA_N", 3: "TSA_R",
    19: "IDR_W_RADL", 20: "IDR_N_LP", 21: "CRA",
    32: "VPS", 33: "SPS", 34: "PPS", 35: "AUD", 38: "FD", 39: "SEI_PREFIX",
    48: "PACI", 49: "FU",
}


def main(path: str) -> None:
    per_stream = defaultdict(lambda: {
        "packets": 0,
        "payload_types": defaultdict(int),
        "nals": defaultdict(int),
        "first_nal_index": {},
        "pt96_lens": [],
        "marker": defaultdict(int),
    })
    total = 0
    with open(path, encoding="utf-8", errors="replace") as fh:
        for raw in fh:
            line = ANSI.sub("", raw)
            match = LINE.search(line)
            if not match:
                continue
            total += 1
            hex_str = re.sub(r"[^0-9a-f]", "", match.group("hex"))
            body = bytes.fromhex(hex_str)
            stream = per_stream[(match.group("camera"), int(match.group("channel")))]
            stream["packets"] += 1
            if len(body) >= 12 and body[0] >> 6 == 2:
                pt = body[1] & 0x7F
                marker = bool(body[1] & 0x80)
                payload = body[12:]
            else:
                pt = -1
                marker = False
                payload = b""
            stream["payload_types"][pt] += 1
            stream["marker"][marker] += 1
            if pt == 96 and len(payload) >= 2:
                nal_type = ((payload[0] << 8) | payload[1]) >> 9 & 0x3F
                stream["nals"][nal_type] += 1
                stream["first_nal_index"].setdefault(nal_type, match.group("index"))
                stream["pt96_lens"].append(len(payload))

    for (camera, channel), stream in sorted(per_stream.items()):
        print(f"{camera} ch{channel}: {stream['packets']} packets")
        pts = ", ".join(
            f"PT{pt} x{count}" for pt, count in sorted(stream["payload_types"].items())
        )
        print(f"  payload types: {pts}")
        if stream["pt96_lens"]:
            print(
                "  PT96 payload len: "
                f"min={min(stream['pt96_lens'])} max={max(stream['pt96_lens'])} "
                f"avg={sum(stream['pt96_lens']) // len(stream['pt96_lens'])}"
            )
        nals = []
        for nal_type, count in sorted(stream["nals"].items()):
            name = NAL_NAMES.get(nal_type, f"T{nal_type}")
            first = stream["first_nal_index"].get(nal_type, "-")
            nals.append(f"{name}({nal_type}) x{count} @pkt{first}")
        if nals:
            print(f"  PT96 NALs: {'; '.join(nals)}")
        print()

    print(f"total dump lines: {total}")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} <log-file>", file=sys.stderr)
        sys.exit(2)
    main(sys.argv[1])
