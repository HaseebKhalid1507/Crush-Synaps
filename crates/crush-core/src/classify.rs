//! Output classifier: tool name, a light shell parse, output sniffing.
//!
//! The class decides what may happen to a result. CONTENT (file views, diffs)
//! and PROSE (documents, agent/host messages) are never changed; everything
//! else is eligible for its class's transforms. When unsure, the classifier
//! leans to CONTENT, which is the safe direction.

use crate::shell::{self, Segment, Stage};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Class {
    /// File views and diffs the model may quote back as an edit's `old_string`.
    Content,
    /// grep / rg / git grep hits.
    Search,
    /// ls, find, ps, tree, docker/kubectl get, path lists.
    Listing,
    /// Builds, tests, installs, logs.
    Log,
    /// JSON, YAML, CSV.
    Structured,
    /// git log / status / branch, gh views.
    Vcs,
    /// Documents and agent/host messages: forum, memory, worker reports, web pages.
    Prose,
    Unknown,
}

impl Class {
    pub const ALL: [Class; 8] = [
        Class::Content,
        Class::Search,
        Class::Listing,
        Class::Log,
        Class::Structured,
        Class::Vcs,
        Class::Prose,
        Class::Unknown,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Class::Content => "CONTENT",
            Class::Search => "SEARCH",
            Class::Listing => "LISTING",
            Class::Log => "LOG",
            Class::Structured => "STRUCTURED",
            Class::Vcs => "VCS",
            Class::Prose => "PROSE",
            Class::Unknown => "UNKNOWN",
        }
    }

    pub fn parse(s: &str) -> Option<Class> {
        Class::ALL.into_iter().find(|c| c.as_str().eq_ignore_ascii_case(s))
    }

    /// Classes whose bytes are never changed, whatever the config says.
    pub fn byte_identical(self) -> bool {
        matches!(self, Class::Content)
    }
}

impl std::fmt::Display for Class {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A class plus where it came from (for `--explain` and the eval).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    pub class: Class,
    /// `tool:read`, `cmd:git log`, `sniff:json`, ...
    pub reason: String,
}

const SHELL_TOOLS: &[&str] = &["bash", "shell", "sh", "shell_start", "shell_send", "powershell", "exec", "terminal"];

const NOISE: &[&str] = &[
    "cd", "pushd", "popd", "source", ".", "export", "set", "unset", "true", "false", ":", "mkdir", "sleep", "clear",
    "shopt", "trap", "ulimit", "alias", "local", "declare", "umask", "wait", "rm", "touch", "chmod", "chown", "cp",
    "mv", "ln", "kill", "pkill", "killall", "test", "[", "[[", "read", "shift", "return", "exit", "break", "continue",
    "eval", "hash", "rmdir", "install", "truncate", "disown", "tput",
];
const PRINTERS: &[&str] = &["echo", "printf"];
/// Interpreters: their output says nothing reliable about class, and in this
/// workload they are mostly edit scripts that precede the real command.
const INTERPRETERS: &[&str] =
    &["python", "python3", "node", "perl", "ruby", "bash", "sh", "zsh", "deno", "bun", "php", "lua"];

const CONTENT_PROGS: &[&str] = &[
    "cat",
    "head",
    "tail",
    "nl",
    "less",
    "more",
    "bat",
    "batcat",
    "tac",
    "view",
    "diff",
    "colordiff",
    "strings",
    "od",
    "xxd",
    "hexdump",
    "base64",
    "zcat",
    "bzcat",
    "xzcat",
    "zstdcat",
    "delta",
    "difft",
];
const SEARCH_PROGS: &[&str] =
    &["grep", "egrep", "fgrep", "rg", "ag", "ack", "zgrep", "pcregrep", "ugrep", "sg", "git-grep"];
