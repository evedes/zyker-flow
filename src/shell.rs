//! Reading a Bash tool call: the simple commands in it, and what each one writes.

use crate::is_space;
use regex::Regex;
use std::sync::LazyLock;

/// The commands only the user may run.
pub const USER_ONLY: [&str; 4] = ["approve", "pause", "resume", "abandon"];

/// `<<` or `<<-` and the heredoc's word, bare or quoted. A word is letters, numbers and
/// `_`: a mark or `²` after it is not part of it, whatever the regex crate's own `\w`
/// says. One preceded by `<` is a here-string (`<<<`), not a heredoc.
static HEREDOC: LazyLock<Regex> =
    regex!(r#"<<(-?)[\s\x1c-\x1f]*(?:'([\p{L}\p{N}_]+)'|"([\p{L}\p{N}_]+)"|([\p{L}\p{N}_]+))"#);
/// Arithmetic, where `<<` is a shift.
static ARITH: LazyLock<Regex> = regex!(r"\$?\(\(.*?\)\)");
static CAT_READER: LazyLock<Regex> = regex!(r"^(?:\w+=\S*\s+)*cat\b");
/// The rest of the line pipes the command's output on (`| sh`, `|& sh`, not `||`).
static PIPED: LazyLock<Regex> = regex!(r"^[^;&|]*\|(?:[^|]|$)");
static DISCARD: LazyLock<Regex> = regex!(r"\d*>[\s\x1c-\x1f]*/dev/null|\d*>&\d");
/// A tool that writes the paths it names.
pub static MUTATES: LazyLock<Regex> = regex!(concat!(
    r"\btee\b|\bsed\s+(-\S*\s+)*-i|\bperl\s+(-\S*\s+)*-i|\brm\b|\bmv\b|\bcp\b|",
    r"\btouch\b|\btruncate\b|\bpatch\b|\bln\b|",
    r"\bgit\s+(checkout|restore|apply|stash|reset|mv|rm)\b"
));
pub static GIT_COMMIT: LazyLock<Regex> = regex!(r"\bgit\s+(?:-[cC]\s+\S+\s+|--\S+\s+)*commit\b");
pub static GIT_PUSH: LazyLock<Regex> = regex!(r"\bgit\s+(?:-[cC]\s+\S+\s+|--\S+\s+)*push\b");
/// A user-only command. The word ends at anything but a letter, a number or `_`, so
/// `flow approve\u{301}` (a combining mark) is still caught.
pub static USER_ONLY_CMD: LazyLock<Regex> = regex!(&format!(
    r#"(^|[\s'"(=])(\S*/)?flow\s+({})(?:[^\p{{L}}\p{{N}}_]|$)"#,
    USER_ONLY.join("|")
));

/// Split shell text on unquoted &&, ||, ;, |, & and newlines (not the & of `>&`, `<&`
/// or `&>`).
pub fn split_commands(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut quote = None;
    let mut prev = None;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let mut last = c;
        match quote {
            Some(q) => {
                cur.push(c);
                if c == '\\' && q == '"' {
                    if let Some(n) = chars.next() {
                        cur.push(n);
                        last = n;
                    }
                } else if c == q {
                    quote = None;
                }
            }
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                cur.push(c);
            }
            None if c == '\\' => {
                cur.push(c);
                if let Some(n) = chars.next() {
                    cur.push(n);
                    last = n;
                }
            }
            None if ";|&\n".contains(c)
                && !(c == '&'
                    && (prev.is_none_or(|p| p == '<' || p == '>')
                        || chars.peek() == Some(&'>'))) =>
            {
                parts.push(std::mem::take(&mut cur));
            }
            None => cur.push(c),
        }
        prev = Some(last);
    }
    parts.push(cur);
    parts.iter().map(|p| p.trim()).filter(|p| !p.is_empty()).map(String::from).collect()
}

