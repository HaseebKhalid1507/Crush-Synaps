"""First-draft output classifier for crush 2 (Phase 0).

classify(tool, tool_input, output) -> (CLASS, reason)

Three signals, in this order:
  1. native tool name (read/grep/ls/find/...),
  2. a light parse of a shell command: the first *substantive* segment of a
     `&&`/`;`/`||` chain (skipping cd/source/export/...), then the first command
     of its pipeline, then subcommand + flags,
  3. output sniffing (JSON, path:line: hits, ls -l rows, diff hunks, test/build
     markers, git oneline...), used when the command says nothing useful.

`classify_detail` also returns the command-only and sniff-only opinions so the
report can show where the two disagree (our only cheap proxy for confusion).
Deliberately simple: Phase 1 owns the real classifier.
"""
from __future__ import annotations

import json
import re

CONTENT, SEARCH, LISTING, LOG, STRUCTURED, VCS, UNKNOWN = (
    "CONTENT", "SEARCH", "LISTING", "LOG", "STRUCTURED", "VCS", "UNKNOWN")

SHELL_TOOLS = {"bash", "shell", "shell_send", "shell_start"}

NATIVE = {
    "read": CONTENT,
    "grep": SEARCH,
    "ls": LISTING,
    "find": LISTING,
}

# Prefix words that don't change what the command prints.
WRAPPERS = {"sudo", "time", "nice", "ionice", "command", "env", "exec", "nohup", "stdbuf",
            "unbuffer", "timeout", "xargs", "watch", "doas", "builtin"}
# Whole segments that print nothing interesting (skipped when picking the
# substantive segment of an && / ; chain).
NOISE_SEGMENTS = {"cd", "pushd", "popd", "source", ".", "export", "set", "unset", "true",
                  ":", "mkdir", "sleep", "clear", "shopt", "trap", "ulimit", "alias", "local",
                  "declare", "umask", "wait", "rm", "touch", "chmod", "cp", "mv", "ln"}

CONTENT_PROGS = {"cat", "head", "tail", "nl", "less", "more", "bat", "batcat", "tac", "view",
                 "diff", "colordiff", "strings", "od", "xxd", "hexdump", "base64"}
SEARCH_PROGS = {"grep", "egrep", "fgrep", "rg", "ag", "ack", "zgrep", "pcregrep", "ugrep", "sg"}
LISTING_PROGS = {"ls", "find", "fd", "tree", "ps", "pgrep", "du", "df", "lsof", "lsblk",
                 "exa", "eza", "locate", "pstree", "top", "htop", "free", "ss", "netstat",
                 "ip", "mount", "lsmod", "systemctl", "pip-list", "stat", "file", "wc", "realpath",
                 "readlink", "which", "whereis", "type"}
LOG_PROGS = {"cargo", "npm", "pnpm", "yarn", "bun", "npx", "bunx", "pip", "pip3", "uv",
             "pytest", "py.test", "tox", "nox", "go", "make", "cmake", "ninja", "meson",
             "gradle", "gradlew", "mvn", "tsc", "jest", "vitest", "mocha", "rustc", "gcc",
             "g++", "clang", "cc", "javac", "dotnet", "journalctl", "dmesg", "rustup",
             "eslint", "prettier", "ruff", "mypy", "black", "clippy-driver", "deno",
             "cargo-nextest", "nextest", "bazel", "swift", "flutter", "pacman", "apt",
             "apt-get", "dnf", "brew", "poetry", "pipenv", "conda", "next", "vite", "webpack",
             "docker-compose"}
STRUCTURED_PROGS = {"jq", "yq", "gron", "jo", "sqlite3", "psql", "mysql", "csvlook"}

GIT_VCS = {"log", "status", "branch", "reflog", "shortlog", "stash", "remote", "tag",
           "fetch", "pull", "push", "commit", "merge", "rebase", "cherry-pick", "checkout",
           "switch", "worktree", "describe", "rev-parse", "rev-list", "blame", "config",
           "add", "reset", "restore", "clone", "am", "apply", "submodule", "bisect", "notes",
           "for-each-ref", "show-ref", "ls-remote", "range-diff", "cherry", "gc", "fsck",
           "count-objects", "init", "mv", "rm", "clean", "revert", "whatchanged"}
GIT_CONTENT = {"show", "diff", "cat-file", "format-patch"}
GIT_LISTING = {"ls-files", "ls-tree"}
GIT_SEARCH = {"grep"}

