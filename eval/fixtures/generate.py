#!/usr/bin/env python3
"""Generate the committed, sanitised synthetic fixture set.

Runs real dev tools in a scratch dir and records their output as an agent
would see it (stdout+stderr, no TTY). Covers the tools our own sessions
under-represent: cargo build/test, pytest, node --test, npm, gcc/make, git,
grep, find, ls, ps, docker, gh, cargo metadata.

Sanitisation: $HOME/username/hostname/scratch paths are rewritten; git authors,
emails and GitHub logins are pseudonymised (stable mapping); docker repos are
renamed; `ps` keeps only non-login-user processes. A final leak check fails the
run if any forbidden token survives.

Network is used only for: a depth-limited public clone (ripgrep), `uv pip
install pytest` into a throwaway venv, `npm install express`, and `gh` on a
public repo. Each step is skipped (and recorded) if unavailable.

Usage: generate.py [--keep-scratch]
"""
from __future__ import annotations

import argparse
import getpass
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import textwrap
from pathlib import Path

HERE = Path(__file__).resolve().parent
DATA = HERE / "data"
MANIFEST = HERE / "manifest.json"
HOME = str(Path.home())
USER = getpass.getuser()
HOST = socket.gethostname()
PUBLIC_REPO = "https://github.com/BurntSushi/ripgrep"
MAX_FIXTURE = 96 * 1024
PUBLIC_IMAGES = r"(mongo|postgres|redis|nginx|alpine|ubuntu|debian|node|python|rust|busybox|hello-world)(:[\w.-]+)?"

FORBIDDEN = [HOME, f"/home/{USER}", HOST, "HaseebKhalid", "Haseeb", "haseeb", "Jawz", "jawz"]
FORBIDDEN_RE = [re.compile(r"\b" + re.escape(USER) + r"\b"), re.compile(r"sk-ant-"), re.compile(r"ghp_[A-Za-z0-9]{20}"),
                re.compile(r"gsk_[A-Za-z0-9]{20}"), re.compile(r"sk-or-v1-"), re.compile(r"nvapi-")]
# Extra private words (employer/project names, account ids) live in a local,
# uncommitted denylist: one case-insensitive word per line.
_DENY = Path(os.environ.get("CRUSH_CORPUS_DIR", Path.home() / ".cache" / "crush-corpus")) / "fixture-denylist.txt"
if _DENY.exists():
    for _w in _DENY.read_text().split():
        FORBIDDEN_RE.append(re.compile(r"\b" + re.escape(_w) + r"\b", re.I))


