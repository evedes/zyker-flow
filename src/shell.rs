//! Reading a Bash tool call: the simple commands in it, and what each one writes.

use crate::is_space;
use regex::Regex;
use std::sync::LazyLock;

/// The commands only the user may run.
pub const USER_ONLY: [&str; 4] = ["approve", "pause", "resume", "abandon"];

/// `<<` or `<<-` and the heredoc's word, bare or quoted. A word is letters, numbers and
/// `_`: a mark or `²` after it is not part of it, whatever the regex crate's own `\w`
/// says.
static HEREDOC: LazyLock<Regex> =
    regex!(r#"<<(-?)[\s\x1c-\x1f]*(?:'([\p{L}\p{N}_]+)'|"([\p{L}\p{N}_]+)"|([\p{L}\p{N}_]+))"#);
static CAT_READER: LazyLock<Regex> = regex!(r"^(?:\w+=\S*\s+)*cat\b");
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
/// script for python, sh, psql...) is kept as part of the command that reads it.
pub fn bash_commands(cmd: &str) -> Vec<String> {
    let mut lines = cmd.split('\n');
    let mut out = Vec::new();
    while let Some(line) = lines.next() {
        let mut bodies = Vec::new();
        for m in HEREDOC.captures_iter(line) {
            let dash = !m[1].is_empty();
            let word = (2..=4).find_map(|i| m.get(i)).map_or("", |g| g.as_str());
            let mut body = Vec::new();
            for l in lines.by_ref() {
                if (if dash { l.trim_start_matches('\t') } else { l }) == word {
                    break;
                }
                body.push(l);
            }
            let start = m.get(0).map_or(0, |g| g.start());
            let reader = split_commands(&line[..start]);
            if !reader.last().is_some_and(|r| CAT_READER.is_match(r)) {
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
        let c = without_discards("ls lib >/dev/null 2>&1 >> out 3>x <>y &>z >'a b'");
        assert_eq!(redirect_targets(&c), ["out", "x", "a"]);
    }
}