SEP_RE = re.compile(r"\s*(?:&&|\|\||;|\n)\s*")
ENV_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*=")


def _tokens(segment: str) -> list[str]:
    # A light split: no shlex (it chokes on heredocs / unbalanced quotes).
    return [t.strip("'\"") for t in segment.split()]


def _strip_prefix(toks: list[str]) -> list[str]:
    i = 0
    while i < len(toks):
        t = toks[i]
        if ENV_RE.match(t):
            i += 1
            continue
        base = t.rsplit("/", 1)[-1]
        if base in WRAPPERS:
            i += 1
            # skip wrapper flags / numeric args (timeout 30, nice -n 5, env -i)
            while i < len(toks) and (toks[i].startswith("-") or re.fullmatch(r"[\d.]+[smh]?", toks[i])
                                     or ENV_RE.match(toks[i])):
                i += 1
            continue
        break
    return toks[i:]


def parse_command(command: str) -> dict:
    """Return {'prog','sub','args','segment','pipeline'} for the first
    substantive command of a shell line."""
    if not command:
        return {"prog": "", "sub": "", "args": [], "segment": "", "pipeline": 0}
    # Cut heredoc bodies: only the first line of a heredoc command matters.
    first_line = command
    if "<<" in command:
        first_line = command.split("\n", 1)[0]
    segments = [s for s in SEP_RE.split(first_line) if s.strip()]
    chosen = None
    for seg in segments:
        head = seg.split("|", 1)[0]
        toks = _strip_prefix(_tokens(head))
        if not toks:
            continue
        prog = toks[0].rsplit("/", 1)[-1]
        if prog in NOISE_SEGMENTS or prog in {"echo", "printf"} and len(segments) > 1:
            continue
        if prog.startswith("#"):
            continue
        chosen = (seg, toks, prog)
        break
    if chosen is None:
        if not segments:
            return {"prog": "", "sub": "", "args": [], "segment": "", "pipeline": 0}
        seg = segments[0]
        toks = _strip_prefix(_tokens(seg.split("|", 1)[0])) or [""]
        chosen = (seg, toks, toks[0].rsplit("/", 1)[-1])
    seg, toks, prog = chosen
    # `git -C dir log` / `cargo +nightly test` / `python3 -m pytest`
    args = toks[1:]
    sub = ""
    i = 0
    while i < len(args):
        a = args[i]
        if prog == "git" and a in ("-C", "-c", "--git-dir", "--work-tree"):
            i += 2
            continue
        if a.startswith("-") or a.startswith("+"):
            i += 1
            continue
        sub = a
        break
    if prog in ("python", "python3") and "-m" in args:
        j = args.index("-m")
        if j + 1 < len(args):
            prog, sub = args[j + 1], ""
    if prog in ("npx", "bunx", "uvx") and sub:
        prog, sub = sub, ""
    return {"prog": prog, "sub": sub, "args": args, "segment": seg,
            "pipeline": seg.count("|") - seg.count("||") * 2}


