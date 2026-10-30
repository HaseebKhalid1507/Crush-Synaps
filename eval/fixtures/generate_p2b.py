#!/usr/bin/env python3
"""Phase 2B fixtures: LOG and STRUCTURED shapes the earlier sets cover thinly.
Appends to manifest.json (replacing entries of the same name) and reuses
generate.py's runner, sanitiser and leak check.

- cargo_workspace_test_fail  real `cargo test --workspace` over 3 crates and a
                             dozen test binaries (~430 tests), failures in a
                             middle binary: over Synaps' 12 KiB spill threshold
- cargo_test_nocapture       real `cargo test -- --nocapture`: test stdout
                             interleaved with the result lines, one failure
- cargo_test_filtered        real `cargo test rule_3`: most binaries run 0 tests
- cargo_build_deps           real first build with crates.io dependencies
                             (Downloaded / Compiling lines) and warnings
- jest_verbose_fail          real `npx jest --verbose` (jest installed into the
                             scratch dir), two failures among ~90 tests
- pip_install                real `pip install` into a throwaway venv
- go_test_verbose            `go test -v ./...` shape (no Go toolchain here):
                             generated from go's documented line formats
- gh_issues_jq               real `gh api … | jq '[…]'`: pretty array of
                             objects with nulls and empty strings
- gh_repo_api                real `gh api repos/…`: one pretty nested object
- gh_issues_jq_stream        real `jq '.[] | {…}'`: a stream of pretty objects

Network: crates.io, the npm registry, PyPI and the GitHub API (public repo).
Each step is skipped (and recorded) when unavailable.

Usage: generate_p2b.py [--only name,...]
"""
from __future__ import annotations

import argparse
import json
import random
import re
import shutil
import sys
import tempfile
import textwrap
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from generate import MANIFEST, Gen  # noqa: E402

NAMES = ["cargo_workspace_test_fail", "cargo_test_nocapture", "cargo_test_filtered", "cargo_build_deps",
         "jest_verbose_fail", "pip_install", "go_test_verbose", "gh_issues_jq", "gh_repo_api", "gh_issues_jq_stream"]


def workspace(root: Path, fail: set[str]) -> None:
    """3 crates; each has a lib with nested test modules and integration tests."""
    root.mkdir(parents=True, exist_ok=True)
    crates = {"ledger": ("accounts", "journal", "reports"), "scheduler": ("queue", "timers", "workers"),
              "wire": ("codec", "frames", "handshake")}
    (root / "Cargo.toml").write_text('[workspace]\nresolver = "2"\nmembers = [' +
                                     ", ".join(f'"{c}"' for c in crates) + "]\n")
    for c, mods in crates.items():
        d = root / c
        (d / "src").mkdir(parents=True, exist_ok=True)
        (d / "tests").mkdir(exist_ok=True)
        (d / "Cargo.toml").write_text(f'[package]\nname = "{c}"\nversion = "0.4.2"\nedition = "2021"\n')
        lib = []
        for m in mods:
            lib.append(f"pub mod {m} {{\n    pub fn score(x: u64) -> u64 {{ x * 3 + 1 }}\n")
            for sub in ("parse", "apply"):
                lib.append(f"    #[cfg(test)]\n    mod {sub}_tests {{\n        use super::*;")
                for i in range(14):
                    name = f"{c}::{m}::{sub}_tests::case_{i:02}"
                    want = 3 * i + 1 + (1 if name in fail else 0)
                    lib.append(f"        #[test]\n        fn case_{i:02}() {{ assert_eq!(score({i}), {want}, \"{m} {sub} case {i}\"); }}")
                lib.append("    }")
            lib.append("}")
        (d / "src" / "lib.rs").write_text("\n".join(lib) + "\n")
        for t in ("roundtrip", "limits", "regressions", "smoke"):
            body = [f"use {c}::*;"]
            for i in range(10):
                tn = f"{t}_{i:02}"
                want = 3 * i + 1 + (1 if f"{c}/{t}::{tn}" in fail else 0)
                body.append(f"#[test]\nfn {tn}() {{ assert_eq!({mods[i % 3]}::score({i}), {want}); }}")
            (d / "tests" / f"{t}.rs").write_text("\n".join(body) + "\n")