const LISTING_PROGS: &[&str] = &[
    "ls", "find", "fd", "fdfind", "tree", "ps", "pgrep", "du", "df", "lsof", "lsblk", "exa", "eza", "locate", "pstree",
    "top", "htop", "free", "ss", "netstat", "ip", "mount", "lsmod", "pip-list", "stat", "file", "wc", "realpath",
    "readlink", "which", "whereis", "type", "lscpu", "lsusb", "lspci", "id", "groups", "w", "who", "last", "dust",
    "ncdu", "column",
];
const LOG_PROGS: &[&str] = &[
    "cargo",
    "npm",
    "pnpm",
    "yarn",
    "bun",
    "npx",
    "bunx",
    "pip",
    "pip3",
    "uv",
    "pytest",
    "py.test",
    "tox",
    "nox",
    "go",
    "make",
    "cmake",
    "ninja",
    "meson",
    "gradle",
    "gradlew",
    "mvn",
    "tsc",
    "jest",
    "vitest",
    "mocha",
    "rustc",
    "gcc",
    "g++",
    "clang",
    "clang++",
    "cc",
    "javac",
    "dotnet",
    "journalctl",
    "dmesg",
    "rustup",
    "eslint",
    "prettier",
    "ruff",
    "mypy",
    "black",
    "clippy-driver",
    "cargo-nextest",
    "nextest",
    "bazel",
    "swift",
    "flutter",
    "pacman",
    "apt",
    "apt-get",
    "dnf",
    "yum",
    "brew",
    "poetry",
    "pipenv",
    "conda",
    "next",
    "vite",
    "webpack",
    "docker-compose",
    "nvcc",
    "ctest",
    "playwright",
    "cypress",
    "zig",
    "terraform",
    "ansible-playbook",
    "helm",
    "mix",
    "rebar3",
    "stack",
    "cabal",
    "sbt",
    "bundle",
    "rake",
    "rspec",
    "phpunit",
    "composer",
    "shellcheck",
    "hadolint",
    "golangci-lint",
    "build.sh",
    "test.sh",
];
const STRUCTURED_PROGS: &[&str] =
    &["jq", "yq", "gron", "jo", "sqlite3", "psql", "mysql", "csvlook", "mlr", "xsv", "jless"];

const GIT_VCS: &[&str] = &[
    "log",
    "status",
    "branch",
    "reflog",
    "shortlog",
    "stash",
    "remote",
    "tag",
    "fetch",
    "pull",
    "push",
    "commit",
    "merge",
    "rebase",
    "cherry-pick",
    "checkout",
    "switch",
    "worktree",
    "describe",
    "rev-parse",
    "rev-list",
    "blame",
    "config",
    "add",
    "reset",
    "restore",
    "clone",
    "am",
    "apply",
    "submodule",
    "bisect",
    "notes",
    "for-each-ref",
    "show-ref",
    "ls-remote",
    "range-diff",
    "cherry",
    "gc",
    "fsck",
    "count-objects",
    "init",
    "mv",
    "rm",
    "clean",
    "revert",
    "whatchanged",
    "show-branch",
    "merge-base",
];
const GIT_CONTENT: &[&str] = &["show", "diff", "cat-file", "format-patch", "difftool"];
const GIT_LISTING: &[&str] = &["ls-files", "ls-tree"];
const DIFF_SUMMARY_FLAGS: &[&str] = &["stat", "name-only", "name-status", "shortstat", "numstat", "dirstat", "summary"];

/// Synaps / extension tools whose output is a document or a message.
fn prose_tool(tool: &str) -> bool {
    let t = tool.to_ascii_lowercase();
    let t = t.trim_start_matches("ext__").trim_start_matches("mcp__");
    t.starts_with("forum_")
        || t.starts_with("memory_")
        || t.starts_with("subagent")
        || t.starts_with("agent-comms")
        || t.starts_with("subconscious")
        || t.starts_with("context_")
        || matches!(t, "respond" | "search_tools" | "activate_tools" | "load_skill" | "ask_user")
        || ((t.contains("web") || t.contains("browser") || t.contains("docs"))
            && (t.contains("fetch") || t.contains("search") || t.contains("read")))
}

fn native_tool(tool: &str) -> Option<Class> {
    Some(match tool {
        "read" | "read_file" | "view" | "cat" => Class::Content,
        "grep" | "search" | "rg" | "search_files" => Class::Search,
        "ls" | "find" | "glob" | "list_dir" | "list_files" | "tree" => Class::Listing,
        _ => return None,
    })
}