def class_from_command(cmd: dict) -> tuple[str | None, str]:
    prog, sub, args = cmd["prog"], cmd["sub"], cmd["args"]
    if not prog:
        return None, "no-command"
    if prog == "git":
        if sub in GIT_CONTENT:
            return CONTENT, f"git {sub}"
        if sub in GIT_SEARCH:
            return SEARCH, "git grep"
        if sub in GIT_LISTING:
            return LISTING, f"git {sub}"
        if sub in GIT_VCS:
            return VCS, f"git {sub}"
        return None, f"git {sub or '?'}"
    if prog == "gh":
        if "--json" in args or sub == "api":
            return STRUCTURED, "gh json"
        if sub in ("pr", "run", "issue", "release", "repo", "workflow"):
            return VCS, f"gh {sub}"
        return None, "gh"
    if prog in ("docker", "podman", "kubectl", "k"):
        if any(a in ("ps", "images", "get", "ls", "top", "stats") for a in args[:3]):
            if any(a in ("-o", "--output") for a in args) and any(a in ("json", "yaml") for a in args):
                return STRUCTURED, f"{prog} -o json"
            return LISTING, f"{prog} {sub}"
        if sub in ("logs", "build", "compose", "describe", "pull", "run"):
            return LOG, f"{prog} {sub}"
        if sub == "inspect":
            return STRUCTURED, f"{prog} inspect"
        return None, prog
    if prog == "sed":
        # sed -n 'a,bp' file is a file view; sed -i edits (prints nothing).
        if any(a.startswith("-n") for a in args) or any(re.search(r"\d+p$", a) for a in args):
            return CONTENT, "sed -n"
        return None, "sed"
    if prog == "awk" or prog == "gawk":
        if any(re.search(r"NR\s*[<>=]", a) for a in args):
            return CONTENT, "awk NR range"
        return None, "awk"
    if prog in ("tail", "head") and any(a.endswith(".log") for a in args):
        return LOG, f"{prog} *.log"
    if prog in CONTENT_PROGS:
        return CONTENT, prog
    if prog in SEARCH_PROGS:
        return SEARCH, prog
    if prog in LISTING_PROGS:
        if prog == "systemctl" and sub == "status":
            return LOG, "systemctl status"
        return LISTING, prog
    if prog in STRUCTURED_PROGS:
        return STRUCTURED, prog
    if prog == "cargo" and sub in ("metadata", "tree", "search", "pkgid", "locate-project", "read-manifest"):
        return (STRUCTURED, "cargo metadata") if sub in ("metadata", "read-manifest", "locate-project") else (LISTING, f"cargo {sub}")
    if prog in LOG_PROGS:
        return LOG, f"{prog} {sub}".strip()
    if prog in ("curl", "wget", "http", "xh"):
        return None, "curl"  # decided by sniffing (JSON vs HTML vs text)
    return None, prog


# ── output sniffing ──────────────────────────────────────────────────────────

GREP_HIT = re.compile(r"^[^\s:][^:\n]{0,300}:\d+[:-]")
NUMBERED = re.compile(r"^\s*\d+(\t|[:→|]\s?| {2})")
LS_ROW = re.compile(r"^[-dlcbps][rwxsStT-]{9}[.+@]?\s+\d+\s")
PATHLIKE = re.compile(r"^\.{0,2}/?[\w@.+-]+(/[\w@.+-]*)+/?$|^[\w@.+-]+\.[A-Za-z0-9]{1,8}$")
ONELINE = re.compile(r"^(\* )?[0-9a-f]{7,40} ")
LOG_MARKERS = re.compile(
    r"^\s*(Compiling|Checking|Finished|Running|Downloaded|Downloading|Fresh|Updating|Installing|Building)\b"
    r"|^test .+ \.\.\. (ok|FAILED|ignored)|^test result:|^(error|warning)(\[\w+\])?:|^=+ .*(passed|failed|error).* =+$"
    r"|PASSED|FAILED|^\s*✓|^\s*✗|npm (WARN|ERR)|^(ok|FAIL|PASS)\s|^---\s(PASS|FAIL)|^\[\d+/\d+\]|^Traceback \(most recent call last\)"
    r"|^\s+at .+:\d+:\d+\)?$")
DIFF_MARK = re.compile(r"^(diff --git |@@ -\d+|\+\+\+ |--- a/|index [0-9a-f]+\.\.)")
GIT_STATUS = re.compile(r"^(On branch |Changes (not )?(staged|to be committed)|Untracked files:|nothing to commit|Your branch is| ?[MADRCU?]{1,2} \S)")


def sniff(output: str) -> tuple[str | None, str]:
    s = output.strip()
    if not s:
        return None, "empty"
    if s[0] in "[{":
        try:
            json.loads(s)
            return STRUCTURED, "json"
        except Exception:
            pass
    lines = [l for l in s.split("\n") if l.strip()][:400]
    n = len(lines)
    if n == 0:
        return None, "empty"

    def frac(rx):
        return sum(1 for l in lines if rx.search(l)) / n

    if n >= 3 and all(l.lstrip().startswith("{") for l in lines[:20]):
        try:
            for l in lines[:20]:
                json.loads(l)
            return STRUCTURED, "jsonl"
        except Exception:
            pass
    if frac(DIFF_MARK) > 0.02 and any(l.startswith("@@") for l in lines):
        return CONTENT, "diff"
    if frac(LS_ROW) > 0.5:
        return LISTING, "ls -l rows"
    if frac(GREP_HIT) > 0.5:
        return SEARCH, "path:line: hits"
    if frac(NUMBERED) > 0.6:
        return CONTENT, "numbered lines"
    if frac(ONELINE) > 0.6:
        return VCS, "git oneline"
    if frac(GIT_STATUS) > 0.5 or s.startswith("On branch "):
        return VCS, "git status"
    if frac(LOG_MARKERS) > 0.15:
        return LOG, "build/test markers"
    if n >= 3 and frac(PATHLIKE) > 0.7:
        return LISTING, "path list"
    if n >= 3:
        commas = [l.count(",") for l in lines[:50]]
        if min(commas) >= 2 and len(set(commas)) == 1:
            return STRUCTURED, "csv"
        if sum(1 for l in lines if re.match(r"^\s*[\w.-]+:\s", l)) / n > 0.7 and not lines[0].startswith("#"):
            return STRUCTURED, "yaml-ish"
    return None, "no-sniff"


