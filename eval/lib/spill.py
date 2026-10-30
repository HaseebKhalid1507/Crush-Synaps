"""SynapsCLI #334 tool-output virtualisation, re-implemented for accounting.

Synaps spills any result over `tool_output_spill_bytes` (12 KiB) to a file and
keeps a head + tail preview in history (`runtime/tool_spill.rs`, `Cut::new` +
`spill_preview`). crush runs first (compress, then spill), so what a crushed
result costs after spill is the preview of the *crushed* text when that is
still over the threshold. `after_spill(text)` returns what history carries.

Port of the Rust, byte for byte on UTF-8:
- head cut at `head` bytes (floor char boundary), moved back to a line
  boundary when that keeps at least half the head;
- tail cut at `len - tail` (ceil char boundary), moved forward to the next
  line start when that keeps at least half the tail;
- `[... lines A-B omitted ...]` between them, then a blank line and the
  `[Output virtualised: ...]` note with the spill path.
"""
from __future__ import annotations

THRESHOLD = 12288
HEAD = 4096
TAIL = 3072
# a realistic spill path (same length class as Synaps'); only its token cost matters
PATH = "/home/user/.synaps-cli/spill/session-1497513-1793340440887-49/toolu_018HPHz1D9QgMpJsQMfa9R3f.txt"


def _floor(b: bytes, i: int) -> int:
    i = min(i, len(b))
    while 0 < i < len(b) and (b[i] & 0xC0) == 0x80:
        i -= 1
    return i


def _ceil(b: bytes, i: int) -> int:
    while i < len(b) and (b[i] & 0xC0) == 0x80:
        i += 1
    return i


def preview(text: str, head: int = HEAD, tail: int = TAIL, path: str = PATH) -> str | None:
    """The preview Synaps would keep, or None when nothing would be omitted."""
    b = text.encode("utf-8", "surrogatepass")
    n = len(b)
    if head + tail >= n:
        return None
    head_end = _floor(b, head)
    nl = b.rfind(b"\n", 0, head_end)
    if nl >= 0 and 2 * (nl + 1) >= head_end:
        head_end = nl + 1
    tail_start = _ceil(b, n - tail)
    if tail_start < n and b[tail_start - 1:tail_start] != b"\n":
        off = b.find(b"\n", tail_start)
        if off >= 0:
            line_start = off + 1
            if 2 * (n - line_start) >= n - tail_start:
                tail_start = line_start
    before = b.count(b"\n", 0, head_end)
    within = b.count(b"\n", head_end, tail_start - 1)
    after = b.count(b"\n", tail_start - 1, n)
    total = before + within + after + (0 if b.endswith(b"\n") else 1)
    first, last = before + 1, before + within + 1
    hd, tl = b[:head_end].decode("utf-8", "surrogatepass"), b[tail_start:].decode("utf-8", "surrogatepass")
    out = hd
    if hd and not hd.endswith("\n"):
        out += "\n"
    out += f"[... lines {first}-{last} omitted ...]\n" + tl
    if not tl.endswith("\n"):
        out += "\n"
    out += (f"\n[Output virtualised: {n} bytes, {total} lines; lines {first}-{last} are omitted above. "
            f"Full output: {path} (read it with offset={first - 1} limit={last - first + 1}, or grep it).]")
    return out


def after_spill(text: str, threshold: int = THRESHOLD) -> str:
    """What history carries for `text` once Synaps' spill step has run."""
    if len(text.encode("utf-8", "surrogatepass")) <= threshold:
        return text
    return preview(text) or text


if __name__ == "__main__":
    # SynapsCLI's own test (cuts_land_on_line_boundaries_and_name_the_omitted_lines)
    def lines(n, w):
        return "".join(f"line {i:05} " + "x" * (w - 12) + "\n" for i in range(1, n + 1))
    text = lines(200, 50)
    want = (lines(20, 50) + "[... lines 21-190 omitted ...]\n" + text[-500:] + "\n"
            "[Output virtualised: 10000 bytes, 200 lines; lines 21-190 are omitted above. "
            "Full output: P (read it with offset=20 limit=170, or grep it).]")
    assert preview(text, 1030, 520, "P") == want
    # one huge multibyte line: cut mid-line on char boundaries
    huge = "\u00e9\U0001f980" * 5000
    assert "[... lines 1-1 omitted ...]" in preview(huge, 1001, 999, "P")
    # a short first line does not waste the head budget
    t = "abc\n" + "y" * 5000 + "\n" + lines(10, 40)
    assert preview(t, 1000, 200, "P").startswith("abc\n" + "y" * 996 + "\n[... lines 2-")
    assert after_spill("short\n") == "short\n"
    print("spill.py self-check ok")
