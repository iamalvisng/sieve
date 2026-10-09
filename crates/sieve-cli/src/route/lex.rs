//! The Bash lexer of the routing hook (`docs/design/lookup-routing-slice3.md`,
//! section A). It splits one command into segments and sorts each segment.

/// One word of a segment, with its quotes removed.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Word {
    /// The text of the word as the shell passes it.
    pub(super) text: String,
    /// Some part of the word was quoted or escaped.
    pub(super) quoted: bool,
}

/// The separator that ends a segment.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub(super) enum Sep {
    /// The text ends.
    #[default]
    End,
    /// `;` or a newline.
    Semi,
    /// `&&`.
    And,
    /// `||`.
    Or,
    /// `|` or `|&`.
    Pipe,
    /// A single `&`: the shell runs the segment in the background.
    Amp,
}

/// One command of a Bash call, between two separators.
#[derive(Debug, Default)]
pub(super) struct Segment {
    /// The words, with the redirect tokens removed.
    pub(super) words: Vec<Word>,
    /// A pipe feeds this segment.
    pub(super) piped_in: bool,
    /// A pipe follows this segment.
    pub(super) piped_out: bool,
    /// The file of a `<file` redirect.
    pub(super) stdin: Option<String>,
    /// The separator that ended this segment.
    pub(super) sep: Sep,
}

/// The result of the lexer.
#[derive(Debug)]
pub(super) enum Lexed {
    /// The text holds a construct the hook does not read.
    Unsafe,
    /// The segments, in order.
    Segments(Vec<Segment>),
}

/// What one segment is, for the routing rules.
#[derive(Debug, PartialEq)]
pub(super) enum Kind {
    /// A `grep`, `rg` or `git grep` command. `start` is the index of the
    /// first word after the tool name.
    Search { rg: bool, start: usize },
    /// A `cd`, `pushd` or `popd` command.
    Cd,
    /// A shell keyword starts the segment.
    Keyword,
    /// `xargs` or `find` runs a search for each file.
    FanOut,
    /// Any other command.
    Other,
}

/// Words that start a shell compound command. The hook does not read them.
const KEYWORDS: [&str; 13] = [
    "for", "while", "until", "if", "case", "do", "then", "else", "elif", "done", "fi", "esac", "!",
];

#[derive(Default)]
struct Lexer {
    segments: Vec<Segment>,
    cur: Segment,
    word: String,
    in_word: bool,
    quoted: bool,
    /// A redirect waits for its target word. `true` is a `<file` source.
    redirect: Option<bool>,
    piped_next: bool,
}

impl Lexer {
    fn push(&mut self, c: char, quoted: bool) {
        self.word.push(c);
        self.in_word = true;
        self.quoted |= quoted;
    }

    fn start_quoted(&mut self) {
        self.in_word = true;
        self.quoted = true;
    }

    fn end_word(&mut self) {
        if !self.in_word {
            return;
        }
        let text = std::mem::take(&mut self.word);
        let quoted = std::mem::take(&mut self.quoted);
        self.in_word = false;
        match self.redirect.take() {
            Some(true) => self.cur.stdin = Some(text),
            Some(false) => {}
            None => self.cur.words.push(Word { text, quoted }),
        }
    }

    fn end_segment(&mut self, sep: Sep) {
        let pipe = sep == Sep::Pipe;
        self.end_word();
        self.redirect = None;
        let mut seg = std::mem::take(&mut self.cur);
        if seg.words.is_empty() && seg.stdin.is_none() {
            return;
        }
        seg.piped_in = self.piped_next;
        seg.piped_out = pipe;
        seg.sep = sep;
        self.piped_next = pipe;
        self.segments.push(seg);
    }

    /// Ends the word before a redirect. A digit-only word is the file
    /// descriptor (`2>`), so the lexer drops it.
    fn before_redirect(&mut self) {
        if self.in_word && !self.quoted && self.word.bytes().all(|b| b.is_ascii_digit()) {
            self.word.clear();
            self.in_word = false;
        } else {
            self.end_word();
        }
    }
}

