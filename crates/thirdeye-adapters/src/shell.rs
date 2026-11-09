//! Which files a shell command reads — a small, certain subset.
//!
//! Codex reads only through its shell, and every agent's shell tool can read
//! (`cat file` in Claude's `Bash` is a read too). Interpreting arbitrary
//! shell is impossible, so this recognizes the forms captured from real
//! sessions (`docs/codex-hook-payload.md`) and the obvious relatives, and
//! returns nothing for anything uncertain: a missed read costs little, a
//! wrong one lists a file that was never read.
//!
//! Recognized: `cat`, `nl`, `less`, `more`, `tail`, `head` (with its line
//! count), and `sed -n` with a single `Np`/`N,Mp` script. Never: a word
//! containing `$`, a glob, a backtick or `~`; a redirection's target; a
//! heredoc's body; `sed -i`.

use std::path::Path;

use thirdeye_core::{AgentId, EventKind, FileEvent, LineRange, LogRecord, Source, Timestamp};

/// One file a command read, and the lines, when the command says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Read {
    pub path: String,
    pub range: Option<LineRange>,
}

/// The files `command` reads.
pub fn reads(command: &str) -> Vec<Read> {
    let Some(tokens) = tokenize(command) else {
        return Vec::new();
    };
    let commands: Vec<&[Token]> = tokens.split(|t| matches!(t, Token::Sep)).collect();
    // `cd sub && cat a.rs` reads sub/a.rs. Every path after a change of
    // directory names something other than it says, so none is trusted.
    if commands
        .iter()
        .any(|c| matches!(c.first(), Some(Token::Word(w, _)) if matches!(w.as_str(), "cd" | "pushd" | "popd")))
    {
        return Vec::new();
    }
    commands.into_iter().flat_map(simple_reads).collect()
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    /// A word, and whether it is certain — no expansion, glob or
    /// substitution could change what it names.
    Word(String, bool),
    /// `>`, `>>`, `<`, `<<` and kin: the next word is not an argument.
    Redirect,
    /// `|`, `||`, `&&`, `;`, `&`, an unquoted newline: a new simple command.
    Sep,
}

/// Split a whole command into tokens — the whole of it, not line by line,
/// because a quoted string can span lines (every commit message Claude Code
/// writes does) and its lines are not commands. `None` when the command
/// holds a subshell or process substitution: what runs where is then too
/// uncertain to name a file.
fn tokenize(command: &str) -> Option<Vec<Token>> {
    let mut lx = Lexer::default();
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                lx.in_word = true;
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    lx.word.push(q);
                }
            }
            '"' => {
                lx.in_word = true;
                while let Some(q) = chars.next() {
                    match q {
                        '"' => break,
                        '\\' => lx.word.extend(chars.next()),
                        '$' | '`' => {
                            lx.certain = false;
                            lx.word.push(q);
                        }
                        _ => lx.word.push(q),
                    }
                }
            }
            // A continuation joins the next line to this one.
            '\\' if chars.peek() == Some(&'\n') => {
                chars.next();
            }
            '\\' => {
                lx.in_word = true;
                lx.word.extend(chars.next());
            }
            '\n' => {
                lx.flush();
                lx.tokens.push(Token::Sep);
                // A heredoc's body follows the line that started it, up to
                // its terminator, and is input — never commands.
                if let Some(end) = lx.heredoc.take() {
                    let mut line = String::new();
                    for b in chars.by_ref() {
                        if b == '\n' {
                            if line.trim() == end {
                                break;
                            }
                            line.clear();
                        } else {
                            line.push(b);
                        }
                    }
                }
            }
            '#' if !lx.in_word => while chars.next_if(|n| *n != '\n').is_some() {},
            '(' | ')' => return None,
            c if c.is_whitespace() => lx.flush(),
            '|' | '&' | ';' => {
                lx.flush();
                while chars.peek().is_some_and(|n| matches!(n, '|' | '&')) {
                    chars.next();
                }
                lx.tokens.push(Token::Sep);
            }
            '<' | '>' => {
                // `2>`: the digits were a file descriptor, not an argument.
                if lx.in_word && lx.word.chars().all(|d| d.is_ascii_digit()) {
                    lx.word.clear();
                    lx.in_word = false;
                }
                lx.flush();
                let mut op = c.to_string();
                while let Some(n) = chars.next_if(|n| matches!(n, '<' | '>' | '&' | '-')) {
                    op.push(n);
                }
                lx.heredoc_next = op.starts_with("<<") && !op.starts_with("<<<");
                lx.tokens.push(Token::Redirect);
            }
            '$' | '*' | '?' | '`' | '[' | '~' => {
                lx.in_word = true;
                lx.certain = false;
                lx.word.push(c);
            }
            _ => {
                lx.in_word = true;
                lx.word.push(c);
            }
        }
    }
    lx.flush();
    Some(lx.tokens)
}

