//! Per-tool schema registry. Maps a Synaps tool (by `tool_name`, or by sniffing
//! a `bash` command's first word) to the [`Schema`] describing its columnar
//! output. Unknown tools return `None` and fall back to the generic pipeline.

use super::columnar::Schema;

/// `ls -lah` (the format the native `ls` tool emits): permissions, link count,
/// owner, group, human size, month, day, time, then the free-text name.
pub const LS: Schema = Schema {
    cols: &[
        "perms", "links", "owner", "group", "size", "month", "day", "time",
    ],
    tail: "name",
    skip_total: true,
};

/// Resolve a columnar schema for a tool invocation, or `None` for "no specialist
/// — use the generic pipeline".
pub fn schema_for(tool_name: &str, command: &str) -> Option<&'static Schema> {
    match tool_name {
        "ls" => Some(&LS),
        // bash/shell: sniff the leading program of the command.
        "bash" | "shell" | "shell_send" => match leading_program(command) {
            "ls" => Some(&LS),
            _ => None,
        },
        _ => None,
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
    fn unknown_tools_fall_back() {
        assert!(schema_for("read", "").is_none());
        assert!(schema_for("bash", "cat foo.txt").is_none());
        assert!(schema_for("grep", "").is_none());
    }
}