fn log_path(a: &str) -> bool {
    let a = a.trim_matches(|c| c == '"' || c == '\'');
    let base = a.rsplit('/').next().unwrap_or(a);
    base.ends_with(".log")
        || base.ends_with(".log.1")
        || base.ends_with(".out")
        || base.ends_with(".err")
        || base.ends_with(".stderr")
        || base.ends_with(".stdout")
        || base.contains(".log.")
        || a.contains("/logs/")
        || a.contains("/log/")
        || a.starts_with("logs/")
        || a.starts_with("log/")
        || a.contains("*.log")
}

/// What one stage prints, judged by program and flags. `None`: no opinion.
fn stage_class(st: &Stage, seg: &Segment) -> Option<(Class, String)> {
    let prog = st.prog.as_str();
    let sub = st.sub();
    let r = |c: Class, why: String| Some((c, why));
    match prog {
        "git" => {
            let sub = sub.to_string();
            if GIT_CONTENT.contains(&sub.as_str()) {
                if st.args.iter().any(|a| {
                    a.strip_prefix("--")
                        .is_some_and(|f| DIFF_SUMMARY_FLAGS.contains(&f.split('=').next().unwrap_or("")))
                }) || (sub == "show"
                    && st.args.iter().any(|a| {
                        a == "-s"
                            || a == "--no-patch"
                            || a.starts_with("--format")
                            || a.starts_with("--pretty")
                            || a == "--oneline"
                    }))
                {
                    return r(Class::Vcs, format!("git {sub} summary"));
                }
                return r(Class::Content, format!("git {sub}"));
            }
            if sub == "grep" {
                if st.has_flag('l', &["files-with-matches", "name-only"]) || st.has_flag('L', &["files-without-match"])
                {
                    return r(Class::Listing, "git grep -l".into());
                }
                return r(Class::Search, "git grep".into());
            }
            if GIT_LISTING.contains(&sub.as_str()) {
                return r(Class::Listing, format!("git {sub}"));
            }
            if GIT_VCS.contains(&sub.as_str()) {
                return r(Class::Vcs, format!("git {sub}"));
            }
            None
        }
        "gh" => {
            if st.args.iter().any(|a| a == "--json" || a.starts_with("--json=")) || sub == "api" {
                return r(Class::Structured, "gh json".into());
            }
            if st.args.iter().any(|a| a == "diff") {
                return r(Class::Content, "gh diff".into());
            }
            if matches!(sub, "pr" | "run" | "issue" | "release" | "repo" | "workflow" | "search") {
                if st.args.iter().any(|a| a == "list") {
                    return r(Class::Vcs, format!("gh {sub} list"));
                }
                if sub == "run" && st.args.iter().any(|a| a == "view" || a == "watch") {
                    return r(Class::Log, "gh run view".into());
                }
                return r(Class::Vcs, format!("gh {sub}"));
            }
            None
        }
        "docker" | "podman" | "kubectl" | "k" | "nerdctl" => {
            let head: Vec<&str> = st.args.iter().take(3).map(String::as_str).collect();
            let json = st.args.iter().any(|a| {
                a == "json"
                    || a == "yaml"
                    || a == "-ojson"
                    || a == "-oyaml"
                    || a.starts_with("--output=")
                    || a.starts_with("--format=json")
                    || a.contains("{{json")
            });
            if head
                .iter()
                .any(|a| matches!(*a, "ps" | "images" | "get" | "ls" | "top" | "stats" | "volume" | "network"))
            {
                if json {
                    return r(Class::Structured, format!("{prog} -o json"));
                }
                return r(Class::Listing, format!("{prog} {sub}"));
            }
            if matches!(
                sub,
                "logs" | "build" | "compose" | "describe" | "pull" | "run" | "push" | "exec" | "apply" | "rollout"
            ) {
                return r(Class::Log, format!("{prog} {sub}"));
            }
            if sub == "inspect" {
                return r(Class::Structured, format!("{prog} inspect"));
            }
            None
        }
        "sed" | "gsed" => {
            if st.args.iter().any(|a| {
                a == "-i"
                    || a.starts_with("-i.")
                    || a.starts_with("--in-place")
                    || (a.starts_with('-') && !a.starts_with("--") && a.contains('i') && a.len() <= 4)
            }) {
                return None; // in-place edit: prints nothing
            }
            if st.args.iter().any(|a| {
                a.starts_with("-n")
                    || (a.ends_with('p')
                        && a.bytes().next().is_some_and(|c| c.is_ascii_digit() || c == b'$' || c == b'/'))
            }) {
                if st.operands().skip(1).any(log_path) {
                    return r(Class::Log, "sed -n *.log".into());
                }
                return r(Class::Content, "sed -n".into());
            }
            None
        }
        "awk" | "gawk" | "mawk" => {
            if st.args.iter().any(|a| a.contains("NR") && (a.contains('<') || a.contains('>') || a.contains("=="))) {
                return r(Class::Content, "awk NR range".into());
            }
            None
        }
        "systemctl" if sub == "status" => r(Class::Log, "systemctl status".into()),
        "cargo" => match sub {
            "metadata" | "read-manifest" | "locate-project" => r(Class::Structured, "cargo metadata".into()),
            "tree" | "search" | "pkgid" => r(Class::Listing, format!("cargo {sub}")),
            _ => r(Class::Log, format!("cargo {sub}").trim().to_string()),
        },
        "rg" if st.args.iter().any(|a| a == "--files") => r(Class::Listing, "rg --files".into()),
        "curl" | "wget" | "http" | "xh" | "https" => None,
        _ => {
            if CONTENT_PROGS.contains(&prog) {
                let ops: Vec<&str> = st.operands().collect();
                let loop_log = seg.loop_items.iter().any(|x| log_path(x))
                    && ops.iter().any(|a| a.starts_with('$') || a.contains("${"));
                if (matches!(prog, "cat" | "tail" | "head" | "tac" | "less" | "zcat")
                    && (ops.iter().any(|a| log_path(a)) || loop_log))
                    || (prog == "tail" && st.has_flag('f', &["follow"]))
                {
                    return r(Class::Log, format!("{prog} *.log"));
                }
                return r(Class::Content, prog.to_string());
            }
            if SEARCH_PROGS.contains(&prog) {
                if st.has_flag('l', &["files-with-matches"]) || st.has_flag('L', &["files-without-match"]) {
                    return r(Class::Listing, format!("{prog} -l"));
                }
                if st.operands().skip(1).any(log_path)
                    && !st.has_flag('r', &["recursive"])
                    && !st.has_flag('R', &["dereference-recursive"])
                {
                    // grep over log files: the hits are log lines
                    return r(Class::Search, format!("{prog} *.log"));
                }
                return r(Class::Search, prog.to_string());
            }
            if LISTING_PROGS.contains(&prog) {
                return r(Class::Listing, prog.to_string());
            }
            if STRUCTURED_PROGS.contains(&prog) {
                return r(Class::Structured, prog.to_string());
            }
            if LOG_PROGS.contains(&prog) {
                return r(Class::Log, format!("{prog} {sub}").trim().to_string());
            }
            if INTERPRETERS.contains(&prog) {
                // python3 -m pytest / node --test / python3 x_test.py
                let m = st.args.iter().position(|a| a == "-m").and_then(|k| st.args.get(k + 1));
                if let Some(m) = m {
                    if LOG_PROGS.contains(&m.as_str())
                        || matches!(m.as_str(), "unittest" | "pip" | "build" | "venv" | "compileall" | "py_compile")
                    {
                        return r(Class::Log, format!("{prog} -m {m}"));
                    }
                    if m == "json.tool" {
                        return r(Class::Structured, "json.tool".into());
                    }
                }
                if st.args.iter().any(|a| a == "--test")
                    || st.operands().next().is_some_and(|a| {
                        let b = a.rsplit('/').next().unwrap_or(a);
                        b.starts_with("test_")
                            || b.ends_with("_test.py")
                            || b.contains(".test.")
                            || b.contains(".spec.")
                    })
                {
                    return r(Class::Log, format!("{prog} test"));
                }
            }
            None
        }
    }
}