/// The tokenizer's state.
struct Lexer {
    tokens: Vec<Token>,
    word: String,
    in_word: bool,
    certain: bool,
    /// The next word ends a heredoc started on this line.
    heredoc_next: bool,
    heredoc: Option<String>,
}

impl Default for Lexer {
    fn default() -> Self {
        Self {
            tokens: Vec::new(),
            word: String::new(),
            in_word: false,
            certain: true,
            heredoc_next: false,
            heredoc: None,
        }
    }
}

impl Lexer {
    /// End the word in progress, if there is one.
    fn flush(&mut self) {
        if self.in_word {
            if std::mem::take(&mut self.heredoc_next) {
                self.heredoc = Some(self.word.clone());
            }
            self.tokens
                .push(Token::Word(std::mem::take(&mut self.word), self.certain));
        }
        self.in_word = false;
        self.certain = true;
    }
}

/// The reads one simple command makes.
fn simple_reads(tokens: &[Token]) -> Vec<Read> {
    // Arguments, with redirection targets dropped; `None` for an uncertain
    // word, which can still be a flag's value and must keep its place.
    let mut words: Vec<Option<String>> = Vec::new();
    let mut skip_next = false;
    for t in tokens {
        match t {
            Token::Redirect => skip_next = true,
            Token::Word(w, certain) => {
                if std::mem::take(&mut skip_next) {
                    continue;
                }
                words.push(certain.then(|| w.clone()));
            }
            Token::Sep => {}
        }
    }
    let Some(Some(program)) = words.first() else {
        return Vec::new();
    };
    let args = &words[1..];
    let whole = |paths: Vec<String>| -> Vec<Read> {
        paths
            .into_iter()
            .map(|path| Read { path, range: None })
            .collect()
    };

    match program.rsplit('/').next().unwrap_or(program) {
        "cat" => whole(files(args, |_| false)),
        // Every short option of nl takes a value except -p; the long ones
        // take theirs after `=`, or as the next word.
        "nl" => whole(files(args, |o| {
            (o.len() == 2 && o != "-p") || (o.starts_with("--") && !o.contains('='))
        })),
        "tail" => whole(files(args, |o| {
            matches!(
                o,
                "-n" | "-c" | "-s" | "--lines" | "--bytes" | "--sleep-interval" | "--pid"
            )
        })),
        // less and more take `+cmd` and options too varied to guess at.
        "less" | "more" if args.iter().flatten().any(|a| a.starts_with(['+', '-'])) => Vec::new(),
        "less" | "more" => whole(files(args, |_| false)),
        "head" => head_reads(args),
        "sed" => sed_reads(args),
        _ => Vec::new(),
    }
}

/// The file arguments: not options, not their values, everything after
/// `--`. `takes_value` names the options whose value is the next word.
fn files(args: &[Option<String>], takes_value: impl Fn(&str) -> bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip = false;
    let mut options_over = false;
    for a in args {
        if std::mem::take(&mut skip) {
            continue;
        }
        let Some(a) = a else {
            continue;
        };
        if options_over {
            out.push(a.clone());
        } else if a == "--" {
            options_over = true;
        } else if a.starts_with(['-', '+']) {
            skip = takes_value(a);
        } else {
            out.push(a.clone());
        }
    }
    out
}

