//! A light shell parse: enough to tell what a command line prints, not a shell.
//!
//! Heredoc bodies are cut out, the line is split into words and operators with
//! quotes respected, and every simple command becomes a [`Stage`] of a
//! [`Segment`] (a pipeline between `&&`, `||`, `;`, `&` and newlines). Loop and
//! conditional keywords are peeled off, so `for f in *.log; do tail $f; done`
//! yields a `tail` stage with the loop's item list attached.

/// Longest command we look at. Anything past it is ignored.
const MAX_COMMAND: usize = 16 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stage {
    /// Program basename (`git`, `grep`, `python3`), wrappers and env removed.
    pub prog: String,
    /// Arguments after the program, quotes removed, redirections dropped.
    pub args: Vec<String>,
    /// stdout goes to a file (`> f`, `>> f`, `&> f`), so this prints nothing.
    pub silenced: bool,
}

impl Stage {
    pub fn has_flag(&self, short: char, long: &[&str]) -> bool {
        self.args.iter().any(|a| {
            if let Some(l) = a.strip_prefix("--") {
                let name = l.split('=').next().unwrap_or("");
                long.contains(&name)
            } else if let Some(s) = a.strip_prefix('-') {
                !s.is_empty()
                    && s.bytes().all(|b| b.is_ascii_alphanumeric())
                    && s.chars().take_while(|c| c.is_ascii_alphabetic()).any(|c| c == short)
            } else {
                false
            }
        })
    }

    /// First non-flag argument (the subcommand for git/cargo/docker...).
    pub fn sub(&self) -> &str {
        let mut i = 0;
        while i < self.args.len() {
            let a = &self.args[i];
            if self.prog == "git" && matches!(a.as_str(), "-C" | "-c" | "--git-dir" | "--work-tree") {
                i += 2;
                continue;
            }
            if a.starts_with('-') || a.starts_with('+') {
                i += 1;
                continue;
            }
            return a;
        }
        ""
    }