class Gen:
    def __init__(self, scratch: Path):
        self.scratch = scratch
        self.fixtures: list[dict] = []
        self.skipped: list[dict] = []
        self.people: dict[str, str] = {}

    # ── sanitising ───────────────────────────────────────────────────────────
    def person(self, name: str) -> str:
        if name not in self.people:
            self.people[name] = f"Dev{len(self.people) + 1:02d}"
        return self.people[name]

    def sanitize(self, s: str) -> str:
        s = s.replace(str(self.scratch), "/work")
        s = s.replace(HOME, "/home/dev")
        s = re.sub(r"\b" + re.escape(USER) + r"\b", "dev", s)
        s = s.replace(HOST, "devbox")
        s = s.replace("HaseebKhalid1507", "example-user").replace("Haseeb Khalid", "Dev User")
        s = re.sub(r"\bJawz\b", "crush-dev", s)
        return s

    def leak_check(self, name: str, s: str) -> None:
        for f in FORBIDDEN:
            if f and f in s:
                raise SystemExit(f"LEAK in fixture {name}: {f!r}")
        for rx in FORBIDDEN_RE:
            if rx.search(s):
                raise SystemExit(f"LEAK in fixture {name}: /{rx.pattern}/")

    # ── running ─────────────────────────────────────────────────────────────
    def sh(self, cmd: str, cwd: Path, env=None, timeout=600) -> tuple[int, str]:
        e = dict(os.environ)
        e.update({"NO_COLOR": "1", "CARGO_TERM_COLOR": "never", "TERM": "dumb", "LC_ALL": "C.UTF-8",
                  "CARGO_TARGET_DIR": str(self.scratch / "target"), "CARGO_INCREMENTAL": "0",
                  "CARGO_PROFILE_DEV_DEBUG": "0", "CARGO_PROFILE_TEST_DEBUG": "0", "GIT_PAGER": "cat",
                  "PAGER": "cat", "GH_PAGER": "cat", "GH_NO_UPDATE_NOTIFIER": "1", "NPM_CONFIG_FUND": "false",
                  "NPM_CONFIG_UPDATE_NOTIFIER": "false"})
        if env:
            e.update(env)
        p = subprocess.run(["bash", "-c", cmd], cwd=cwd, env=e, capture_output=True, text=True,
                           timeout=timeout, errors="replace")
        return p.returncode, p.stdout + p.stderr

    def add(self, name: str, cls: str, command: str | None, output: str, tool: str = "bash",
            tool_input: dict | None = None, note: str = "") -> None:
        output = self.sanitize(output)
        if len(output.encode()) > MAX_FIXTURE and cls == "STRUCTURED":
            return self.skip(name, f"JSON over {MAX_FIXTURE} B (not cut: would be invalid)")
        if len(output.encode()) > MAX_FIXTURE:
            cut = output.encode()[:MAX_FIXTURE].decode("utf-8", "ignore")
            output = cut[: cut.rfind("\n") + 1]
            note = (note + "; " if note else "") + f"cut at {MAX_FIXTURE // 1024} KiB on a line boundary"
        if command is not None:
            command = self.sanitize(command)
        self.leak_check(name, output)
        if len(output.strip()) == 0:
            self.skipped.append({"name": name, "why": "empty output"})
            return
        ext = ".json" if cls == "STRUCTURED" and output.lstrip()[:1] in "[{" else ".txt"
        fn = f"{name}{ext}"
        (DATA / fn).write_text(output)
        ti = tool_input if tool_input is not None else {"command": command}
        self.fixtures.append({"name": name, "file": f"data/{fn}", "tool": tool, "input": ti,
                              "class": cls, "bytes": len(output.encode()), "note": note})
        print(f"  + {fn:34} {cls:10} {len(output.encode()):>7} B")

    def skip(self, name: str, why: str) -> None:
        self.skipped.append({"name": name, "why": why})
        print(f"  - skip {name}: {why}")


# ── fixture sources ──────────────────────────────────────────────────────────