/// `head` reads lines 1 to N — unless it counts bytes (`-c`), or counts
/// from the end (`-n -5`), when which lines is unknown.
fn head_reads(args: &[Option<String>]) -> Vec<Read> {
    let mut count: Option<u32> = Some(10);
    let mut iter = args.iter().flatten();
    while let Some(a) = iter.next() {
        if a.starts_with("-c") || a.starts_with("--bytes") {
            if a == "-c" || a == "--bytes" {
                iter.next();
            }
            count = None;
            continue;
        }
        let value = match a.as_str() {
            "-n" | "--lines" => iter.next().map(String::as_str),
            _ => a
                .strip_prefix("--lines=")
                .or_else(|| a.strip_prefix("-n"))
                .or_else(|| {
                    a.strip_prefix('-')
                        .filter(|n| n.starts_with(|c: char| c.is_ascii_digit()))
                }),
        };
        if let Some(v) = value
            && count.is_some()
        {
            count = if v.starts_with('-') {
                None
            } else {
                v.parse().ok().or(count)
            };
        }
    }
    let takes_value = |o: &str| matches!(o, "-n" | "-c" | "--lines" | "--bytes");
    files(args, takes_value)
        .into_iter()
        .map(|path| Read {
            path,
            range: count.map(|n| LineRange::new(1, n.saturating_add(1))),
        })
        .collect()
}

/// `sed -n 'Np' file` / `sed -n 'N,Mp' file`, and nothing else: any other
/// script may edit, and `-i` certainly does.
fn sed_reads(args: &[Option<String>]) -> Vec<Read> {
    if args.iter().any(|a| {
        a.as_deref()
            .is_none_or(|a| a.starts_with("-i") || a.starts_with("--in-place"))
    }) {
        return Vec::new();
    }
    let words: Vec<&str> = args.iter().flatten().map(String::as_str).collect();
    if !words.contains(&"-n") {
        return Vec::new();
    }
    let rest: Vec<&str> = words.into_iter().filter(|w| *w != "-n").collect();
    let [script, files @ ..] = rest.as_slice() else {
        return Vec::new();
    };
    let Some(spec) = script.strip_suffix('p') else {
        return Vec::new();
    };
    let range = match spec.split_once(',') {
        Some((a, b)) => (a.parse::<u32>().ok(), b.parse::<u32>().ok()),
        None => (spec.parse::<u32>().ok(), spec.parse::<u32>().ok()),
    };
    // Line 0 does not exist; sed rejects it, and so does this.
    let (Some(start @ 1..), Some(end)) = range else {
        return Vec::new();
    };
    files
        .iter()
        .filter(|f| !f.starts_with('-'))
        .map(|f| Read {
            path: (*f).to_string(),
            range: Some(LineRange::new(start, end.saturating_add(1))),
        })
        .collect()
}

/// What one shell tool call contributes to the log on its own: a `Read` for
/// each file its command reads, joined to its turn. What it *wrote* comes
/// from watching the workspace, never from the command text.
///
/// `resolve` turns the command's path into the absolute one the agent's own
/// tools would — each adapter knows how its agent spells paths.
pub(crate) fn read_records(
    command: &str,
    agent: AgentId,
    session: Option<&str>,
    turn: Option<&str>,
    call: Option<&str>,
    now: Timestamp,
    resolve: impl Fn(&str) -> std::path::PathBuf,
) -> Vec<LogRecord> {
    reads(command)
        .into_iter()
        .map(|read| {
            let path = resolve(&read.path);
            let mut e = FileEvent::new(
                now,
                agent.clone(),
                &path,
                EventKind::Read { range: read.range },
                Source::Hook,
            );
            e.session = session.map(str::to_owned);
            e.prompt_id = turn.map(str::to_owned);
            // One call can read several files; each is its own record.
            e.dedup_key = call.map(|c| format!("{c}#read#{}", path.display()));
            LogRecord::File(e)
        })
        .collect()
}