def classify_detail(tool: str, tool_input, output: str) -> dict:
    tool = tool or ""
    command = ""
    if isinstance(tool_input, dict):
        command = tool_input.get("command") or tool_input.get("input") or ""
        if not isinstance(command, str):
            command = ""
    snf, snf_reason = sniff(output)
    cmd_cls, cmd_reason, parsed = None, "", None
    if tool in NATIVE:
        cmd_cls, cmd_reason = NATIVE[tool], f"tool:{tool}"
    elif tool in SHELL_TOOLS:
        parsed = parse_command(command)
        cmd_cls, cmd_reason = class_from_command(parsed)
    else:
        cmd_reason = f"tool:{tool}"

    # Decision: the command wins when it is specific; sniffing fills the gaps.
    # Two narrow overrides where the output contradicts the command shape:
    #   - CONTENT-by-command but the output is JSON (cat x.json) -> still CONTENT
    #     (it's a file view the model may quote for an edit)
    #   - curl/unknown -> sniff.
    if cmd_cls is not None:
        cls, reason = cmd_cls, cmd_reason
        # A grep piped into `wc -l` / `head` is still search; but a cat piped
        # into grep is search: look at the pipeline's grep stage.
        if cls == CONTENT and parsed and parsed["pipeline"] > 0 and re.search(r"\|\s*(grep|rg)\b", parsed["segment"]):
            cls, reason = SEARCH, f"{cmd_reason}|grep"
    elif snf is not None:
        cls, reason = snf, f"sniff:{snf_reason}"
    else:
        cls, reason = UNKNOWN, f"{cmd_reason or 'none'}/{snf_reason}"
    return {"class": cls, "reason": reason, "cmd_class": cmd_cls, "cmd_reason": cmd_reason,
            "sniff_class": snf, "sniff_reason": snf_reason,
            "prog": parsed["prog"] if parsed else tool, "sub": parsed["sub"] if parsed else ""}


def classify(tool: str, tool_input, output: str) -> tuple[str, str]:
    d = classify_detail(tool, tool_input, output)
    return d["class"], d["reason"]


if __name__ == "__main__":  # tiny self-check
    cases = [
        ("bash", {"command": "cd /x && git log --oneline -5"}, "abc1234 msg\n", VCS),
        ("bash", {"command": "sed -n '1,40p' src/main.rs"}, "fn main() {}\n", CONTENT),
        ("bash", {"command": "grep -rn foo src | head"}, "src/a.rs:1:foo\n", SEARCH),
        ("bash", {"command": "cat a.rs | grep -n fn"}, "1:fn x\n", SEARCH),
        ("bash", {"command": "cargo test 2>&1 | tail -20"}, "test result: ok\n", LOG),
        ("bash", {"command": "ls -la"}, "total 0\n", LISTING),
        ("bash", {"command": "curl -s http://x"}, '{"a":1}', STRUCTURED),
        ("bash", {"command": "python3 - <<'EOF'\nprint(1)\nEOF"}, "hello there\n", UNKNOWN),
        ("read", {"path": "x"}, "1\tfoo\n", CONTENT),
        ("bash", {"command": "FOO=1 timeout 30 git -C repo status"}, "On branch main\n", VCS),
        ("bash", {"command": "git diff HEAD~1"}, "diff --git a/x b/x\n", CONTENT),
        ("subagent_collect", {}, "The agent found...", UNKNOWN),
    ]
    bad = 0
    for tool, inp, out, want in cases:
        got = classify(tool, inp, out)
        if got[0] != want:
            bad += 1
            print("MISMATCH", tool, inp, got, "want", want)
    print("ok" if not bad else f"{bad} mismatches")