def gen_cargo(g: Gen) -> None:
    if not shutil.which("cargo"):
        return g.skip("cargo_p2b", "cargo not on PATH")
    s = g.scratch / "ws"
    workspace(s, {"scheduler::timers::apply_tests::case_06", "scheduler::queue::parse_tests::case_11",
                  "scheduler/limits::limits_04"})
    g.sh("cargo build --workspace --tests -q", s, timeout=900)
    rc, out = g.sh("cargo test --workspace --no-fail-fast 2>&1", s, timeout=900)
    g.add("cargo_workspace_test_fail", "LOG", "cargo test --workspace --no-fail-fast", out,
          note=f"exit {rc}; 3 crates, 12 integration binaries + 3 libs + doc-tests, 3 failures in the middle crate")
    rc, out = g.sh("cargo test case_03 --workspace 2>&1", s, timeout=600)
    g.add("cargo_test_filtered", "LOG", "cargo test case_03 --workspace", out,
          note=f"exit {rc}; a name filter: most binaries run 0 tests")

    n = g.scratch / "noisy"
    (n / "src").mkdir(parents=True)
    (n / "Cargo.toml").write_text('[package]\nname = "noisy"\nversion = "0.1.0"\nedition = "2021"\n')
    body = ["pub fn add(a: i32, b: i32) -> i32 { a + b }", "#[cfg(test)]\nmod tests {\n    use super::*;"]
    for i in range(40):
        say = f'        println!("seeding fixture {i}: {{}} rows", {i * 17});\n' if i % 4 == 0 else ""
        want = i + 1 + (1 if i == 23 else 0)
        body.append(f"    #[test]\n    fn adds_{i:02}() {{\n{say}        assert_eq!(add({i}, 1), {want});\n    }}")
    body.append("}")
    (n / "src" / "lib.rs").write_text("\n".join(body) + "\n")
    g.sh("cargo build --tests -q", n, timeout=600)
    rc, out = g.sh("cargo test -- --nocapture 2>&1", n, timeout=600)
    g.add("cargo_test_nocapture", "LOG", "cargo test -- --nocapture", out,
          note=f"exit {rc}; test stdout interleaved with result lines; 1 failure")

    b = g.scratch / "deps"
    (b / "src").mkdir(parents=True)
    (b / "Cargo.toml").write_text(textwrap.dedent("""\
        [package]
        name = "ingest"
        version = "0.2.0"
        edition = "2021"

        [dependencies]
        regex = "1"
        serde = { version = "1", features = ["derive"] }
        itoa = "1"
        smallvec = "1"
        """))
    (b / "src" / "main.rs").write_text(textwrap.dedent("""\
        use serde::Serialize;
        #[derive(Serialize)]
        struct Row { id: u32, name: String }
        fn parse(line: &str) -> Option<Row> {
            let re = regex::Regex::new(r"^(\\d+),(\\w+)$").unwrap();
            let unused_capture_count = re.captures_len();
            let c = re.captures(line)?;
            Some(Row { id: c[1].parse().ok()?, name: c[2].to_string() })
        }
        fn legacy(total: u64) -> u64 { let scratch = total * 2; return 0; }
        fn main() {
            let mut buf = itoa::Buffer::new();
            let v: smallvec::SmallVec<[u8; 4]> = smallvec::smallvec![1, 2];
            println!("{} {:?}", buf.format(7), v);
            let _ = parse("1,a");
        }
        """))
    rc, out = g.sh("cargo build 2>&1", b, timeout=900)
    if "Downloaded" not in out and "Compiling regex" not in out:
        g.skip("cargo_build_deps", "no download/compile lines (offline or cached?)")
    else:
        g.add("cargo_build_deps", "LOG", "cargo build", out, note=f"exit {rc}; first build with crates.io deps + warnings")