    /// Non-flag arguments (file operands, roughly).
    pub fn operands(&self) -> impl Iterator<Item = &str> {
        self.args.iter().map(String::as_str).filter(|a| !a.starts_with('-'))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Segment {
    /// Pipeline stages, in order.
    pub stages: Vec<Stage>,
    /// Item words of the enclosing `for x in ...` loop, if any.
    pub loop_items: Vec<String>,
    /// The segment's first stage reads a heredoc (`python3 - <<'EOF'`).
    pub heredoc: bool,
}

impl Segment {
    pub fn first(&self) -> &Stage {
        &self.stages[0]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Word(String),
    /// `&&`, `||`, `;`, `&`, newline: ends a segment.
    Sep,
    Pipe,
    /// A redirection operator; the next word is its target. `true` when it
    /// redirects stdout.
    Redir(bool),
    /// A redirection that carries its own target (`2>&1`, `>/dev/null`).
    RedirDone,
    HeredocOp,
}

/// Remove heredoc bodies; return the remaining text and whether any were cut.
fn strip_heredocs(cmd: &str) -> (String, bool) {
    if !cmd.contains("<<") {
        return (cmd.to_string(), false);
    }
    let mut out = String::with_capacity(cmd.len());
    let mut pending: Vec<(String, bool)> = Vec::new();
    let mut cut = false;
    for line in cmd.split('\n') {
        if let Some((delim, dash)) = pending.first() {
            let l = if *dash { line.trim_start_matches('\t') } else { line };
            if l.trim_end() == delim {
                pending.remove(0);
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
        // find `<<[-]['"]?WORD['"]?`, skipping `<<<` here-strings
        let b = line.as_bytes();
        let mut i = 0;
        while i + 1 < b.len() {
            if b[i] == b'<' && b[i + 1] == b'<' {
                if i + 2 < b.len() && b[i + 2] == b'<' {
                    i += 3;
                    continue;
                }
                let mut j = i + 2;
                let dash = j < b.len() && b[j] == b'-';
                if dash {
                    j += 1;
                }
                while j < b.len() && b[j] == b' ' {
                    j += 1;
                }
                let q = if j < b.len() && (b[j] == b'\'' || b[j] == b'"') { Some(b[j]) } else { None };
                if q.is_some() {
                    j += 1;
                }
                let s = j;
                while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_' || b[j] == b'-') {
                    j += 1;
                }
                if j > s {
                    pending.push((line[s..j].to_string(), dash));
                    cut = true;
                }
                if let Some(q) = q {
                    if j < b.len() && b[j] == q {
                        j += 1;
                    }
                }
                i = j;
            } else {
                i += 1;
            }
        }
    }
    (out, cut)
}

fn lex(s: &str) -> Vec<Tok> {
    let b = s.as_bytes();
    let mut toks = Vec::new();
    let mut i = 0;
    let mut word = String::new();
    let mut in_word = false;
    macro_rules! flush {
        () => {
            if in_word {
                toks.push(Tok::Word(std::mem::take(&mut word)));
            }
            #[allow(unused_assignments)]
            {
                in_word = false;
            }
        };
    }
    while i < b.len() {
        let c = b[i];
        match c {
            b' ' | b'\t' | b'\r' => {
                flush!();
                i += 1;
            }
            b'\n' | b';' => {
                flush!();
                toks.push(Tok::Sep);
                i += 1;
            }
            b'#' if !in_word => {
                // comment to end of line
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'&' => {
                flush!();
                if b.get(i + 1) == Some(&b'&') {
                    i += 2;
                } else if b.get(i + 1) == Some(&b'>') {
                    // &>file
                    toks.push(Tok::Redir(true));
                    i += 2;
                    if b.get(i) == Some(&b'>') {
                        i += 1;
                    }
                    continue;
                } else {
                    i += 1;
                }
                toks.push(Tok::Sep);
            }
            b'|' => {
                flush!();
                if b.get(i + 1) == Some(&b'|') {
                    toks.push(Tok::Sep);
                    i += 2;
                } else {
                    toks.push(Tok::Pipe);
                    i += if b.get(i + 1) == Some(&b'&') { 2 } else { 1 };
                }
            }
            b'(' | b')' | b'{' | b'}' if !in_word => {
                // grouping: treat as a segment boundary
                toks.push(Tok::Sep);
                i += 1;
            }
            b'<' | b'>' => {
                // a pure-digit word right before it is an fd number
                let fd = in_word && word.bytes().all(|x| x.is_ascii_digit());
                let stdout = c == b'>' && (!fd || word == "1");
                if fd {
                    word.clear();
                    in_word = false;
                } else {
                    flush!();
                }
                if c == b'<' && b.get(i + 1) == Some(&b'<') {
                    toks.push(Tok::HeredocOp);
                    i += 2;
                    while i < b.len() && matches!(b[i], b'<' | b'-' | b'~') {
                        i += 1;
                    }
                    // the delimiter word follows; swallow it
                    while i < b.len() && b[i] == b' ' {
                        i += 1;
                    }
                    while i < b.len() && !matches!(b[i], b' ' | b'\t' | b'\n' | b';' | b'|' | b'&' | b')') {
                        i += 1;
                    }
                    continue;
                }
                i += 1;
                while i < b.len() && matches!(b[i], b'>' | b'|') {
                    i += 1;
                }
                if b.get(i) == Some(&b'&') {
                    // >&2, 2>&1
                    i += 1;
                    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'-') {
                        i += 1;
                    }
                    toks.push(Tok::RedirDone);
                    continue;
                }
                if c == b'<' && b.get(i) == Some(&b'(') {
                    // process substitution <(...): skip it as a word
                    let end = skip_parens(b, i);
                    toks.push(Tok::Word(s[i - 1..end].to_string()));
                    i = end;
                    continue;
                }
                while i < b.len() && b[i] == b' ' {
                    i += 1;
                }
                toks.push(Tok::Redir(stdout));
            }
            b'\'' => {
                in_word = true;
                let end = s[i + 1..].find('\'').map_or(b.len(), |p| i + 1 + p);
                word.push_str(&s[i + 1..end]);
                i = (end + 1).min(b.len());
            }
            b'"' => {
                in_word = true;
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' && i + 1 < b.len() {
                        i += 1;
                    }
                    let ch = s[i..].chars().next().unwrap_or(' ');
                    word.push(ch);
                    i += ch.len_utf8();
                }
                i += 1;
            }
            b'\\' => {
                in_word = true;
                if let Some(ch) = s.get(i + 1..).and_then(|r| r.chars().next()) {
                    if ch != '\n' {
                        word.push(ch);
                    }
                    i += 1 + ch.len_utf8();
                } else {
                    i += 1;
                }
            }
            b'$' if b.get(i + 1) == Some(&b'(') => {
                in_word = true;
                let end = skip_parens(b, i + 1);
                word.push_str(&s[i..end]);
                i = end;
            }
            b'`' => {
                in_word = true;
                let end = s[i + 1..].find('`').map_or(b.len(), |p| i + 2 + p);
                word.push_str(&s[i..end.min(b.len())]);
                i = end.min(b.len());
            }
            _ => {
                in_word = true;
                let ch = s[i..].chars().next().unwrap_or(' ');
                word.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    flush!();
    toks
}

/// Index just past the `)` matching the `(` at `open`.
fn skip_parens(b: &[u8], open: usize) -> usize {
    let mut depth = 0usize;
    let mut i = open;
    let mut q: Option<u8> = None;
    while i < b.len() {
        let c = b[i];
        match q {
            Some(qc) => {
                if c == b'\\' && qc == b'"' {
                    i += 1;
                } else if c == qc {
                    q = None;
                }
            }
            None => match c {
                b'\'' | b'"' => q = Some(c),
                b'(' => depth += 1,
                b')' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return i + 1;
                    }
                }
                _ => {}
            },
        }
        i += 1;
    }
    b.len()
}

const WRAPPERS: &[&str] = &[
    "sudo",
    "time",
    "nice",
    "ionice",
    "command",
    "env",
    "exec",
    "nohup",
    "stdbuf",
    "unbuffer",
    "timeout",
    "xargs",
    "watch",
    "doas",
    "builtin",
    "setsid",
    "chronic",
    "caffeinate",
    "taskset",
];
const KEYWORDS: &[&str] = &["do", "then", "else", "elif", "if", "while", "until", "!", "time"];
const ENDERS: &[&str] = &["done", "fi", "esac", "}", ";;"];

fn is_assignment(w: &str) -> bool {
    let Some(eq) = w.find('=') else { return false };
    eq > 0
        && w[..eq]
            .bytes()
            .enumerate()
            .all(|(k, c)| c == b'_' || c.is_ascii_alphabetic() || (k > 0 && c.is_ascii_digit()))
}

fn basename(w: &str) -> &str {
    w.rsplit('/').next().unwrap_or(w)
}

/// Turn the words of one simple command into a stage (wrappers, env and
/// keywords removed). `None` when nothing is left.
fn stage_of(words: &[String]) -> Option<Stage> {
    let mut i = 0;
    while i < words.len() {
        let w = words[i].as_str();
        if is_assignment(w) || KEYWORDS.contains(&w) {
            i += 1;
            continue;
        }
        if WRAPPERS.contains(&basename(w)) {
            let wrapper = basename(w);
            i += 1;
            // wrapper flags and their numeric/duration args (timeout 30, nice -n 5)
            while i < words.len() {
                let a = words[i].as_str();
                let numeric = !a.is_empty()
                    && a.bytes().all(|c| c.is_ascii_digit() || c == b'.' || c == b's' || c == b'm' || c == b'h')
                    && a.as_bytes()[0].is_ascii_digit();
                if a.starts_with('-') || numeric || is_assignment(a) {
                    // `-n 5`, `-u user`, `-s KILL`: skip a short flag's value too
                    let takes_value = wrapper != "env"
                        && a.len() == 2
                        && a.starts_with('-')
                        && i + 1 < words.len()
                        && !words[i + 1].starts_with('-');
                    i += if takes_value { 2 } else { 1 };
                    continue;
                }
                break;
            }
            continue;
        }
        break;
    }
    let rest = &words[i.min(words.len())..];
    let first = rest.first()?;
    let prog = basename(first).to_string();
    if prog.is_empty() || ENDERS.contains(&prog.as_str()) {
        return None;
    }
    Some(Stage { prog, args: rest[1..].to_vec(), silenced: false })
}

/// Parse a command line into segments. Never fails; garbage in, few segments out.
pub fn parse(command: &str) -> Vec<Segment> {
    let cmd = if command.len() > MAX_COMMAND {
        let mut cut = MAX_COMMAND;
        while !command.is_char_boundary(cut) {
            cut -= 1;
        }
        &command[..cut]
    } else {
        command
    };
    let (text, _) = strip_heredocs(cmd);
    let toks = lex(&text);
    let mut segs: Vec<Segment> = Vec::new();
    let mut cur = Segment::default();
    let mut words: Vec<String> = Vec::new();
    let mut loop_items: Vec<String> = Vec::new();
    let mut loop_depth = 0usize;
    let mut skip_next_word = false;
    let mut redirect_stdout = false;
    let mut silenced = false;
    let mut heredoc = false;

    let end_stage = |words: &mut Vec<String>, cur: &mut Segment, silenced: &mut bool| {
        if let Some(mut st) = stage_of(words) {
            st.silenced = *silenced;
            cur.stages.push(st);
        }
        *silenced = false;
        words.clear();
    };
    let mut i = 0;
    while i < toks.len() {
        match &toks[i] {
            Tok::Word(w) => {
                if skip_next_word {
                    skip_next_word = false;
                    if redirect_stdout && !matches!(w.as_str(), "/dev/stderr" | "/dev/tty" | "/dev/stdout") {
                        silenced = true;
                    }
                } else if words.is_empty() && (w == "for" || w == "select") {
                    // for VAR in ITEMS... ; do
                    let mut j = i + 1;
                    let mut items = Vec::new();
                    let mut seen_in = false;
                    while j < toks.len() {
                        match &toks[j] {
                            Tok::Word(x) if x == "in" => seen_in = true,
                            Tok::Word(x) if seen_in => items.push(x.clone()),
                            Tok::Word(_) => {}
                            _ => break,
                        }
                        j += 1;
                    }
                    loop_items = items;
                    loop_depth += 1;
                    i = j;
                    continue;
                } else if words.is_empty() && w == "done" {
                    loop_depth = loop_depth.saturating_sub(1);
                    if loop_depth == 0 {
                        loop_items.clear();
                    }
                } else if words.is_empty() && w == "case" {
                    // skip `case X in`
                    while i < toks.len() && !matches!(&toks[i], Tok::Word(x) if x == "in") {
                        i += 1;
                    }
                } else if words.is_empty() && w.ends_with(')') && !w.contains('(') && !w.starts_with('$') {
                    // a case pattern like `foo)`: skip
                } else {
                    words.push(w.clone());
                }
            }
            Tok::Redir(out) => {
                skip_next_word = true;
                redirect_stdout = *out;
            }
            Tok::RedirDone => {}
            Tok::HeredocOp => heredoc = true,
            Tok::Pipe => end_stage(&mut words, &mut cur, &mut silenced),
            Tok::Sep => {
                end_stage(&mut words, &mut cur, &mut silenced);
                if !cur.stages.is_empty() {
                    cur.loop_items = loop_items.clone();
                    cur.heredoc = heredoc;
                    segs.push(std::mem::take(&mut cur));
                }
                cur = Segment::default();
                heredoc = false;
            }
        }
        i += 1;
    }
    end_stage(&mut words, &mut cur, &mut silenced);
    if !cur.stages.is_empty() {
        cur.loop_items = loop_items;
        cur.heredoc = heredoc;
        segs.push(cur);
    }
    segs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progs(c: &str) -> Vec<Vec<String>> {
        parse(c).iter().map(|s| s.stages.iter().map(|st| st.prog.clone()).collect()).collect()
    }

    #[test]
    fn chains_pipes_wrappers() {
        assert_eq!(progs("cd /x && git log --oneline -5 | head"), vec![vec!["cd"], vec!["git", "head"]]);
        assert_eq!(progs("FOO=1 timeout 30 git -C repo status"), vec![vec!["git"]]);
        assert_eq!(progs("sudo -u bob nice -n 5 ls -la"), vec![vec!["ls"]]);
        assert_eq!(progs("cargo test 2>&1 | tail -20"), vec![vec!["cargo", "tail"]]);
    }

    #[test]
    fn heredoc_bodies_are_cut() {
        let c = "cd x && python3 - <<'EOF'\nimport os\nprint('a | b; c')\nEOF\ncargo test 2>&1 | tail -5";
        assert_eq!(progs(c), vec![vec!["cd"], vec!["python3"], vec!["cargo", "tail"]]);
        assert!(parse(c)[1].heredoc);
    }

    #[test]
    fn loops_keep_items() {
        let s = parse("for f in a.log b.log; do tail -n1 $f; done");
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].first().prog, "tail");
        assert_eq!(s[0].loop_items, vec!["a.log", "b.log"]);
    }

    #[test]
    fn quotes_and_subst() {
        let s = parse(r#"grep -rn "a|b; c" src | grep -v 'x && y' $(echo z)"#);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].stages.len(), 2);
        assert_eq!(s[0].first().args, vec!["-rn", "a|b; c", "src"]);
    }

    #[test]
    fn flags() {
        let s = parse("grep -rln foo .");
        assert!(s[0].first().has_flag('l', &["files-with-matches"]));
        assert!(!s[0].first().has_flag('c', &["count"]));
        let s = parse("git -C repo log --oneline");
        assert_eq!(s[0].first().sub(), "log");
    }

    #[test]
    fn redirects() {
        let s = parse("git show HEAD:a.js > a.js && npx jest 2>&1 | grep x; cat f >&2; echo x > /dev/stderr");
        assert!(s[0].first().silenced);
        assert!(!s[1].first().silenced);
        assert!(!s[2].first().silenced);
        assert!(!s[3].first().silenced);
        assert!(parse("cargo build &> /tmp/b.log")[0].first().silenced);
        assert!(!parse("cargo build 2> /tmp/err")[0].first().silenced);
    }

    #[test]
    fn garbage_is_fine() {
        for c in ["", "'''", "$(((", "<<", "a <<'X'\nb", "| | ;;", "\\", "for", "case x in"] {
            let _ = parse(c);
        }
    }
}