/// The simple commands in a Bash tool call. A heredoc fed to `cat` is data (its text
/// lands in the redirect target), so its body is dropped; any other heredoc body (a
/// script for python, sh, psql..., or for whatever `cat` pipes it to) is kept as part of
/// the last command on its line.
pub fn bash_commands(cmd: &str) -> Vec<String> {
    let mut lines = cmd.split('\n');
    let mut out = Vec::new();
    while let Some(line) = lines.next() {
        let mut bodies = Vec::new();
        // Arithmetic blanked out, at the same byte offsets as in `line`.
        let scan = ARITH.replace_all(line, |c: &regex::Captures| " ".repeat(c[0].len()));
        for m in HEREDOC.captures_iter(&scan) {
            let (start, end) = m.get(0).map_or((0, 0), |g| (g.start(), g.end()));
            if scan[..start].ends_with('<') {
                continue;
            }
            let dash = !m[1].is_empty();
            let word = (2..=4).find_map(|i| m.get(i)).map_or("", |g| g.as_str());
            let mut body = Vec::new();
            for l in lines.by_ref() {
                if (if dash { l.trim_start_matches('\t') } else { l }) == word {
                    break;
                }
                body.push(l);
            }
            let reader = split_commands(&line[..start]);
            let piped = PIPED.is_match(&scan[end..]);
            if piped || !reader.last().is_some_and(|r| CAT_READER.is_match(r)) {
                bodies.push(body.join(" "));
            }
        }
        let mut cmds = split_commands(line);
        if let (false, Some(last)) = (bodies.is_empty(), cmds.last_mut()) {
            last.push(' ');
            last.push_str(&bodies.join(" "));
        }
        out.extend(cmds);
    }
    out
}

/// A word that is an absolute path, or one under `~`.
static ABSOLUTE: LazyLock<Regex> = regex!(r#"(?:^|[\s\x1c-\x1f'"=])(~?/[^\s\x1c-\x1f'";&|()<>]*)"#);

/// The absolute paths a simple command names, `~` expanded from $HOME (when absolute).
pub fn absolute_words(cmd: &str) -> Vec<String> {
    let home = std::env::var("HOME").ok().filter(|h| h.starts_with('/'));
    let words = ABSOLUTE.captures_iter(cmd).map(|m| m[1].to_string());
    let expand = |w: String| match (w.strip_prefix('~'), &home) {
        (Some(rest), Some(home)) => Some(format!("{}{rest}", home.trim_end_matches('/'))),
        (Some(_), None) => None,
        (None, _) => Some(w),
    };
    words.filter_map(expand).collect()
}

/// `git commit` options that take a value, and those that commit more than the index.
const COMMIT_VALUE_OPTS: [&str; 16] = [
    "-m",
    "-F",
    "-C",
    "-c",
    "-t",
    "--message",
    "--file",
    "--author",
    "--date",
    "--template",
    "--reuse-message",
    "--reedit-message",
    "--fixup",
    "--squash",
    "--cleanup",
    "--trailer",
];
const COMMIT_WIDENS: [&str; 6] =
    ["--all", "--include", "--only", "--interactive", "--patch", "--pathspec-from-file"];

/// Whether the Bash call is a lone `git commit` that takes only what is staged: no other
/// command (a `git add` before it would stage more, as would one in a `$(...)`, backtick
/// or `<(...)` substitution), no -a/-i/-o/-p, no pathspec.
pub fn commits_index_only(cmds: &[String]) -> bool {
    let [cmd] = cmds else {
        return false;
    };
    if ["$(", "`", "<(", ">("].iter().any(|s| cmd.contains(s)) {
        return false;
    }
    let Some(words) = shell_words(cmd) else {
        return false;
    };
    if words.len() < 2 || words[0] != "git" || words[1] != "commit" {
        return false;
    }
    let mut rest = words[2..].iter();
    while let Some(w) = rest.next() {
        if let Some(long) = w.strip_prefix("--") {
            let name = format!("--{}", long.split('=').next().unwrap_or(""));
            if w == "--" || COMMIT_WIDENS.contains(&name.as_str()) {
                return false;
            }
            if COMMIT_VALUE_OPTS.contains(&name.as_str()) && !w.contains('=') {
                rest.next();
            }
        } else if let Some(flags) = w.strip_prefix('-').filter(|f| !f.is_empty()) {
            for (i, c) in flags.char_indices() {
                if "aiop".contains(c) {
                    return false;
                }
                if COMMIT_VALUE_OPTS.contains(&format!("-{c}").as_str()) {
                    if i + c.len_utf8() == flags.len() {
                        rest.next();
                    }
                    break;
                }
            }
        } else {
            return false; // a pathspec
        }
    }
    true
}

/// A simple command split into words as the shell would (quotes removed, backslashes
/// applied); None when a quote is left open.
fn shell_words(cmd: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = cmd.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                let w = word.get_or_insert_with(String::new);
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => w.push(c),
                    }
                }
            }
            '"' => {
                let w = word.get_or_insert_with(String::new);
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => match chars.next()? {
                            c @ ('"' | '\\' | '$' | '`') => w.push(c),
                            '\n' => {}
                            c => {
                                w.push('\\');
                                w.push(c);
                            }
                        },
                        c => w.push(c),
                    }
                }
            }
            '\\' => match chars.next() {
                Some('\n') | None => {}
                Some(c) => word.get_or_insert_with(String::new).push(c),
            },
            c if crate::is_space(c) => words.extend(word.take()),
            c => word.get_or_insert_with(String::new).push(c),
        }
    }
    words.extend(word);
    Some(words)
}

