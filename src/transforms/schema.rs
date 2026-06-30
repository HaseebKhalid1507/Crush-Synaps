//! Per-tool schema registry. Maps a Synaps tool (by `tool_name`, or by sniffing
//! a `bash` command's first word) to the [`Schema`] describing its columnar
//! output. Unknown tools return `None` and fall back to the generic pipeline.
//!
//! Dispatch is intentionally strict (exact program match) but **safe even when
//! wrong**: [`super::columnar::fold`] validates the output shape and bails to the
//! generic pipeline if a line doesn't fit the schema. So a `git status` that
//! sniffs as `git` simply doesn't fold — it never corrupts.

use super::columnar::{Delim, Schema};

/// `ls -lah` (the format the native `ls` tool emits): permissions, link count,
/// owner, group, human size, month, day, time, then the free-text name.
pub const LS: Schema = Schema {
    cols: &[
        "perms", "links", "owner", "group", "size", "month", "day", "time",
    ],
    tail: "name",
    delim: Delim::Whitespace,
    skip_total: true,
    skip_header: false,
};

/// `ps aux`: USER PID %CPU %MEM VSZ RSS TTY STAT START TIME, then the free-text
/// COMMAND. The first line is a column-name header, held aside.
pub const PS: Schema = Schema {
    cols: &[
        "USER", "PID", "%CPU", "%MEM", "VSZ", "RSS", "TTY", "STAT", "START", "TIME",
    ],
    tail: "COMMAND",
    delim: Delim::Whitespace,
    skip_total: false,
    skip_header: true,
};

/// `git log --pretty=format:%H|%an|%ae|%ad|%s` — the canonical agent-friendly
/// pipe-delimited format. Author/email factor or dictionary to almost nothing;
/// the subject (which may itself contain `|`) is the free-text tail.
pub const GITLOG: Schema = Schema {
    cols: &["sha", "author", "email", "date"],
    tail: "subject",
    delim: Delim::Char('|'),
    skip_total: false,
    skip_header: false,
};

/// Resolve a columnar schema for a tool invocation, or `None` for "no specialist
/// — use the generic pipeline".
pub fn schema_for(tool_name: &str, command: &str) -> Option<&'static Schema> {
    match tool_name {
        "ls" => Some(&LS),
        "bash" | "shell" | "shell_send" => match leading_program(command) {
            "ls" => Some(&LS),
            "ps" => Some(&PS),
            // Only the pipe-delimited `git log` format folds; fold() validates
            // and bails for any other git output (status/diff/default log).
            "git" if command.contains("log") => Some(&GITLOG),
            _ => None,
        },
        _ => None,
    }
}

/// Mirror of [`schema_for`]'s dispatch returning a short bucket label for
/// stats (`"ls"`, `"ps"`, `"git_log"`). Stays in lockstep with the dispatch
/// above so the label is always accurate.
pub fn label_for(tool_name: &str, command: &str) -> &'static str {
    match tool_name {
        "ls" => "ls",
        "bash" | "shell" | "shell_send" => match leading_program(command) {
            "ls" => "ls",
            "ps" => "ps",
            "git" if command.contains("log") => "git_log",
            _ => "columnar",
        },
        _ => "columnar",
    }
}

/// First whitespace-delimited token of a command, skipping leading env-var
/// assignments (`FOO=bar ls ...`).
fn leading_program(command: &str) -> &str {
    for tok in command.split_whitespace() {
        if tok.contains('=') {
            continue; // env assignment prefix
        }
        return tok.rsplit('/').next().unwrap_or(tok);
    }
    ""
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_ls_resolves_to_ls_schema() {
        assert!(schema_for("ls", "").is_some());
    }

    #[test]
    fn bash_ls_command_resolves() {
        assert!(schema_for("bash", "ls -lah /usr/bin").is_some());
        assert!(schema_for("bash", "/bin/ls -la").is_some());
        assert!(schema_for("bash", "LC_ALL=C ls -l").is_some());
    }

    #[test]
    fn bash_ps_command_resolves() {
        assert!(schema_for("bash", "ps aux").is_some());
    }

    #[test]
    fn git_log_resolves_but_other_git_does_not() {
        assert!(schema_for("bash", "git log --pretty=format:%H|%an").is_some());
        assert!(schema_for("bash", "git status").is_none());
        assert!(schema_for("bash", "git diff").is_none());
    }

    #[test]
    fn unknown_tools_fall_back() {
        assert!(schema_for("read", "").is_none());
        assert!(schema_for("bash", "cat foo.txt").is_none());
        assert!(schema_for("grep", "").is_none());
    }
}