/// Pipeline-aware class of a segment.
fn segment_class(seg: &Segment) -> Option<(Class, String)> {
    let first = seg.first();
    let (mut c, mut why) = stage_class(first, seg)?;
    // later stages that change what the output is
    for st in &seg.stages[1..] {
        let p = st.prog.as_str();
        match c {
            Class::Content | Class::Log | Class::Vcs | Class::Unknown if SEARCH_PROGS.contains(&p) => {
                let invert = st.has_flag('v', &["invert-match"]);
                let count = st.has_flag('c', &["count"]);
                if count {
                    return Some((Class::Unknown, format!("{why}|grep -c")));
                }
                if !invert && c == Class::Content {
                    // `cat f | grep -n x`: search hits (lines still verbatim)
                    c = Class::Search;
                    why = format!("{why}|{p}");
                }
            }
            // `cat f | cut -c1-200`, `| sort`, `| awk …`: still the file's text
            // (cut, filtered or reordered); CONTENT is the safe answer
            Class::Content
                if matches!(
                    p,
                    "awk"
                        | "gawk"
                        | "cut"
                        | "tr"
                        | "sort"
                        | "uniq"
                        | "column"
                        | "paste"
                        | "rev"
                        | "fold"
                        | "fmt"
                        | "sed"
                ) =>
            {
                why = format!("{why}|{p}");
            }
            _ => {}
        }
        if matches!(p, "wc") {
            return Some((Class::Unknown, format!("{why}|wc")));
        }
    }
    Some((c, why))
}