/// A simple command without its redirects to /dev/null and between descriptors.
pub fn without_discards(cmd: &str) -> String {
    DISCARD.replace_all(cmd, "").into_owned()
}

/// The files a simple command redirects into: the word after each `>` or `>>` that does
/// not follow `<`, `&` or `>` (so not `<>`, `&>` or the second `>` of `>>`), with one
/// opening quote dropped.
pub fn redirect_targets(cmd: &str) -> Vec<&str> {
    let ends_word = |c: char| is_space(c) || "'\";&|()<>".contains(c);
    let mut targets = Vec::new();
    let mut from = 0;
    while let Some(at) = cmd[from..].find('>').map(|i| from + i) {
        from = at + 1;
        if cmd[..at].ends_with(['<', '&', '>']) {
            continue;
        }
        let rest = &cmd[at + 1..];
        let rest = rest.strip_prefix('>').unwrap_or(rest).trim_start_matches(is_space);
        let rest = rest.strip_prefix(['\'', '"']).unwrap_or(rest);
        let end = rest.find(ends_word).unwrap_or(rest.len());
        if end > 0 {
            targets.push(&rest[..end]);
            from = cmd.len() - rest.len() + end;
        }
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rustfmt::skip]
    #[test]
    fn reads_commands_like_the_shell() {
        let cases: [(&str, &[&str]); 9] = [
            ("a && b; c | d & e", &["a", "b", "c", "d", "e"]),
            ("ls >/dev/null 2>&1 &>x <&0", &["ls >/dev/null 2>&1 &>x <&0"]),
            ("&x", &["&x"]),
            ("echo 'a;b' \"c\\\"|d\"", &["echo 'a;b' \"c\\\"|d\""]),
            ("a\\;b a\\>&b", &["a\\;b a\\>&b"]),
            ("cat > /tmp/m <<'EOF'\nlib/x\nEOF\ngit status", &["cat > /tmp/m <<'EOF'", "git status"]),
            ("A=1 cat <<EOF\nrm x\nEOF", &["A=1 cat <<EOF"]),
            ("python3 - <<EOF\nopen('lib/x','w')\nEOF", &["python3 - <<EOF open('lib/x','w')"]),
            ("sh <<-X\n\trm a\n\tX\nls", &["sh <<-X \trm a", "ls"]),
        ];
        for (cmd, want) in cases {
            assert_eq!(bash_commands(cmd), want, "{cmd:?}");
        }
        let words = |s: &str| shell_words(s).map(|w| w.join("|"));
        assert_eq!(words(r#"git commit -m 'a b' --author="x \"y\"" c\ d"#).as_deref(),
                   Some(r#"git|commit|-m|a b|--author=x "y"|c d"#));
        assert_eq!(words("git commit -m 'open"), None);
        for (cmd, staged) in [
            ("git commit -qm x --author='a <a@b>'", true), ("git commit -am x", false),
            ("git commit -m x lib", false), ("git commit -mx -v", true),
            ("git commit -m \"$(git add -A)\"", false), ("git -C . commit", false),
        ] {
            assert_eq!(commits_index_only(&[cmd.to_string()]), staged, "{cmd}");
        }
        assert!(!commits_index_only(&["git add x".into(), "git commit -m x".into()]));
        let c = without_discards("ls lib >/dev/null 2>&1 >> out 3>x <>y &>z >'a b'");
        assert_eq!(redirect_targets(&c), ["out", "x", "a"]);
    }
}