def rust_crate(root: Path, n_tests: int, failing: set[int], warnings: int, errors: bool) -> None:
    (root / "src").mkdir(parents=True, exist_ok=True)
    (root / "Cargo.toml").write_text(textwrap.dedent("""\
        [package]
        name = "inventory"
        version = "0.3.1"
        edition = "2021"

        [dependencies]
        """))
    mods = []
    for m in ("pricing", "stock", "orders", "shipping"):
        lines = [f"//! {m} module", "#[allow(dead_code)]", "pub struct Item { pub sku: u32, pub qty: i64, pub price_cents: i64 }", ""]
        for i in range(n_tests // 4):
            k = len(mods) * 1000 + i
            lines.append(f"pub fn {m}_rule_{i}(x: i64) -> i64 {{ x * {i % 7 + 1} + {k % 13} }}")
        lines.append("")
        lines.append("#[cfg(test)]\nmod tests {\n    use super::*;")
        for i in range(n_tests // 4):
            idx = len(mods) * (n_tests // 4) + i
            want = 3 * (i % 7 + 1) + (len(mods) * 1000 + i) % 13
            if idx in failing:
                want += 1
            lines.append(f"    #[test]\n    fn {m}_rule_{i}_holds() {{\n        let got = {m}_rule_{i}(3);\n"
                         f"        assert_eq!(got, {want}, \"rule {i} for {m}\");\n    }}")
        lines.append("}")
        if warnings:
            for w in range(warnings // 4):
                lines.append(f"pub fn {m}_legacy_{w}(input: i64) -> i64 {{\n    let unused_{w} = input * 2;\n"
                             f"    let mut counter = 0;\n    return 0;\n}}")
        if errors and m == "orders":
            lines.append("pub fn total(items: &[Item]) -> i64 {\n    let s: String = items.len();\n"
                         "    items.iter().map(|i| i.qty * i.price).sum()\n}")
        (root / "src" / f"{m}.rs").write_text("\n".join(lines) + "\n")
        mods.append(m)
    (root / "src" / "lib.rs").write_text("".join(f"pub mod {m};\n" for m in mods))


def gen_cargo(g: Gen) -> None:
    if not shutil.which("cargo"):
        return g.skip("cargo", "cargo not on PATH")
    s = g.scratch
    rust_crate(s / "inventory", 160, set(), 0, False)
    g.sh("cargo build", s / "inventory")  # warm deps (none) so test output is test-only
    rc, out = g.sh("cargo test 2>&1", s / "inventory")
    g.add("cargo_test_pass", "LOG", "cargo test", out, note=f"exit {rc}; 160 tests, all pass")

    shutil.rmtree(s / "inventory")
    rust_crate(s / "inventory", 160, {7, 61, 62, 133}, 0, False)
    rc, out = g.sh("cargo test 2>&1", s / "inventory")
    g.add("cargo_test_fail", "LOG", "cargo test", out, note=f"exit {rc}; 160 tests, 4 fail")

    shutil.rmtree(s / "inventory")
    rust_crate(s / "inventory", 40, set(), 48, False)
    rc, out = g.sh("cargo build 2>&1", s / "inventory")
    g.add("cargo_build_warnings", "LOG", "cargo build", out, note=f"exit {rc}; ~100 warnings, same 3 kinds at many locations")

    shutil.rmtree(s / "inventory")
    rust_crate(s / "inventory", 40, set(), 8, True)
    rc, out = g.sh("cargo build 2>&1", s / "inventory")
    g.add("cargo_build_error", "LOG", "cargo build", out, note=f"exit {rc}; type errors + warnings")

    # cargo metadata of this repo (real dep graph, offline).
    repo = HERE.parents[1]
    rc, out = g.sh("cargo metadata --format-version 1 --offline 2>/dev/null", repo)
    if rc == 0 and out.strip():
        out = out.replace(str(repo), "/work/crush")
        g.add("cargo_metadata", "STRUCTURED", "cargo metadata --format-version 1", out, note="minified JSON")
        pretty = json.dumps(json.loads(g.sanitize(out))["packages"][:12], indent=2) + "\n"
        g.add("cargo_metadata_packages_pretty", "STRUCTURED",
              "cargo metadata --format-version 1 | jq '.packages[:12]'", pretty, note="pretty-printed JSON array of objects")
    else:
        g.skip("cargo_metadata", f"exit {rc}")


def gen_pytest(g: Gen) -> None:
    s = g.scratch / "py"
    s.mkdir()
    if not shutil.which("uv"):
        return g.skip("pytest", "uv not available")
    rc, out = g.sh("uv venv -q .venv && uv pip install -q --python .venv/bin/python pytest", s, timeout=300)
    if rc != 0:
        return g.skip("pytest", "could not install pytest (network?)")
    (s / "tests").mkdir()
    for f in ("parser", "billing", "auth"):
        body = ["import pytest", ""]
        for i in range(30):
            body.append(f"def test_{f}_case_{i}():\n    assert {i} * 2 == {i * 2}\n")
        body.append(f"@pytest.mark.parametrize('n', range(12))\ndef test_{f}_param(n):\n    assert n >= 0\n")
        (s / "tests" / f"test_{f}.py").write_text("\n".join(body))
    rc, out = g.sh(".venv/bin/python -m pytest -v tests 2>&1", s)
    g.add("pytest_verbose_pass", "LOG", "pytest -v tests", out, note=f"exit {rc}; 126 tests pass")
    fail = (s / "tests" / "test_billing.py").read_text()
    fail = fail.replace("assert 7 * 2 == 14", "assert round(7 * 2.15, 2) == 15.06, 'invoice total drifted'")
    fail = fail.replace("assert 19 * 2 == 38", "data = {'a': 1}\n    assert data['b'] == 1")
    (s / "tests" / "test_billing.py").write_text(fail)
    rc, out = g.sh(".venv/bin/python -m pytest tests 2>&1", s)
    g.add("pytest_fail", "LOG", "pytest tests", out, note=f"exit {rc}; 2 failures")
    rc, out = g.sh(".venv/bin/python -m pytest -v tests 2>&1", s)
    g.add("pytest_verbose_fail", "LOG", "pytest -v tests", out, note=f"exit {rc}; 2 failures, verbose")


def gen_node(g: Gen) -> None:
    if not shutil.which("node"):
        return g.skip("node", "node not on PATH")
    s = g.scratch / "js"
    s.mkdir()
    tests = ["const test = require('node:test');", "const assert = require('node:assert');", ""]
    for suite in ("cart", "checkout", "catalog"):
        tests.append(f"test.describe('{suite}', () => {{")
        for i in range(20):
            ok = not (suite == "checkout" and i in (4, 15))
            tests.append(f"  test('{suite} handles case {i}', () => {{ assert.strictEqual({i} + 1, {i + 1 if ok else i + 2}); }});")
        tests.append("});")
    (s / "app.test.js").write_text("\n".join(tests) + "\n")
    rc, out = g.sh("node --test --test-reporter=spec 2>&1", s)
    g.add("node_test_fail", "LOG", "node --test", out, note=f"exit {rc}; 60 tests, 2 fail")
    if shutil.which("npm"):
        rc, out = g.sh("npm init -y >/dev/null && npm install express@4 --no-audit 2>&1", s, timeout=300)
        if rc == 0:
            g.add("npm_install", "LOG", "npm install express@4", out, note="")
            rc, out = g.sh("npm ls --all 2>&1", s)
            g.add("npm_ls_all", "LISTING", "npm ls --all", out, note=f"exit {rc}")
        else:
            g.skip("npm", "npm install failed (network?)")


def gen_gcc(g: Gen) -> None:
    if not (shutil.which("gcc") and shutil.which("make")):
        return g.skip("gcc", "gcc/make missing")
    s = g.scratch / "c"
    s.mkdir()
    srcs = []
    for i in range(14):
        name = f"mod{i:02d}.c"
        body = ["#include <stdio.h>", "#include <string.h>", ""]
        for j in range(6):
            body.append(f"int handler_{i}_{j}(int argc, char **argv) {{\n    int unused_{j} = {j};\n"
                        f"    unsigned int n = argc;\n    if (n < -1) return 0;\n    char buf[8];\n"
                        f"    sprintf(buf, \"%d\", argc * {j + 1000});\n    return strlen(buf);\n}}")
        (s / name).write_text("\n".join(body) + "\n")
        srcs.append(name)
    (s / "Makefile").write_text("CFLAGS=-Wall -Wextra -O2\nOBJS=" + " ".join(x.replace(".c", ".o") for x in srcs)
                                + "\nall: $(OBJS)\n\tar rcs libmods.a $(OBJS)\n%.o: %.c\n\t$(CC) $(CFLAGS) -c $< -o $@\n")
    rc, out = g.sh("make -j1 2>&1", s)
    g.add("make_gcc_warnings", "LOG", "make 2>&1", out, note=f"exit {rc}; long build-warning log, same warnings at many locations")


def gen_git(g: Gen) -> None:
    if not shutil.which("git"):
        return g.skip("git", "git missing")
    s = g.scratch
    rc, out = g.sh(f"git clone -q --depth 400 {PUBLIC_REPO} rg 2>&1", s, timeout=300)
    if rc != 0:
        return g.skip("git", "public clone failed (network?)")
    rg = s / "rg"
    # Pseudonymise authors via a mailmap-style rewrite of the log output.
    rc, raw = g.sh("git log --format='%an%x09%ae' -n 400", rg)
    for line in raw.splitlines():
        if "\t" in line:
            n, e = line.split("\t", 1)
            g.person(n)
            g.person(e)

    def pseudo(text: str) -> str:
        for real in sorted(g.people, key=len, reverse=True):
            alias = g.people[real]
            if "@" in real:
                text = text.replace(real, f"{alias.lower()}@example.com")
            else:
                text = text.replace(real, alias)
        text = re.sub(r"[\w.+-]+@users\.noreply\.github\.com", "dev@example.com", text)
        text = re.sub(r"(?<![\w.])@[A-Za-z0-9-]{2,39}\b", "@someone", text)
        return text

    def add_git(name, cls, cmd, note=""):
        rc, out = g.sh(cmd + " 2>&1", rg)
        g.add(name, cls, cmd, pseudo(out), note=note or f"exit {rc}; public repo, authors pseudonymised")

    add_git("git_log_default", "VCS", "git log -n 40")
    add_git("git_log_oneline", "VCS", "git log --oneline -n 300")
    add_git("git_log_pipe", "VCS", "git log --pretty=format:'%H|%an|%ae|%ad|%s' -n 400")
    add_git("git_log_stat", "VCS", "git log --stat -n 12")
    add_git("git_show", "CONTENT", "git show HEAD~2")
    add_git("git_diff", "CONTENT", "git diff HEAD~6 HEAD -- crates/")
    add_git("grep_rn", "SEARCH", "grep -rn 'pub fn' crates/")
    add_git("grep_rn_pattern", "SEARCH", "grep -rn 'unwrap()' crates/ --include=*.rs")
    add_git("grep_n_single", "SEARCH", "grep -n 'fn ' crates/searcher/src/searcher/mod.rs")
    add_git("find_rs", "LISTING", "find . -name '*.rs' -not -path './target/*'")
    add_git("find_all", "LISTING", "find crates -maxdepth 4")
    add_git("cat_source", "CONTENT", "cat crates/searcher/src/searcher/mod.rs")
    add_git("sed_range", "CONTENT", "sed -n '1,220p' crates/core/flags/defs.rs")
    add_git("cat_markdown", "CONTENT", "cat GUIDE.md")
    add_git("head_changelog", "CONTENT", "head -n 400 CHANGELOG.md")
    # The native `read` tool's gutter format: "<n>\t<line>".
    p = rg / "crates/printer/src/standard.rs"
    if p.exists():
        lines = p.read_text().split("\n")[:700]
        out = "".join(f"{i + 1}\t{l}\n" for i, l in enumerate(lines))
        g.add("read_tool_rust", "CONTENT", None, out, tool="read",
              tool_input={"path": "/work/rg/crates/printer/src/standard.rs", "limit": 700})
    # git status in a dirty tree (long form)
    g.sh("git checkout -q -b feature/flags && for f in crates/core/flags/*.rs; do echo '// todo' >> $f; done;"
         " mkdir -p scratch && touch scratch/a.txt scratch/b.txt notes.md && git add crates/core/flags/defs.rs", rg)
    add_git("git_status_long", "VCS", "git status")
    add_git("git_status_short", "VCS", "git status --short")
    add_git("ls_la_repo", "LISTING", "ls -la")
    add_git("ls_la_crate", "LISTING", "ls -la crates/core/flags")
    add_git("ls_lR", "LISTING", "ls -lR crates/printer")
    add_git("tree_like_du", "LISTING", "du -a crates | sort -k2")


def gen_system(g: Gen) -> None:
    s = g.scratch
    rc, out = g.sh("ls -la /usr/lib", s)
    g.add("ls_la_usr_lib", "LISTING", "ls -la /usr/lib", out, note="system dir")
    rc, out = g.sh("ls -lah /usr/bin", s)
    g.add("ls_lah_usr_bin", "LISTING", "ls -lah /usr/bin", out, note="system dir")
    # ps aux restricted to system (non-login-user) processes.
    rc, out = g.sh("ps aux", s)
    lines = out.splitlines()
    keep = [lines[0]] + [l for l in lines[1:] if not l.startswith(USER) and USER not in l and HOME not in l]
    g.add("ps_aux", "LISTING", "ps aux", "\n".join(keep) + "\n", note="login-user processes removed")
    if shutil.which("docker"):
        rc, out = g.sh("docker images --format '{{.Repository}}\t{{.Tag}}\t{{.ID}}\t{{.CreatedSince}}\t{{.Size}}' 2>&1", s)
        if rc == 0 and out.strip():
            names: dict[str, dict[str, str]] = {}

            def nm(x, pre):
                if x in ("<none>",) or re.fullmatch(PUBLIC_IMAGES, x):
                    return x
                tbl = names.setdefault(pre, {})
                tbl.setdefault(x, f"{pre}{len(tbl) + 1:02d}")
                return tbl[x]
            rows = [["REPOSITORY", "TAG", "IMAGE ID", "CREATED", "SIZE"]]
            for line in out.strip().splitlines():
                r = line.split("\t")
                if len(r) == 5:
                    rows.append([nm(r[0], "registry.example.com/team/svc"), r[1] if r[1] == "latest" else nm(r[1], "v1."), r[2], r[3], r[4]])
            w = [max(len(r[i]) for r in rows) for i in range(5)]
            txt = "".join("   ".join(c.ljust(w[i]) for i, c in enumerate(r)).rstrip() + "\n" for r in rows)
            g.add("docker_images", "LISTING", "docker images", txt, note="repos/tags renamed; re-rendered in docker's column layout")
        else:
            g.skip("docker_images", "daemon unreachable or no images")
        rc, out = g.sh("docker ps -a --format '{{.ID}}\t{{.Image}}\t{{.Command}}\t{{.RunningFor}}\t{{.Status}}\t{{.Ports}}\t{{.Names}}' 2>&1", s)
        if rc == 0 and len(out.strip().splitlines()) >= 2:
            names = {}
            rows = [["CONTAINER ID", "IMAGE", "COMMAND", "CREATED", "STATUS", "PORTS", "NAMES"]]
            for line in out.strip().splitlines():
                r = line.split("\t")
                if len(r) == 7:
                    img = r[1] if re.fullmatch(PUBLIC_IMAGES, r[1]) else names.setdefault(r[1], f"registry.example.com/app{len(names) + 1:02d}:latest")
                    rows.append([r[0], img, r[2], r[3], r[4], r[5], f"container-{len(rows):02d}"])
            w = [max(len(r[i]) for r in rows) for i in range(7)]
            txt = "".join("   ".join(c.ljust(w[i]) for i, c in enumerate(r)).rstrip() + "\n" for r in rows)
            g.add("docker_ps", "LISTING", "docker ps -a", txt, note="images/names renamed; re-rendered in docker's column layout")
        else:
            g.skip("docker_ps", "no containers")


def gen_gh(g: Gen) -> None:
    if not shutil.which("gh"):
        return g.skip("gh", "gh missing")
    s = g.scratch
    rc, out = g.sh("gh issue list -R cli/cli --state all --limit 80 --json number,title,state,author,labels,createdAt,updatedAt 2>&1", s, timeout=120)
    if rc != 0:
        return g.skip("gh", "gh failed (network/auth)")
    logins = {}

    def pl(m):
        logins.setdefault(m.group(1), f"user{len(logins) + 1:03d}")
        return f'"login":"{logins[m.group(1)]}"'
    out = re.sub(r'"login":"([^"]+)"', pl, out)
    out = re.sub(r'("login":"user\d+","name":)"[^"]*"', r'\1"Some Name"', out)
    out = re.sub(r'"id":"[A-Za-z0-9_=-]{8,}"', '"id":"ID"', out)
    g.add("gh_issue_list_json", "STRUCTURED", "gh issue list -R cli/cli --state all --limit 80 --json number,title,state,author,labels,createdAt,updatedAt", out,
          note="public repo; logins pseudonymised")
    rc, out = g.sh("gh api 'repos/cli/cli/releases?per_page=3' 2>&1", s, timeout=120)
    if rc == 0:
        out = re.sub(r'"login":"([^"]+)"', pl, out)
        out = re.sub(r'https://avatars\.githubusercontent\.com/[^"]+', "https://avatars.example/x", out)
        out = re.sub(r'(github\.com/|api\.github\.com/users/)(?!cli/)[A-Za-z0-9-]+', r"\1someone", out)
        g.add("gh_api_releases", "STRUCTURED", "gh api 'repos/cli/cli/releases?per_page=3'", out, note="nested JSON")
    rc, out = g.sh("gh pr list -R cli/cli --state all --limit 40 2>&1", s, timeout=120)
    if rc == 0:
        out = re.sub(r"(?m)^(\d+\t[^\t]*\t)[A-Za-z0-9-]+([:/])", r"\1someone\2", out)
        g.add("gh_pr_list", "VCS", "gh pr list -R cli/cli --state all --limit 40", out, note="tab-separated")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--keep-scratch", action="store_true")
    args = ap.parse_args()
    DATA.mkdir(parents=True, exist_ok=True)
    for f in DATA.iterdir():
        f.unlink()
    scratch = Path(tempfile.mkdtemp(prefix="crush-fx-"))
    g = Gen(scratch)
    try:
        for step in (gen_cargo, gen_pytest, gen_node, gen_gcc, gen_git, gen_system, gen_gh):
            print(f"[{step.__name__}]")
            try:
                step(g)
            except subprocess.TimeoutExpired:
                g.skip(step.__name__, "timeout")
    finally:
        if not args.keep_scratch:
            shutil.rmtree(scratch, ignore_errors=True)
    # Strip the person map: it maps real public names to aliases; never written.
    MANIFEST.write_text(json.dumps({"fixtures": g.fixtures, "skipped": g.skipped}, indent=2) + "\n")
    print(f"{len(g.fixtures)} fixtures, {sum(f['bytes'] for f in g.fixtures)} bytes; skipped {len(g.skipped)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