fn substantive(seg: &Segment) -> bool {
    let p = seg.first().prog.as_str();
    let silenced = seg.stages.last().is_some_and(|s| s.silenced);
    let quiet_git = p == "git" && matches!(seg.first().sub(), "add" | "stash" | "checkout" | "switch");
    !(silenced || NOISE.contains(&p) || p.starts_with('#') || quiet_git)
}

/// Class from the command line alone: the first substantive segment decides
/// (edit scripts and opaque interpreter runs yield to a later known command).
/// Also returns the classes of every other recognised segment.
pub fn command_class(command: &str) -> Option<(Class, String)> {
    command_classes(command).0
}

fn command_classes(command: &str) -> (Option<(Class, String)>, Vec<Class>) {
    let segs = shell::parse(command);
    let subst: Vec<&Segment> = segs.iter().filter(|s| substantive(s)).collect();
    let others_exist = subst.len() > 1;
    let mut weak: Option<(Class, String)> = None;
    let mut decided: Option<(Class, String)> = None;
    let mut all = Vec::new();
    let mut stopped = false;
    for seg in &subst {
        let p = seg.first().prog.as_str();
        if PRINTERS.contains(&p) && others_exist {
            continue;
        }
        let interp = INTERPRETERS.contains(&p)
            || (p == "sed" && seg.first().args.iter().any(|a| a == "-i" || a.starts_with("-i")));
        let c = segment_class(seg);
        // `x | wc -l`, `x | grep -c y`: prints a count, like an echo
        if c.as_ref().is_some_and(|(_, why)| why.ends_with("|wc") || why.ends_with("|grep -c")) && others_exist {
            continue;
        }
        if let Some((k, _)) = &c {
            if !interp || *k == Class::Log {
                all.push(*k);
            }
        }
        if decided.is_some() || stopped {
            continue;
        }
        match c {
            Some(c) if !interp || c.0 == Class::Log => decided = Some(c),
            Some(c) => {
                weak.get_or_insert(c);
            }
            None => {
                if interp || PRINTERS.contains(&p) {
                    continue;
                }
                // the first substantive command is unknown: its output leads
                stopped = true;
            }
        }
    }
    (decided.or(weak), all)
}

// ── output sniffing ─────────────────────────────────────────────────────────

/// Lines/bytes sniffing looks at, from the start of the output.
const SNIFF_LINES: usize = 400;
const SNIFF_BYTES: usize = 32 * 1024;

fn is_grep_hit(l: &str) -> bool {
    crate::transforms::search::parse_hit(l).is_some()
}

fn is_numbered(l: &str) -> bool {
    let t = l.trim_start_matches(' ');
    let d = t.bytes().take_while(u8::is_ascii_digit).count();
    if d == 0 || d > 7 {
        return false;
    }
    let rest = &t.as_bytes()[d..];
    matches!(rest.first(), Some(b'\t' | b':' | b'|'))
        || rest.starts_with("→".as_bytes())
        || rest.starts_with(b"  ")
        || (rest.first() == Some(&b'-') && d > 0 && l.len() > d + 1)
}