/// Splits `command` into segments. The lexer splits on `&&`, `||`, `;`, `|`,
/// `|&`, `&` and a newline outside quotes. It never splits `2>&1`.
pub(super) fn lex(command: &str) -> Lexed {
    let chars: Vec<char> = command.chars().collect();
    let mut lx = Lexer::default();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        i += 1;
        match c {
            '\\' => match next {
                Some('\n') => i += 1,
                Some(n) => {
                    lx.push(n, true);
                    i += 1;
                }
                None => {}
            },
            '\'' => {
                lx.start_quoted();
                loop {
                    match chars.get(i) {
                        None => return Lexed::Unsafe,
                        Some('\'') => {
                            i += 1;
                            break;
                        }
                        Some(&q) => {
                            lx.word.push(q);
                            i += 1;
                        }
                    }
                }
            }
            '"' => {
                lx.start_quoted();
                loop {
                    let Some(&q) = chars.get(i) else {
                        return Lexed::Unsafe;
                    };
                    i += 1;
                    match q {
                        '"' => break,
                        '`' => return Lexed::Unsafe,
                        '$' if chars.get(i) == Some(&'(') => return Lexed::Unsafe,
                        '\\' => match chars.get(i) {
                            Some('\n') => i += 1,
                            Some(&n) if matches!(n, '"' | '\\' | '$' | '`') => {
                                lx.word.push(n);
                                i += 1;
                            }
                            _ => lx.word.push('\\'),
                        },
                        _ => lx.word.push(q),
                    }
                }
            }
            '`' | '(' | '{' => return Lexed::Unsafe,
            '$' if next == Some('(') => return Lexed::Unsafe,
            ' ' | '\t' | '\r' => lx.end_word(),
            '\n' | ';' => lx.end_segment(Sep::Semi),
            '#' if !lx.in_word => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '|' => match next {
                Some('|') => {
                    i += 1;
                    lx.end_segment(Sep::Or);
                }
                Some('&') => {
                    i += 1;
                    lx.end_segment(Sep::Pipe);
                }
                _ => lx.end_segment(Sep::Pipe),
            },
            '&' => match next {
                Some('&') => {
                    i += 1;
                    lx.end_segment(Sep::And);
                }
                Some('>') => {
                    lx.end_word();
                    i += 1;
                    if chars.get(i) == Some(&'>') {
                        i += 1;
                    }
                    lx.redirect = Some(false);
                }
                _ => lx.end_segment(Sep::Amp),
            },
            '>' => {
                lx.before_redirect();
                if matches!(next, Some('>' | '&' | '|')) {
                    i += 1;
                }
                lx.redirect = Some(false);
            }
            '<' => {
                if next == Some('<') {
                    return Lexed::Unsafe;
                }
                lx.before_redirect();
                let dup = next == Some('&');
                if dup {
                    i += 1;
                }
                lx.redirect = Some(!dup);
            }
            _ => lx.push(c, false),
        }
    }
    lx.end_segment(Sep::End);
    Lexed::Segments(lx.segments)
}