/// A relative path joined to the working directory — the reading every
/// shell gives a bare file name.
pub(crate) fn under(cwd: Option<&Path>, path: &str) -> std::path::PathBuf {
    match cwd {
        Some(cwd) if !Path::new(path).is_absolute() => cwd.join(path),
        _ => path.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(path: &str, range: Option<(u32, u32)>) -> Read {
        Read {
            path: path.into(),
            range: range.map(|(s, e)| LineRange::new(s, e)),
        }
    }

    #[test]
    fn the_reads_codex_was_captured_making_are_recognized() {
        // docs/codex-hook-payload.md, verbatim.
        assert_eq!(reads("cat data.txt"), vec![r("data.txt", None)]);
        assert_eq!(
            reads("sed -n 2p data.txt"),
            vec![r("data.txt", Some((2, 3)))]
        );
        assert_eq!(
            reads(
                "sed -n '1,2p' data.txt && python3 - <<'PY'\nfrom pathlib import Path\np = Path('data.txt')\ncat = 1\nPY"
            ),
            vec![r("data.txt", Some((1, 3)))]
        );
    }

    #[test]
    fn head_and_tail_and_friends_read_their_files() {
        assert_eq!(
            reads("head -n 20 src/a.rs"),
            vec![r("src/a.rs", Some((1, 21)))]
        );
        assert_eq!(reads("head -5 a.rs"), vec![r("a.rs", Some((1, 6)))]);
        assert_eq!(reads("head a.rs"), vec![r("a.rs", Some((1, 11)))]);
        assert_eq!(reads("tail -n 5 log.txt"), vec![r("log.txt", None)]);
        assert_eq!(
            reads("nl -ba main.rs | sed -n '10,20p'"),
            vec![r("main.rs", None)]
        );
        assert_eq!(
            reads("cat a.rs b.rs"),
            vec![r("a.rs", None), r("b.rs", None)]
        );
    }

    #[test]
    fn writes_and_uncertain_words_are_never_reads() {
        // A wrong guess puts a file in the list that was never read.
        assert!(reads("cat > notes.md").is_empty());
        assert!(reads("cat <<EOF > out.txt\nhello\nEOF").is_empty());
        assert!(reads("cat $FILE").is_empty());
        assert!(reads("cat *.rs").is_empty());
        assert!(reads("cat `ls`").is_empty());
        assert!(reads("sed -i 's/a/b/' data.txt").is_empty());
        assert!(reads("printf 'x' > shell.txt").is_empty());
        assert!(reads("cat").is_empty());
    }

    #[test]
    fn quoting_is_honoured() {
        assert_eq!(reads("cat 'my file.txt'"), vec![r("my file.txt", None)]);
        assert_eq!(reads(r#"cat "a b.rs""#), vec![r("a b.rs", None)]);
    }

    #[test]
    fn a_multi_line_quoted_string_is_not_commands() {
        // How Claude Code writes every commit message: the body is a string.
        let commit = "git commit -m \"$(cat <<'EOF'\nfix: more robust handling of x\n\ncat secret.rs\nEOF\n)\"";
        assert!(reads(commit).is_empty(), "{:?}", reads(commit));
        assert!(reads("echo \"first\ncat secret.rs\"").is_empty());
        assert!(reads("python3 -c \"\nhead = df.head()\n\"").is_empty());
    }

    #[test]
    fn a_continued_line_is_one_command() {
        assert_eq!(reads("cat \\\n  a.rs"), vec![r("a.rs", None)]);
        assert_eq!(reads("sed -n 1p \\\n  f.rs"), vec![r("f.rs", Some((1, 2)))]);
    }

    #[test]
    fn option_values_and_comments_are_never_files() {
        assert!(
            reads("less +G f.rs").is_empty(),
            "less options are not worth guessing"
        );
        assert_eq!(reads("nl -v 5 f.rs"), vec![r("f.rs", None)]);
        assert_eq!(reads("nl -s ' ' f.rs"), vec![r("f.rs", None)]);
        assert_eq!(reads("tail --lines 5 f.rs"), vec![r("f.rs", None)]);
        assert_eq!(reads("cat a.rs # check it"), vec![r("a.rs", None)]);
        assert_eq!(reads("cat -- -odd.rs"), vec![r("-odd.rs", None)]);
        assert!(reads("cat <(sort a.rs) b.rs").is_empty());
        assert!(reads("(cat a.rs)").is_empty());
    }

    #[test]
    fn a_change_of_directory_makes_every_path_uncertain() {
        // `cd sub && cat a.rs` reads sub/a.rs; joined to the session's
        // directory it would name another file, or none.
        assert!(reads("cd sub && cat a.rs").is_empty());
        assert!(reads("pushd sub; head a.rs").is_empty());
    }

    #[test]
    fn line_counts_that_are_not_lines_one_to_n_give_no_range() {
        assert_eq!(reads("head -c 5 f.rs"), vec![r("f.rs", None)]);
        assert_eq!(reads("head -n -5 f.rs"), vec![r("f.rs", None)]);
        assert_eq!(
            reads("head -n 4294967295 f.rs"),
            vec![r("f.rs", Some((1, u32::MAX)))]
        );
        assert!(reads("sed -n 0p f.rs").is_empty());
        assert!(reads("sed -n 1p f.rs --in-place").is_empty());
    }
}