fn is_ls_row(l: &str) -> bool {
    let b = l.as_bytes();
    b.len() > 12
        && matches!(b[0], b'-' | b'd' | b'l' | b'c' | b'b' | b'p' | b's')
        && b[1..10].iter().all(|c| matches!(c, b'r' | b'w' | b'x' | b's' | b'S' | b't' | b'T' | b'-'))
}

fn is_oneline(l: &str) -> bool {
    let t = l.strip_prefix("* ").unwrap_or(l);
    let h = t.bytes().take_while(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()).count();
    (7..=40).contains(&h) && t.as_bytes().get(h) == Some(&b' ')
}

fn is_diff_mark(l: &str) -> bool {
    l.starts_with("diff --git ")
        || l.starts_with("@@ -")
        || l.starts_with("+++ ")
        || l.starts_with("--- a/")
        || (l.starts_with("index ") && l.contains(".."))
}

fn is_git_status(l: &str) -> bool {
    l.starts_with("On branch ")
        || l.starts_with("Changes to be committed")
        || l.starts_with("Changes not staged")
        || l.starts_with("Untracked files:")
        || l.starts_with("nothing to commit")
        || l.starts_with("Your branch is")
        || {
            let b = l.as_bytes();
            b.len() > 3 && b"MADRCU? ".contains(&b[0]) && b"MADRCU? ".contains(&b[1]) && b[2] == b' ' && b[0] != b' '
                || (b.len() > 3 && b[0] == b' ' && b"MD".contains(&b[1]) && b[2] == b' ')
        }
}

fn is_log_marker(l: &str) -> bool {
    let t = l.trim_start();
    const STARTS: &[&str] = &[
        "Compiling ",
        "Checking ",
        "Finished ",
        "Running ",
        "Downloaded ",
        "Downloading ",
        "Fresh ",
        "Updating ",
        "Installing ",
        "Building ",
        "test result:",
        "error[",
        "error:",
        "warning:",
        "warning[",
        "Traceback (most recent call last)",
        "npm WARN",
        "npm ERR",
        "PASS ",
        "FAIL ",
        "ok ",
        "--- PASS",
        "--- FAIL",
        "✓ ",
        "✗ ",
        "✔ ",
        "✖ ",
        "Collecting ",
        "Successfully ",
        "Requirement already",
        "thread '",
        "panicked at",
        "INFO",
        "WARN",
        "ERROR",
        "DEBUG",
    ];
    if STARTS.iter().any(|s| t.starts_with(s)) {
        return true;
    }
    if l.starts_with("test ") && (l.ends_with(" ... ok") || l.ends_with(" ... FAILED") || l.ends_with(" ... ignored")) {
        return true;
    }
    if l.contains(" PASSED") || l.contains(" FAILED") {
        return true;
    }
    if t.starts_with("at ") && l.ends_with(')') && l.contains(':') {
        return true;
    }
    // [12:34:56] / 2026-10-30T... / [INFO] style timestamps and levels
    let b = t.as_bytes();
    if b.len() > 10
        && b[0] == b'['
        && (b[1].is_ascii_digit()
            || t[1..].starts_with("INFO")
            || t[1..].starts_with("WARN")
            || t[1..].starts_with("ERROR")
            || t[1..].starts_with("DEBUG"))
    {
        return true;
    }
    b.len() > 19
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[7] == b'-'
        && (b[10] == b'T' || b[10] == b' ')
        && b[13] == b':'
}

fn is_pathlike(l: &str) -> bool {
    let t = l.trim();
    if t.is_empty() || t.len() > 400 || t.contains(' ') && !t.contains('/') {
        return false;
    }
    let ok = t.bytes().all(|c| c.is_ascii_alphanumeric() || b"/._-+@~".contains(&c));
    ok && (t.contains('/')
        || t.rsplit_once('.').is_some_and(|(a, e)| {
            !a.is_empty() && (1..=8).contains(&e.len()) && e.bytes().all(|c| c.is_ascii_alphanumeric())
        }))
}