def gen_jest(g: Gen) -> None:
    if not shutil.which("npm"):
        return g.skip("jest", "npm not on PATH")
    s = g.scratch / "jest"
    s.mkdir()
    (s / "package.json").write_text('{"name":"shop-ui","version":"1.0.0","private":true,"scripts":{"test":"jest"}}\n')
    rc, _ = g.sh("npm install --no-audit --no-fund --silent jest@29 2>&1", s, timeout=600)
    if rc != 0:
        return g.skip("jest", "npm install jest failed (network?)")
    (s / "src").mkdir()
    rng = random.Random(5)
    suites = {"cart": ["adds an item", "removes an item", "updates quantity", "applies a coupon", "rejects an expired coupon",
                       "computes tax for {r}", "rounds totals for {r}"],
              "checkout": ["validates the address", "requires a payment method", "creates an order", "sends a receipt",
                           "retries a declined card for {r}", "handles a timeout for {r}"],
              "catalog": ["lists products", "filters by category", "sorts by price", "paginates results",
                          "shows stock for {r}", "hides discontinued items for {r}"]}
    regions = ["us-east", "eu-west", "ap-south", "sa-east"]
    fail = {("checkout", "creates an order"), ("catalog", "shows stock for eu-west")}
    for suite, cases in suites.items():
        lines = [f"describe('{suite}', () => {{"]
        for grp in ("happy path", "edge cases"):
            lines.append(f"  describe('{grp}', () => {{")
            for c in cases:
                for r in (regions if "{r}" in c else [None]):
                    name = c.format(r=r) if r else c
                    v = rng.randint(2, 40)
                    want = v * 2 + (1 if (suite, name) in fail and grp == "happy path" else 0)
                    lines.append(f"    test('{name}', () => {{ expect({v} * 2).toBe({want}); }});")
            lines.append("  });")
        lines.append("});")
        (s / "src" / f"{suite}.test.js").write_text("\n".join(lines) + "\n")
    rc, out = g.sh("npx jest --verbose --ci 2>&1", s, timeout=600)
    out = re.sub(r"(?m)(Time:\s+)[\d.]+ ?s", r"\g<1>2.31 s", out)
    out = re.sub(r"\((\d+) ms\)", lambda m: f"({int(m.group(1)) % 9 + 1} ms)", out)
    g.add("jest_verbose_fail", "LOG", "npx jest --verbose", out, note=f"exit {rc}; real jest 29, 2 failures")


def gen_pip(g: Gen) -> None:
    s = g.scratch / "pip"
    s.mkdir()
    rc, out = g.sh("python3 -m venv .venv 2>&1", s, timeout=300)
    if rc != 0:
        return g.skip("pip_install", "venv failed")
    rc, out = g.sh(".venv/bin/pip install --no-cache-dir --disable-pip-version-check requests rich python-dateutil 2>&1",
                   s, timeout=600)
    if rc != 0:
        return g.skip("pip_install", "pip install failed (network?)")
    g.add("pip_install", "LOG", "pip install requests rich python-dateutil", out, note=f"exit {rc}; real pip into a throwaway venv")


def gen_go(g: Gen) -> None:
    """go test -v ./... in go's documented formats (no toolchain on this box)."""
    rng = random.Random(9)
    pkgs = {"example.test/billing/internal/invoice": ["TestTotal", "TestTax", "TestRounding", "TestDiscount", "TestCurrency"],
            "example.test/billing/internal/ledger": ["TestAppend", "TestBalance", "TestReconcile", "TestSnapshot"],
            "example.test/billing/api": ["TestCreateInvoice", "TestGetInvoice", "TestListInvoices", "TestAuth"]}
    fail = {"TestReconcile/partial_refund"}
    out = []
    for pkg, tests in pkgs.items():
        pkg_fail = False
        for t in tests:
            pool = ["empty", "single_item", "many_items", "zero_rate", "negative", "large_values", "unicode_names"]
            subs = [f"{t}/{s}" for s in rng.sample(pool, 4)]
            if t == "TestReconcile":
                subs[1] = f"{t}/partial_refund"
            out.append(f"=== RUN   {t}")
            res = []
            for st in subs:
                out.append(f"=== RUN   {st}")
                if st in fail:
                    out.append(f"    {pkg.rsplit('/', 1)[1]}_test.go:{rng.randint(40, 300)}: balance = 1250, want 1200")
                    res.append(f"    --- FAIL: {st} (0.00s)")
                else:
                    res.append(f"    --- PASS: {st} (0.0{rng.randint(0, 9)}s)")
            bad = any(x.startswith("    --- FAIL") for x in res)
            pkg_fail |= bad
            out.append(f"--- {'FAIL' if bad else 'PASS'}: {t} (0.0{rng.randint(0, 9)}s)")
            out.extend(res)
        if pkg_fail:
            out += ["FAIL", f"FAIL\t{pkg}\t0.{rng.randint(10, 99)}s"]
        else:
            out += ["PASS", f"ok  \t{pkg}\t0.{rng.randint(10, 99)}s"]
    out.append("FAIL" if any(x.startswith("FAIL\t") for x in out) else "ok")
    g.add("go_test_verbose", "LOG", "go test -v ./...", "\n".join(out) + "\n",
          note="go test -v shape generated from go's line formats (no Go toolchain here); 1 failing subtest")