/// True if the word is `NAME=value`.
fn is_assignment(word: &Word) -> bool {
    word.text.split_once('=').is_some_and(|(key, _)| {
        !key.is_empty()
            && !key.starts_with(|c: char| c.is_ascii_digit())
            && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// The index of the command word: the first word after `NAME=value`,
/// `time`, `sudo` and `env`.
fn command_index(words: &[Word]) -> usize {
    let skip = |w: &Word| {
        is_assignment(w) || (!w.quoted && matches!(w.text.as_str(), "time" | "sudo" | "env"))
    };
    words.iter().position(|w| !skip(w)).unwrap_or(words.len())
}

/// Sorts one segment.
pub(super) fn classify(seg: &Segment) -> Kind {
    let words = &seg.words;
    let Some(first) = words.first() else {
        return Kind::Other;
    };
    if !first.quoted && KEYWORDS.contains(&first.text.as_str()) {
        return Kind::Keyword;
    }
    if !first.quoted && matches!(first.text.as_str(), "cd" | "pushd" | "popd") {
        return Kind::Cd;
    }
    let idx = command_index(words);
    let Some(cmd) = words.get(idx).filter(|w| !w.quoted) else {
        return Kind::Other;
    };
    let tool = |w: &Word| matches!(w.text.as_str(), "grep" | "rg");
    match cmd.text.as_str() {
        "grep" => Kind::Search {
            rg: false,
            start: idx + 1,
        },
        "rg" => Kind::Search {
            rg: true,
            start: idx + 1,
        },
        "git" if words.get(idx + 1).is_some_and(|w| w.text == "grep") => Kind::Search {
            rg: false,
            start: idx + 2,
        },
        "xargs" | "find" if words[idx + 1..].iter().any(tool) => Kind::FanOut,
        _ => Kind::Other,
    }
}

/// True if the text holds a `grep` or `rg` word.
pub(super) fn mentions_search(command: &str) -> bool {
    command
        .split(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '.')))
        .any(|t| matches!(t, "grep" | "rg"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(command: &str) -> Vec<Vec<String>> {
        match lex(command) {
            Lexed::Segments(segs) => segs
                .iter()
                .map(|s| s.words.iter().map(|w| w.text.clone()).collect())
                .collect(),
            Lexed::Unsafe => vec![vec!["UNSAFE".to_string()]],
        }
    }

    fn row(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn test_route_lexer_splits_outside_quotes_only() {
        assert_eq!(
            texts("cd a && grep -n \"x|y;z&&w\" f | head -3; echo 'a || b'"),
            vec![
                row(&["cd", "a"]),
                row(&["grep", "-n", "x|y;z&&w", "f"]),
                row(&["head", "-3"]),
                row(&["echo", "a || b"]),
            ]
        );
        // A redirect token never splits and never stays in the words.
        assert_eq!(
            texts("grep -n x f 2>&1 >/dev/null &> out; grep y g 1>&2"),
            vec![row(&["grep", "-n", "x", "f"]), row(&["grep", "y", "g"])]
        );
        assert_eq!(
            texts("a |& b & c\nd"),
            vec![row(&["a"]), row(&["b"]), row(&["c"]), row(&["d"]),]
        );
    }

    #[test]
    fn test_route_lexer_stops_on_unreadable_text() {
        for command in [
            "grep -n $(cat p) f",
            "grep -n `cat p` f",
            "grep -n \"$(cat p)\" f",
            "grep -n \"`cat p`\" f",
            "cat <<EOF",
            "(grep -n x f)",
            "echo {a,b}",
            "grep -n 'open",
            "grep -n \"open",
        ] {
            assert_eq!(texts(command), vec![row(&["UNSAFE"])], "{command}");
        }
        // The same characters are literal inside single quotes, a double
        // quote pair and after a backslash.
        for command in [
            "grep -rnE '^\\s+(async |static )*(x)\\b' src",
            "grep -rnE \"^\\s+(async |static )*(x)\\b\" src",
            "grep -n 'a{2}' f",
            "grep -n \"a\\$(b\" f",
            "grep -n a\\(b f",
            "grep -o '[A-Za-z`.-]*' f",
        ] {
            assert_ne!(texts(command), vec![row(&["UNSAFE"])], "{command}");
        }
    }

    #[test]
    fn test_route_lexer_reads_stdin_redirect() {
        let Lexed::Segments(segs) = lex("grep -n x < src/a.ts") else {
            panic!("unsafe");
        };
        assert_eq!(segs[0].stdin.as_deref(), Some("src/a.ts"));
        assert_eq!(segs[0].words.len(), 3);
    }

    #[test]
    fn test_route_lexer_classifies_segments() {
        let kind = |c: &str| match lex(c) {
            Lexed::Segments(s) => classify(&s[0]),
            Lexed::Unsafe => Kind::Other,
        };
        assert_eq!(
            kind("grep -n x f"),
            Kind::Search {
                rg: false,
                start: 1
            }
        );
        assert_eq!(
            kind("A=1 time sudo rg x"),
            Kind::Search { rg: true, start: 4 }
        );
        assert_eq!(
            kind("git grep -n x"),
            Kind::Search {
                rg: false,
                start: 2
            }
        );
        assert_eq!(
            kind("env X=1 grep x"),
            Kind::Search {
                rg: false,
                start: 3
            }
        );
        assert_eq!(kind("for f in a; do grep x; done"), Kind::Keyword);
        assert_eq!(kind("! grep x f"), Kind::Keyword);
        assert_eq!(kind("xargs grep -n x"), Kind::FanOut);
        assert_eq!(kind("find . -exec grep -n x {} +"), Kind::Other);
        assert_eq!(kind("find . -name x -exec grep -n x f"), Kind::FanOut);
        assert_eq!(kind("cd /tmp"), Kind::Cd);
        assert_eq!(kind("cat f"), Kind::Other);
        assert!(mentions_search("echo $(grep x)"));
        assert!(!mentions_search("echo a.grep"));
    }
}