fn json_like(s: &str) -> bool {
    let b = s.as_bytes();
    let (Some(&f), Some(&l)) = (b.first(), b.last()) else { return false };
    match (f, l) {
        (b'{', b'}') => {
            let k = s[1..].trim_start();
            k.starts_with('"') || k.starts_with('}')
        }
        (b'[', b']') => {
            let k = s[1..].trim_start();
            k.is_empty()
                || matches!(k.as_bytes()[0], b'{' | b'[' | b'"' | b']' | b'-' | b'0'..=b'9' | b't' | b'f' | b'n')
        }
        _ => false,
    }
}

/// Class from the output text alone.
pub fn sniff(output: &str) -> Option<(Class, &'static str)> {
    let mut end = output.len().min(SNIFF_BYTES);
    while !output.is_char_boundary(end) {
        end -= 1;
    }
    let head = output[..end].trim();
    if head.is_empty() {
        return None;
    }
    if output.len() <= SNIFF_BYTES && json_like(output.trim()) {
        return Some((Class::Structured, "json"));
    }
    if output.len() > SNIFF_BYTES && json_like(head.split_at(1).0) && output.trim_end().ends_with(['}', ']']) {
        return Some((Class::Structured, "json"));
    }
    let lines: Vec<&str> = head.split('\n').filter(|l| !l.trim().is_empty()).take(SNIFF_LINES).collect();
    let n = lines.len();
    if n == 0 {
        return None;
    }
    let frac = |f: fn(&str) -> bool| lines.iter().filter(|l| f(l)).count() as f64 / n as f64;
    if n >= 3
        && lines.iter().take(20).all(|l| {
            let t = l.trim();
            t.starts_with('{') && t.ends_with('}')
        })
    {
        return Some((Class::Structured, "jsonl"));
    }
    if frac(is_diff_mark) > 0.02 && lines.iter().any(|l| l.starts_with("@@")) {
        return Some((Class::Content, "diff"));
    }
    if frac(is_ls_row) > 0.5 {
        return Some((Class::Listing, "ls -l rows"));
    }
    if frac(is_grep_hit) > 0.5 {
        return Some((Class::Search, "path:line: hits"));
    }
    if frac(is_numbered) > 0.6 {
        return Some((Class::Content, "numbered lines"));
    }
    if frac(is_oneline) > 0.6 {
        return Some((Class::Vcs, "git oneline"));
    }
    if frac(is_git_status) > 0.5 || head.starts_with("On branch ") {
        return Some((Class::Vcs, "git status"));
    }
    if frac(is_log_marker) > 0.15 {
        return Some((Class::Log, "build/test markers"));
    }
    if n >= 3 && frac(is_pathlike) > 0.7 {
        return Some((Class::Listing, "path list"));
    }
    if n >= 3 {
        let commas: Vec<usize> = lines.iter().take(50).map(|l| l.matches(',').count()).collect();
        if commas.iter().all(|&c| c >= 2 && c == commas[0]) {
            return Some((Class::Structured, "csv"));
        }
        let tabs: Vec<usize> = lines.iter().take(50).map(|l| l.matches('\t').count()).collect();
        if tabs.iter().all(|&c| c >= 2 && c == tabs[0]) {
            return Some((Class::Structured, "tsv"));
        }
        let yaml = lines
            .iter()
            .filter(|l| {
                let t = l.trim_start_matches([' ', '-']);
                t.split_once(": ").is_some_and(|(k, _)| {
                    !k.is_empty() && k.bytes().all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
                }) || (t.ends_with(':')
                    && t.len() > 1
                    && t[..t.len() - 1].bytes().all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c)))
            })
            .count();
        if yaml as f64 / n as f64 > 0.7 && !lines[0].starts_with('#') {
            return Some((Class::Structured, "yaml-ish"));
        }
    }
    None
}