def gen_json(g: Gen) -> None:
    if not shutil.which("gh") or not shutil.which("jq"):
        return g.skip("gh_json_p2b", "gh or jq missing")
    s = g.scratch
    logins: dict[str, str] = {}

    def pseudo(text: str) -> str:
        def pl(m):
            logins.setdefault(m.group(2), f"user{len(logins) + 1:03d}")
            return f'{m.group(1)}"{logins[m.group(2)]}"'
        text = re.sub(r'("(?:login|user|author|assignee)": ?)"([^"]+)"', pl, text)
        text = re.sub(r'https://avatars\.githubusercontent\.com/[^"]+', "https://avatars.example/x", text)
        text = re.sub(r'(github\.com/|api\.github\.com/users/)(?!cli/)[A-Za-z0-9-]+', r"\1someone", text)
        # third-party emails (e.g. a spam issue title): never into the repo
        text = re.sub(r"(?<![\w.@-])(?!git@github\.com)[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)*\.[a-z]{2,}",
                      "someone@example.com", text)
        return text
    q = ('[.[] | {number, title, state, user: .user.login, labels: [.labels[].name], comments, '
         'milestone: .milestone.title, closed_at, draft: (.draft // null), body: ((.body // "") | .[0:60])}]')
    cmd = f"gh api 'repos/cli/cli/issues?state=all&per_page=18' | jq '{q}'"
    rc, out = g.sh(cmd + " 2>&1", s, timeout=120)
    if rc != 0:
        return g.skip("gh_json_p2b", "gh api failed (network/auth)")
    g.add("gh_issues_jq", "STRUCTURED", cmd, pseudo(out), note="real gh api + jq; logins pseudonymised; nulls and empty strings")
    cmd = "gh api 'repos/cli/cli/issues?state=closed&per_page=12' | jq '.[] | {number, state, closed_at, comments, author_association}'"
    rc, out = g.sh(cmd + " 2>&1", s, timeout=120)
    if rc == 0:
        g.add("gh_issues_jq_stream", "STRUCTURED", cmd, pseudo(out), note="real jq stream of pretty objects")
    cmd = "gh api repos/cli/cli | jq ."
    rc, out = g.sh(cmd + " 2>&1", s, timeout=120)
    if rc == 0:
        out = re.sub(r'"(node_id|temp_clone_token)": "[^"]*"', r'"\1": "ID"', out)
        g.add("gh_repo_api", "STRUCTURED", cmd, pseudo(out), note="real gh api; one nested object")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--only", default="")
    args = ap.parse_args()
    steps = [gen_cargo, gen_jest, gen_pip, gen_go, gen_json]
    if args.only:
        steps = [s for s in steps if s.__name__.removeprefix("gen_") in args.only.split(",")]
    scratch = Path(tempfile.mkdtemp(prefix="crush-fx2b-"))
    g = Gen(scratch)
    try:
        for st in steps:
            print(f"[{st.__name__}]", flush=True)
            st(g)
    finally:
        shutil.rmtree(scratch, ignore_errors=True)
    man = json.loads(MANIFEST.read_text())
    names = {f["name"] for f in g.fixtures}
    man["fixtures"] = [f for f in man["fixtures"] if f["name"] not in names] + g.fixtures
    man["skipped"] = [x for x in man.get("skipped", []) if x["name"] not in names] + g.skipped
    MANIFEST.write_text(json.dumps(man, indent=2) + "\n")
    print(f"added {sorted(names)}; skipped {g.skipped}; manifest: {len(man['fixtures'])} fixtures")
    return 0


if __name__ == "__main__":
    sys.exit(main())
