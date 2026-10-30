"""Post-hoc analysis helpers shared by the report."""
from __future__ import annotations

import re

TRAIL_WS = re.compile(r"[ \t\r\f\v]+$", re.M)


def text_steps(raw: str, crushed: str, label: str | None) -> list[str]:
    """Which steps of crush v0.1 changed this output.

    For the text chain (strip_ansi -> rstrip_lines -> collapse_blank_runs ->
    collapse_dup_runs -> elide_blobs -> fold_common_prefix) each step leaves a
    signature we can test exactly against raw/crushed."""
    if label != "text":
        return [label or "none"]
    steps = []
    if "\x1b" in raw:
        steps.append("strip_ansi")
    if TRAIL_WS.search(raw) or "\r\n" in raw:
        steps.append("rstrip_lines")
    r2 = TRAIL_WS.sub("", raw)
    if "\n\n\n" in r2:
        steps.append("collapse_blank_runs")
    if crushed.count(" (×") > raw.count(" (×"):
        steps.append("collapse_dup_runs")
    if "@crush/1.blob" in crushed:
        steps.append("elide_blobs")
    if "@crush/1.prefix=" in crushed:
        steps.append("fold_common_prefix")
    return steps or ["(unattributed)"]


def body(crushed: str) -> str:
    """Strip the one-line provenance header crush prepends."""
    if crushed.startswith("[@crush/1 "):
        return crushed.split("\n", 1)[1] if "\n" in crushed else ""
    return crushed


def lines_altered(raw: str, crushed: str) -> tuple[int, int]:
    """(# non-empty raw lines that no longer appear verbatim as a line of the
    crushed body, # non-empty raw lines). A quoted `old_string` spanning any
    such line would fail to match."""
    have = set(body(crushed).split("\n"))
    rl = [l for l in raw.split("\n") if l.strip()]
    return sum(1 for l in rl if l not in have), len(rl)