/// Classify one tool result.
pub fn classify(tool: &str, command: Option<&str>, output: &str) -> Classification {
    let mk = |class: Class, reason: String| Classification { class, reason };
    if let Some(c) = native_tool(tool) {
        return mk(c, format!("tool:{tool}"));
    }
    if prose_tool(tool) {
        return mk(Class::Prose, format!("tool:{tool}"));
    }
    let shell_like = SHELL_TOOLS.contains(&tool) || command.is_some();
    if shell_like {
        if let Some(cmd) = command {
            let (first, all) = command_classes(cmd);
            if let Some((c, why)) = first {
                // Segments disagree: let the output pick among the commands'
                // own classes. Never away from CONTENT, except to SEARCH (whose
                // transform keeps every line's text byte-exact).
                if all.iter().any(|&k| k != c) {
                    if let Some((s, swhy)) = sniff(output) {
                        if s != c && all.contains(&s) && (c != Class::Content || s == Class::Search) {
                            return mk(s, format!("cmd+sniff:{swhy}"));
                        }
                    }
                }
                return mk(c, format!("cmd:{why}"));
            }
        }
    }
    if let Some((c, why)) = sniff(output) {
        return mk(c, format!("sniff:{why}"));
    }
    mk(Class::Unknown, if shell_like { "cmd:?".into() } else { format!("tool:{tool}") })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cls(cmd: &str, out: &str) -> Class {
        classify("bash", Some(cmd), out).class
    }

    #[test]
    fn phase0_selfcheck_cases() {
        assert_eq!(cls("cd /x && git log --oneline -5", "abc1234 msg\n"), Class::Vcs);
        assert_eq!(cls("sed -n '1,40p' src/main.rs", "fn main() {}\n"), Class::Content);
        assert_eq!(cls("grep -rn foo src | head", "src/a.rs:1:foo\n"), Class::Search);
        assert_eq!(cls("cat a.rs | grep -n fn", "1:fn x\n"), Class::Search);
        assert_eq!(cls("cargo test 2>&1 | tail -20", "test result: ok\n"), Class::Log);
        assert_eq!(cls("ls -la", "total 0\n"), Class::Listing);
        assert_eq!(cls("curl -s http://x", "{\"a\":1}"), Class::Structured);
        assert_eq!(cls("python3 - <<'EOF'\nprint(1)\nEOF", "hello there\n"), Class::Unknown);
        assert_eq!(classify("read", None, "1\tfoo\n").class, Class::Content);
        assert_eq!(cls("FOO=1 timeout 30 git -C repo status", "On branch main\n"), Class::Vcs);
        assert_eq!(cls("git diff HEAD~1", "diff --git a/x b/x\n"), Class::Content);
        assert_eq!(classify("subagent_collect", None, "The agent found...").class, Class::Prose);
    }

    #[test]
    fn logs_loops_heredocs() {
        assert_eq!(cls("cat oracle/all.log; grep -E x t.txt", "x"), Class::Log);
        assert_eq!(cls("for f in a.log b.log; do tail -n1 $f; done", "x"), Class::Log);
        assert_eq!(cls("cd x && python3 - <<'EOF'\np=1\nEOF\ncargo test 2>&1 | tail", "x"), Class::Log);
        assert_eq!(cls("python3 /tmp/x.py && rm /tmp/x.py && bash -n f && sed -n 1,20p f", "x"), Class::Content);
        assert_eq!(cls("grep -rln foo src | head", "src/a\n"), Class::Listing);
        assert_eq!(cls("tail -f /tmp/build.out", "x"), Class::Log);
        assert_eq!(cls("cat x.tsv | awk '{print $1}'", "a\tb\tc\nd\te\tf\ng\th\ti\n"), Class::Content);
        assert_eq!(cls("git show --stat HEAD", "x"), Class::Vcs);
        assert_eq!(cls("cat abstracts.txt | cut -c1-160", "x"), Class::Content);
        assert_eq!(cls("git show main:docs/h.md | wc -l; git show main:docs/h.md | head -30", "x"), Class::Content);
        assert_eq!(cls("git show HEAD:a.js > a.js && npx jest t.test.js 2>&1 | grep -E x", "x"), Class::Log);
        let paths = "./a/b.py\n./c/d.rs\n./e/f.md\n./g/h.txt\n";
        assert_eq!(
            cls("for d in a b; do (cd $d; git log --oneline -3; find . -type f | head); done", paths),
            Class::Listing
        );
    }

    #[test]
    fn prose_tools() {
        for t in ["forum_read", "memory_fetch", "web-tools_fetch", "ext__web-tools__fetch", "subagent_status"] {
            assert_eq!(classify(t, None, "x").class, Class::Prose, "{t}");
        }
    }
}
