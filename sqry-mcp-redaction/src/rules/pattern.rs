//! Pattern-based detection of paths in arbitrary string fields.
//!
//! This module finds the absolute paths embedded in a string that is not
//! itself under a path key (an error message, a log line, a value under a
//! key the walker does not know). The rule is general rather than a list of
//! prefixes: an absolute path is found whatever directory it starts in,
//! within the limits stated at the end of this documentation.
//!
//! In text, the forms found are:
//!
//! - a `file:` URI in any letter case (`file:///x`, `FILE:///x`, `file:/x`,
//!   `file://host/x`, and the `file:` URI of `git+file:///x`);
//! - a UNC path whose server name is letters, digits, `_`, `.` and `-`
//!   (`\\server\share\x`), also as `Debug` writes it inside a
//!   string, every backslash doubled (`\\\\server\\share\\x`);
//! - a Windows drive path (`C:\x`, `c:/x`, `C:\\x` as `Debug` writes it);
//! - a Unix absolute path (`/x`, `/dev/shm/x`, `//server/share`): a `/`
//!   followed by a path character, at the start of the text or after
//!   whitespace, punctuation, an operator or a shell redirection (`=`, `:`,
//!   `,`, `;`, `(`, `|`, `&`, `+`, `%`, `@`, `$`, `#`, `*`, `!`, `?`, `^`,
//!   `>`, `<`, a quote; not `-`, `_`, `.` or `~`, which continue a word),
//!   after a one-letter flag that opens a token
//!   (`-I/usr/include`), after an ellipsis (`see.../x`), and after a `~`
//!   that follows a word (`x~/x`); also a Unix path whose slashes are all
//!   JSON-escaped and whose first segment follows `\/` directly
//!   (`\/home\/u\/s`, redacted as `/home/u/s` would be; `\/\/srv` is not
//!   found, see the limits).
//!
//! An escape written out in text by `Debug`, JSON or C is a boundary before
//! every one of these forms, as whitespace is: a backslash that no other
//! backslash escapes, then `n`, `t`, `r`, `0`, `f`, `v`, `b`, `a` or `e`,
//! `x` and two hex digits, `u` and four hex digits, or `u{`, one to six hex
//! digits and `}` (`line\nC:\x`, `line\u000afile:///x`, `line\t\\srv\s`,
//! `line\x0a/x`).
//!
//! A path ends at whitespace, a quote, a backtick, an angle bracket, a
//! closing bracket it did not open (`/a/My(1)/b` is one path; `(/x)` ends
//! before the `)`), or an unescaped backslash where an escape really
//! begins (the second backslash of a `\\` pair never begins one, so a pair
//! is never split): `\"` or `\'` for every path (so `Debug`'s
//! `\"/home/u/s\"` keeps its closing `\"`); for a UNC path as `Debug`
//! writes it, any written escape; for a Unix path, `\n`, `\r` or `\t`
//! whatever follows (`/x/secret.rs\nhint`), `\\` that no path character
//! follows, and any other written escape that the end of the text, a
//! character that cannot continue a path, or a `/` follows (`/x\b/y`, whose
//! `/y` is a path of its own). Any other backslash stays in a Unix path and
//! is redacted with it (`/mnt/c/Users\alice\secret.txt`,
//! `/home/u/s\backup/x`, `/home/u/a\xyz/x`, `Debug`'s `/home/alice\\t/x`).
//! A Windows or raw UNC path keeps all its own backslashes (`C:\new\x`). A URL of a scheme other
//! than `file` keeps its scheme, authority and path, but its query,
//! parameters and fragment are scanned as other text is
//! (`ws://h/a?f=/home/u/z` redacts `/home/u/z`). These are not paths and
//! are left as they are:
//!
//! - a URL's own path (`https://host/a/b`, `sqry://docs/x`), also after an
//!   escape;
//! - a relative path (`src/lib.rs`, `./x`, `../x`, `~/x`, `$HOME/x`), where
//!   the `/` follows a word, `_`, `.`, `-`, or a `~` opening a token;
//! - an endpoint's qualified name exactly as sqry's plugins write it,
//!   `route::<METHOD>::/path` with `route` opening a token and one of `GET`,
//!   `POST`, `PUT`, `DELETE`, `PATCH`, `HEAD`, `OPTIONS`, `ALL`
//!   (`Foo::BAR::/x` is a path);
//! - a URI fragment or JSON pointer after a word, a `/` or a quote
//!   (`schema.json#/defs/x`, `"$ref": "#/definitions/X"` in a code
//!   snippet), a glob (`**/x`, `a*/b`), `c++/x`, a scoped package
//!   (`@scope/pkg`), a placeholder (`<workspace>/x`), a closing markup tag
//!   (`</div>`) and a comment delimiter (`// x`, `/* x */`);
//! - a sqry regular-expression literal after the `~=` operator
//!   (`name~=/.*/i`) when nothing after it can continue into a path: no
//!   whitespace before its closing `/`, then only flags (`i`, `m`, `s`),
//!   then the end of the text, whitespace, a quote, a backtick, one of
//!   `)`, `]`, `}`, `<`, `>`, `,`, `;`, `:`, `!`, `?`, or a `.` that the end,
//!   whitespace, a quote, a backtick or a closing bracket follows. Any
//!   other literal (`name~=/foo`, `name~=/x/./home/u/s`) fails safe: it is
//!   scanned as other text is, so it never hides a later path. A regex
//!   between slashes elsewhere is left when its first character cannot
//!   open a path segment (`/^a+$/i`, `/[a-z]+/`).
//!
//! Free-text detection is heuristic and incomplete: it scans text for the
//! shapes a path takes, and cannot see every way a path can be written. A
//! structural redesign (paths emitted through a typed formatter, so
//! redaction is exact rather than a text scan) is tracked separately as
//! verivus-oss/sqry#908; until then the limits below are frozen, and
//! each is pinned by a test that asserts today's behaviour (the exact output,
//! or for the over-redaction, whitespace and glued-word groups the exact path
//! detected), so any
//! change in what stays readable is visible.
//!
//! The limits, with what stays readable:
//!
//! - Over-redaction: a token that only reads as an absolute path is
//!   redacted (`/results/0`, `#/definitions/x` after whitespace or at the
//!   start of the text, the `/foo` of `name~=/foo`, `/api/users`,
//!   `?next=/api/users`, `@/components/x`, `/foo/i` outside `~=`, `/2`);
//!   only its last segment stays readable.
//! - Whitespace inside a path: the path is found only up to its first
//!   whitespace; `/mnt/My Dir/x` leaves `Dir/x` readable.
//! - A path glued to a word longer than a one-letter flag, or after a
//!   backslash that begins no escape: not found, wholly readable
//!   (`-isystem/usr/include`, `1/home/u/x`, `a\q/x`, `x\\n/x`, `x\\nC:\x`,
//!   a short `\x0` or `\u00a`, `a\\/home/u/x`).
//! - A backslash-separated segment that is exactly an escape (`\b`, `\t`,
//!   `\n` and the other letter escapes, `\xNN`, `\uNNNN`) followed by `/`
//!   or `\`, and any `\n`, `\r` or `\t` in a Unix path: the path ends there
//!   and what follows stays readable (`/home/alice\t/secret` and
//!   `/mnt/c/proj\b/secret.txt` keep `alice` and `proj` as file names;
//!   `/mnt/c/Users\t\alice\secret.txt` leaves `\alice\secret.txt`;
//!   `/home/alice\\\\secret` leaves `\\\\secret`; `/mnt/c/x\notes\a` leaves
//!   `\notes\a`).
//! - JSON-escaped `//` (`\/\/srv\/share\/secret`,
//!   `file:\/\/\/home\/alice\/secret`): not found, wholly readable.
//! - The octal escape `\012` is no boundary: `line\012/home/alice/secret`
//!   is not found, wholly readable.
//! - A doubly escaped UNC path (`\\\\\\\\srv\\\\share`) and a raw
//!   `\\?\UNC\srv\share\secret`: not found, wholly readable.
//! - A UNC path's output repeats its `<network:hash>` prefix
//!   (`<network:H><network:H>/x`); the hash, never the server, is readable.
//! - A `Debug` drive path runs on through a written escape:
//!   `"C:\\x\\y\nnext"` becomes `"<external>/nnext"`, the text after
//!   the escape swallowed into the file name.
//! - A `/` right after a `-` is no token start, since `-` continues a word
//!   (`a-b/c`): a path after a `-` that opens a token or ends a word is not
//!   found, wholly readable (`-/home/alice/old.rs`,
//!   `--root=-/home/alice/x`, `1-/home/alice/x`).
//! - A UNC path whose server name holds any character but a letter, a
//!   digit, `_`, `.` or `-` is not found in text, wholly readable
//!   (`\\wsl$\Ubuntu\home\alice\x`, the WSL share); under a path key the
//!   same value is redacted as any UNC path is, the server hashed and the
//!   directories after the share kept
//!   (`<network:H><network:H>/home/alice/x`).

/// A detected path in a string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedPath {
    /// Start position in the original string.
    pub start: usize,
    /// End position in the original string.
    pub end: usize,
    /// The detected path string.
    pub path: String,
    /// Type of path detected.
    pub kind: DetectedPathKind,
}

/// Type of detected path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectedPathKind {
    /// `file:` URI.
    FileUri,
    /// Unix absolute path.
    UnixPath,
    /// Windows absolute path with drive letter.
    WindowsPath,
    /// UNC network path.
    UncPath,
}

/// Whether `c` ends a path in text: whitespace, a quote, a backtick, an
/// angle bracket or a closing bracket.
fn ends_path(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '<' | '>' | ']' | ')' | '}')
}

/// Whether `c` may open a segment of an absolute path, right after its
/// leading `/`: a name character, or one of `_ . - ~ $ @ + %`.
fn opens_segment(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '.' | '-' | '~' | '$' | '@' | '+' | '%')
}

/// Whether `c` continues a word or a relative path: a name character or
/// one of `_ . - ~ / \`. A `/` after one does not start an absolute path
/// (`src/lib.rs`, `./x`, `~/x`, `a-b/c`).
fn continues_word(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '.' | '-' | '~' | '/' | '\\')
}

/// Whether `before` ends with a redaction placeholder or another
/// `<name>` (`<workspace>`, `<external>`): a `<`, then one or more name
/// characters, `_` or `-`, then `>`.
fn ends_with_placeholder(before: &str) -> bool {
    let Some(inner) = before.strip_suffix('>') else {
        return false;
    };
    let Some(open) = inner.rfind('<') else {
        return false;
    };
    let name = &inner[open + 1..];
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-'))
}

/// The HTTP methods sqry's language plugins write into an endpoint's
/// qualified name, the set `parse_endpoint_qualified_name` in
/// `sqry-core`'s cross-language pass reads back.
const ROUTE_METHODS: [&str; 8] = [
    "GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS", "ALL",
];

/// Whether `before` ends with `route::<METHOD>::`, the qualified name
/// sqry's plugins give an HTTP endpoint (`route::GET::/api/users`):
/// `route` opening a token (not after a word character or `:`), then one
/// of [`ROUTE_METHODS`] exactly. The `/` after it opens a URL path.
/// `Foo::BAR::/x` and `app::route::GET::/x` are not that shape.
fn ends_with_route_method(before: &str) -> bool {
    ROUTE_METHODS.iter().any(|method| {
        before
            .strip_suffix("::")
            .and_then(|inner| inner.strip_suffix(method))
            .and_then(|inner| inner.strip_suffix("route::"))
            .is_some_and(|ahead| {
                ahead
                    .chars()
                    .next_back()
                    .is_none_or(|c| !(c.is_alphanumeric() || matches!(c, '_' | ':')))
            })
    })
}

/// Whether `before` ends with an escape sequence written out in text, as
/// Rust's `Debug`, JSON and C print a control character: a backslash that
/// no other backslash escapes, then one of
///
/// - `n`, `t`, `r`, `0`, `f`, `v`, `b`, `a` or `e` (`\n`, `\0`, `\f`, ...);
/// - `x` and two hex digits (`\x0a`, as `Debug` prints a byte);
/// - `u` and four hex digits (`\u000a`, as JSON prints a control
///   character).
///
/// (`\u{a}`, as `Debug` prints a character, ends with `}`, which is no word
/// character, so a `/` after it starts a path without this check.) A `/`
/// after one starts a new piece of text, so a token.
fn ends_with_written_escape(before: &str) -> bool {
    let bytes = before.as_bytes();
    let len = bytes.len();
    // The backslash at byte `at` is unescaped when an even run of
    // backslashes precedes it.
    let unescaped = |at: usize| {
        before[..at]
            .bytes()
            .rev()
            .take_while(|&b| b == b'\\')
            .count()
            .is_multiple_of(2)
    };
    let single = len >= 2
        && bytes[len - 2] == b'\\'
        && matches!(
            bytes[len - 1],
            b'n' | b't' | b'r' | b'0' | b'f' | b'v' | b'b' | b'a' | b'e'
        )
        && unescaped(len - 2);
    let byte = len >= 4
        && bytes[len - 4] == b'\\'
        && bytes[len - 3] == b'x'
        && bytes[len - 2..].iter().all(u8::is_ascii_hexdigit)
        && unescaped(len - 4);
    let unicode = len >= 6
        && bytes[len - 6] == b'\\'
        && bytes[len - 5] == b'u'
        && bytes[len - 4..].iter().all(u8::is_ascii_hexdigit)
        && unescaped(len - 6);
    single || byte || unicode
}

/// The length of the escape sequence written out in text that opens
/// `rest` (which starts with a backslash), as Rust's `Debug`, JSON and C
/// print a control character: a backslash and one of `n`, `t`, `r`, `0`,
/// `f`, `v`, `b`, `a`, `e` (2); `\x` and two hex digits (4); `\u` and four
/// hex digits (6); or `\u{`, one to six hex digits and `}`. `None` for any
/// other backslash (`\q`, `\xyz`, `\u00a`, `\\`).
fn written_escape_len(rest: &str) -> Option<usize> {
    let bytes = rest.as_bytes();
    let hex = |from: usize, len: usize| {
        bytes.len() >= from + len && bytes[from..from + len].iter().all(u8::is_ascii_hexdigit)
    };
    match bytes.get(1)? {
        b'n' | b't' | b'r' | b'0' | b'f' | b'v' | b'b' | b'a' | b'e' => Some(2),
        b'x' => hex(2, 2).then_some(4),
        b'u' if bytes.get(2) == Some(&b'{') => {
            let digits = bytes[3..]
                .iter()
                .take_while(|b| b.is_ascii_hexdigit())
                .count();
            ((1..=6).contains(&digits) && bytes.get(3 + digits) == Some(&b'}'))
                .then_some(4 + digits)
        }
        b'u' => hex(2, 4).then_some(6),
        _ => None,
    }
}

/// Whether `c` can continue a path in text: anything that does not end one
/// ([`ends_path`]) and is not a backslash (which begins an escape or, in a
/// Unix path, another backslash run).
fn is_path_char(c: char) -> bool {
    !(ends_path(c) || c == '\\')
}

/// Whether the unescaped backslash that opens `rest` begins an escape a
/// path in text must end before ([`path_end`] never asks about a backslash
/// that another backslash escapes, the second of a `\\` pair, so a pair is
/// never split). For every path: `\"` and `\'` (a quote `Debug` escapes,
/// which no path holds). For a UNC path as `Debug` writes it
/// (`escaped`, every separator a `\\` pair), also any written escape
/// ([`written_escape_len`]), since a lone backslash there can only begin
/// one (`\\\\srv\\share\\x\nnext` ends before `\n`). For a Unix path, which
/// uses no backslash of its own, also: `\n`, `\r` and `\t` whatever
/// follows (a line break or a tab, as whitespace ends a path:
/// `/x/secret.rs\nhint`); `\\` that no path character follows; and any
/// other written escape that the end of the text, a character that cannot
/// continue a path, or a `/` follows (`/x\u0041"`, `/x\b/y`, whose `/y`
/// then starts a path of its own). Any other backslash stays in the path
/// and is redacted with it: `/mnt/c/Users\alice\secret.txt`,
/// `/home/u/s\backup/more`, `/home/u/a\xyz/secret`, `/home/alice\\t/x`. A
/// Windows or raw UNC path keeps its own backslashes (`C:\new\x`).
fn backslash_ends_path(rest: &str, kind: DetectedPathKind, escaped: bool) -> bool {
    match rest[1..].chars().next() {
        Some('"' | '\'') => true,
        _ if escaped => written_escape_len(rest).is_some(),
        _ if kind != DetectedPathKind::UnixPath => false,
        Some('n' | 'r' | 't') => true,
        Some('\\') => rest[2..].chars().next().is_none_or(|c| !is_path_char(c)),
        _ => written_escape_len(rest).is_some_and(|len| {
            rest[len..]
                .chars()
                .next()
                .is_none_or(|c| c == '/' || !is_path_char(c))
        }),
    }
}

/// Whether the backslash at byte `at` of `input` is itself unescaped: an
/// even run of backslashes (none included) precedes it.
fn backslash_is_unescaped(input: &str, at: usize) -> bool {
    input[..at]
        .bytes()
        .rev()
        .take_while(|&b| b == b'\\')
        .count()
        .is_multiple_of(2)
}

/// Whether `rest` opens a UNC path as `Debug` writes it inside a string,
/// every backslash doubled: `\\\\`, a server name, `\\`, then a character
/// that can continue a path (`"\\\\srv\\share\\x"` for `\\srv\share\x`).
fn opens_escaped_unc(rest: &str) -> bool {
    let Some(after) = rest.strip_prefix("\\\\\\\\") else {
        return false;
    };
    let server_len = after
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')))
        .unwrap_or(after.len());
    server_len > 0
        && after[server_len..]
            .strip_prefix("\\\\")
            .and_then(|share| share.chars().next())
            .is_some_and(is_path_char)
}

/// The path text `redact_path` reads for `detected`: a JSON-escaped Unix
/// path (`\/home\/u\/s`) with its `\/` read as `/`, and a `Debug`-escaped
/// UNC path (`\\\\srv\\share`) with each doubled backslash read as one;
/// any other path as found.
fn unescaped_path(detected: &DetectedPath) -> std::borrow::Cow<'_, str> {
    let path = detected.path.as_str();
    match detected.kind {
        DetectedPathKind::UnixPath if path.starts_with("\\/") => {
            std::borrow::Cow::Owned(path.replace("\\/", "/"))
        }
        DetectedPathKind::UncPath if path.starts_with("\\\\\\\\") => {
            std::borrow::Cow::Owned(path.replace("\\\\", "\\"))
        }
        _ => std::borrow::Cow::Borrowed(path),
    }
}

/// Whether the text `after` a closed `~=` regex literal (past its flags)
/// lets the literal end there: the end of the text, whitespace, a quote, a
/// backtick, or one of `)`, `]`, `}`, `<`, `>`, `,`, `;`, `:`, `!`, `?`, each
/// of which either ends a path or makes a `/` after it start one, so
/// nothing after the literal reads as part of it; or a `.` followed by the end, whitespace, a quote, a backtick or a
/// closing bracket (a sentence's full stop). Anything else, `./x`, `../x`,
/// `.x` or a word character, could continue into a path the skip would
/// hide, so the literal is not skipped.
fn literal_ends_here(after: &str) -> bool {
    let mut chars = after.chars();
    match chars.next() {
        Some('.') => chars
            .next()
            .is_none_or(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '`' | ')' | ']' | '}')),
        next => next.is_none_or(|c| {
            c.is_whitespace()
                || matches!(
                    c,
                    '"' | '\'' | '`' | ')' | ']' | '}' | '<' | '>' | ',' | ';' | ':' | '!' | '?'
                )
        }),
    }
}

/// Whether the `/` that opens `rest` starts an absolute path, judged by
/// `before`, the text ahead of it. It does, at the start of the text and
/// after whitespace, punctuation, an operator or a shell redirection
/// (`=`, `:`, `,`, `;`, `(`, `|`, `&`, `+`, `%`, `@`, `$`, `!`, `?`, `^`,
/// `>`, `<`, a quote, ...). It does not:
///
/// - after a word or a relative path ([`continues_word`]), except a
///   one-letter flag at the start of a token (`-I/usr/include`), an
///   escape sequence written out in text (`line\n/home/u/x`), an
///   ellipsis (`see.../x`) and a `~` that follows a word (`x~/x`; a `~`
///   opening a token is the home directory, `~/x`, kept as relative);
/// - after a `<name>` placeholder (`<workspace>/x`), or as the `</name>`
///   of a closing markup tag;
/// - after `route::<METHOD>::` with one of sqry's HTTP methods (an
///   endpoint's URL path, [`ends_with_route_method`]);
/// - after `*` that follows a word or another `*` (a glob, `**/x`);
/// - after `++` (`include/c++/v1`);
/// - after `#` that follows a word, a `/` or a quote (a URI fragment,
///   `schema.json#/defs/x`, `https://h/app/#/route`, and a JSON pointer
///   written as a string in code, `"$ref": "#/definitions/X"`);
/// - as the closing `/` of a regular expression ending in `$`, followed
///   only by its flags (`/^a+$/i`).
fn slash_starts_path(before: &str, rest: &str) -> bool {
    let mut back = before.chars().rev();
    let Some(prev) = back.next() else {
        return true;
    };
    let prev2 = back.next();
    let prev3 = back.next();
    match prev {
        c if c.is_whitespace() => true,
        c if continues_word(c) => {
            // `-I/usr/include`, `-L/x`: a one-letter flag opening a token.
            let flag = c.is_ascii_alphabetic()
                && prev2 == Some('-')
                && prev3.is_none_or(|c| !(continues_word(c) || c == '-'));
            // `see.../x`: an ellipsis, not `./x` or `../x`.
            let ellipsis = c == '.' && prev2 == Some('.') && prev3 == Some('.');
            // `x~/x`: a `~` that follows a word is no home directory.
            let tilde_after_word =
                c == '~' && prev2.is_some_and(|c| c.is_alphanumeric() || c == '_');
            flag || ellipsis || tilde_after_word || ends_with_written_escape(before)
        }
        ':' => prev2 != Some(':') || !ends_with_route_method(before),
        '>' => !ends_with_placeholder(before),
        '<' => !closes_markup_tag(rest),
        '*' => prev2.is_none_or(|c| !(continues_word(c) || c == '*')),
        '+' => prev2 != Some('+'),
        '#' => prev2.is_none_or(|c| {
            !(c.is_alphanumeric() || matches!(c, '_' | '.' | '-' | '/' | '"' | '\''))
        }),
        '$' => !closes_regex_with_flags(rest),
        _ => true,
    }
}

/// Whether `rest`, which starts with `/`, is the `/name>` of a closing
/// markup tag (`</div>`).
fn closes_markup_tag(rest: &str) -> bool {
    let name = &rest[1..];
    let len = name
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .unwrap_or(name.len());
    len > 0 && name.as_bytes()[0].is_ascii_alphabetic() && name[len..].starts_with('>')
}

/// Whether `rest`, which starts with `/`, is only a closing slash and
/// regular-expression flags (sqry's `i`, `m`, `s`) up to the end of its
/// token (`/i`, `/ms`).
fn closes_regex_with_flags(rest: &str) -> bool {
    let flags_len = rest[1..]
        .find(|c: char| !matches!(c, 'i' | 'm' | 's'))
        .unwrap_or(rest.len() - 1);
    flags_len > 0 && rest[1 + flags_len..].chars().next().is_none_or(ends_path)
}

/// The length of the sqry regular-expression literal that opens `rest`
/// when `before` ends with the regex operator `~=` (spaces and tabs
/// allowed between): the `/`, the pattern up to the next `/` that no odd
/// run of backslashes escapes, and the flags `i`, `m`, `s`, as sqry's
/// query lexer reads it, when the literal is plainly one within its token.
///
/// It fails safe: `None` (so the text is scanned as any other, and the `/`
/// may read as a path) when `before` does not end with `~=`, when the
/// pattern reaches whitespace or the end of the text before its closing
/// `/` (an unterminated literal, or one holding whitespace: sqry's lexer
/// reads on across whitespace, but in a message what follows is prose that
/// may name a path), or when what follows the closing `/` and flags could
/// continue into a path ([`literal_ends_here`]: an ambiguous literal such
/// as `/a/b/`, or `/x/./home/u/s`). The skip therefore covers only the
/// literal itself, and every path after it is scanned as in any text.
fn regex_literal_len(before: &str, rest: &str) -> Option<usize> {
    if !before.trim_end_matches([' ', '\t']).ends_with("~=") {
        return None;
    }
    let mut backslashes = 0usize;
    for (offset, c) in rest.char_indices().skip(1) {
        match c {
            c if c.is_whitespace() => return None,
            '\\' => backslashes += 1,
            '/' if backslashes.is_multiple_of(2) => {
                let after = offset + 1;
                let flags = rest[after..]
                    .find(|c: char| !matches!(c, 'i' | 'm' | 's'))
                    .unwrap_or(rest.len() - after);
                let end = after + flags;
                return literal_ends_here(&rest[end..]).then_some(end);
            }
            _ => backslashes = 0,
        }
    }
    None
}

/// Whether a URL scheme that follows `prev` starts a token: not after a
/// name character or `+ - . _` (which continue a scheme, `git+ssh`).
fn word_starts(prev: Option<char>) -> bool {
    prev.is_none_or(|c| !(c.is_alphanumeric() || matches!(c, '+' | '-' | '.' | '_')))
}

/// Whether a drive letter or `file:` that follows `prev` starts a path:
/// not after a name character or `- . _` (`x+C:\x` is a path after an
/// operator).
fn drive_starts(prev: Option<char>) -> bool {
    prev.is_none_or(|c| !(c.is_alphanumeric() || matches!(c, '-' | '.' | '_')))
}

/// The end of the token that starts at `from`: the first character that
/// ends a path, or the end of the input.
fn token_end(input: &str, from: usize) -> usize {
    input[from..]
        .char_indices()
        .find(|&(_, c)| ends_path(c))
        .map_or(input.len(), |(offset, _)| from + offset)
}

/// The end of the path that starts at `from`: as [`token_end`], except
/// that a bracket the path itself opened is part of it, so
/// `/home/u/My(1)/sub/file` and `/app/[id]/page.tsx` are one path, while a
/// closing bracket the path did not open (`(/x)`) ends it, and an
/// unescaped backslash that begins an escape ends it
/// ([`backslash_ends_path`]); the second backslash of a `\\` pair is part
/// of the pair, never the start of an escape.
fn path_end(input: &str, from: usize, kind: DetectedPathKind) -> usize {
    let escaped = kind == DetectedPathKind::UncPath && input[from..].starts_with("\\\\\\\\");
    let mut depth = [0usize; 3];
    for (offset, c) in input[from..].char_indices() {
        let at = from + offset;
        if c == '\\'
            && backslash_is_unescaped(input, at)
            && backslash_ends_path(&input[at..], kind, escaped)
        {
            return at;
        }
        let slot = match c {
            '(' | ')' => 0,
            '[' | ']' => 1,
            '{' | '}' => 2,
            _ => {
                if ends_path(c) {
                    return from + offset;
                }
                continue;
            }
        };
        if matches!(c, '(' | '[' | '{') {
            depth[slot] += 1;
        } else if depth[slot] > 0 {
            depth[slot] -= 1;
        } else {
            return from + offset;
        }
    }
    input.len()
}

/// The end of the scheme, authority and path of the URL that starts at
/// `from`: the first `?`, `;` or `#`, or the end of its token. A URL's
/// query, parameters and fragment are scanned as other text is, so a host
/// path in a query value (`?f=/home/u/x`) is found.
fn url_path_end(input: &str, from: usize) -> usize {
    let end = token_end(input, from);
    input[from..end]
        .find(['?', ';', '#'])
        .map_or(end, |offset| from + offset)
}

/// The length of a URL scheme followed by `://` at the start of `rest`, for
/// a scheme of two or more characters (RFC 3986: a letter, then letters,
/// digits, `+`, `-` or `.`), or `None`. A scheme that ends in `+file`
/// (`git+file://`, `x+file://`) is no other scheme: its `file:` URI names a
/// local path, so it is `None` and the scan finds the `file:` URI.
fn url_scheme_len(rest: &str) -> Option<usize> {
    let bytes = rest.as_bytes();
    if !bytes.first()?.is_ascii_alphabetic() {
        return None;
    }
    let scheme_len = bytes
        .iter()
        .position(|&b| !(b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.')))?;
    let scheme = &rest[..scheme_len];
    let names_a_local_file =
        scheme.len() > 5 && scheme[scheme.len() - 5..].eq_ignore_ascii_case("+file");
    (scheme_len >= 2 && !names_a_local_file && rest[scheme_len..].starts_with("://"))
        .then_some(scheme_len)
}

/// Whether `rest` opens a `file:` URI: `file:` in any letter case, then `/`.
fn opens_file_uri(rest: &str) -> bool {
    rest.get(..5)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file:"))
        && rest[5..].starts_with('/')
}

/// Whether `rest` opens a UNC path: `\\`, a server name, `\`, then a
/// character that does not end a path.
fn opens_unc(rest: &str) -> bool {
    let Some(after) = rest.strip_prefix("\\\\") else {
        return false;
    };
    let server_len = after
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')))
        .unwrap_or(after.len());
    server_len > 0
        && after[server_len..]
            .strip_prefix('\\')
            .and_then(|share| share.chars().next())
            .is_some_and(|c| !ends_path(c))
}

/// Whether `rest` opens a Windows drive path: a drive letter, `:`, `\` or
/// `/`, then a character that does not end a path.
fn opens_drive_path(rest: &str) -> bool {
    let bytes = rest.as_bytes();
    bytes.len() >= 4
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
        && rest[3..].chars().next().is_some_and(|c| !ends_path(c))
}

/// Whether `rest`, which starts with `/`, opens a Unix absolute path: one
/// or two slashes, then a character that opens a segment.
fn opens_unix_path(rest: &str) -> bool {
    let after = rest.strip_prefix("//").unwrap_or(&rest[1..]);
    after.chars().next().is_some_and(opens_segment)
}

/// Detect paths embedded in a string value.
///
/// Scans the input for absolute paths (see the module documentation for
/// the forms found and the forms left as they are) and returns them in
/// order, never overlapping.
///
/// # Example
///
/// ```rust
/// use sqry_mcp_redaction::rules::detect_paths_in_string;
///
/// let text = "Error in /home/user/project/src/main.rs at line 42";
/// let paths = detect_paths_in_string(text);
/// assert_eq!(paths.len(), 1);
/// assert!(paths[0].path.contains("main.rs"));
///
/// // Any prefix, not a fixed list of directories.
/// let paths = detect_paths_in_string("rebuild of /dev/shm/x refused");
/// assert_eq!(paths[0].path, "/dev/shm/x");
///
/// // A URL of another scheme is not a path.
/// assert!(detect_paths_in_string("see https://host/home/u/x").is_empty());
/// ```
#[must_use]
pub fn detect_paths_in_string(input: &str) -> Vec<DetectedPath> {
    let mut paths = Vec::new();
    let mut prev: Option<char> = None;
    let mut i = 0;
    while i < input.len() {
        let rest = &input[i..];
        let Some(c) = rest.chars().next() else {
            break;
        };
        let kind = if drive_starts(prev) && opens_file_uri(rest) {
            Some(DetectedPathKind::FileUri)
        } else if word_starts(prev) && url_scheme_len(rest).is_some() {
            // A URL of another scheme: its scheme, authority and path are
            // no path on this host and are skipped; its query, parameters
            // and fragment are scanned on.
            let end = url_path_end(input, i);
            prev = input[..end].chars().next_back();
            i = end;
            continue;
        } else if c == '/'
            && let Some(len) = regex_literal_len(&input[..i], rest)
        {
            // A sqry regular-expression literal (`name~=/.*/i`): never a
            // path.
            let end = i + len;
            prev = input[..end].chars().next_back();
            i = end;
            continue;
        } else if prev != Some('\\') && opens_escaped_unc(rest) {
            // `"\\\\srv\\share"`, a UNC path as `Debug` writes it.
            Some(DetectedPathKind::UncPath)
        } else if prev != Some('\\') && opens_unc(rest) {
            Some(DetectedPathKind::UncPath)
        } else if c == '\\'
            && rest.starts_with("\\/")
            && backslash_is_unescaped(input, i)
            && slash_starts_path(&input[..i], &rest[1..])
            && opens_unix_path(&rest[1..])
        {
            // `\/home\/u\/s`, a Unix path with JSON-escaped slashes.
            Some(DetectedPathKind::UnixPath)
        } else if c == '\\'
            && backslash_is_unescaped(input, i)
            && let Some(len) = written_escape_len(rest)
        {
            // An escape written out in text (`\n`, `\u000a`, ...) is a
            // boundary: whatever follows starts a token, so a drive path,
            // a `file:` URI, a UNC path or a URL after it is found as after
            // whitespace (`line\nC:\x`).
            i += len;
            prev = Some('\n');
            continue;
        } else if drive_starts(prev) && opens_drive_path(rest) {
            Some(DetectedPathKind::WindowsPath)
        } else if c == '/' && slash_starts_path(&input[..i], rest) && opens_unix_path(rest) {
            Some(DetectedPathKind::UnixPath)
        } else {
            None
        };
        if let Some(kind) = kind {
            let end = path_end(input, i, kind);
            paths.push(DetectedPath {
                start: i,
                end,
                path: input[i..end].to_string(),
                kind,
            });
            prev = input[..end].chars().next_back();
            i = end;
            continue;
        }
        prev = Some(c);
        i += c.len_utf8();
    }
    paths
}

/// Redact paths in a string, replacing them with the provided replacement.
///
/// # Arguments
///
/// * `input` - The input string
/// * `workspace_root` - Optional workspace root for relative conversion
/// * `workspace_placeholder` - Placeholder for workspace paths
/// * `hash_filenames` - Whether to hash filenames
/// * `hash_salt` - Optional salt for hashing
///
/// # Returns
///
/// Tuple of (redacted string, number of paths redacted).
pub fn redact_paths_in_string(
    input: &str,
    workspace_root: Option<&str>,
    workspace_placeholder: &str,
    hash_filenames: bool,
    hash_salt: Option<&str>,
) -> (String, usize) {
    let paths = detect_paths_in_string(input);

    if paths.is_empty() {
        return (input.to_string(), 0);
    }

    let mut result = String::with_capacity(input.len());
    let mut last_end = 0;

    for detected in &paths {
        // Add text before this path
        result.push_str(&input[last_end..detected.start]);

        // Redact the path
        match super::path::redact_path(
            &unescaped_path(detected),
            workspace_root,
            workspace_placeholder,
            hash_filenames,
            hash_salt,
        ) {
            Ok(redacted) => result.push_str(&redacted),
            Err(_) => {
                // On error, use a generic placeholder
                result.push_str(workspace_placeholder);
            }
        }

        last_end = detected.end;
    }

    // Add remaining text
    result.push_str(&input[last_end..]);

    (result, paths.len())
}

/// Check if a string contains any path the detector finds.
#[must_use]
pub fn contains_path_pattern(input: &str) -> bool {
    !detect_paths_in_string(input).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_file_uri() {
        let paths = detect_paths_in_string("See file:///home/user/file.rs");
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].kind, DetectedPathKind::FileUri);
        assert_eq!(paths[0].path, "file:///home/user/file.rs");
    }

    #[test]
    fn test_detect_unix_path() {
        let paths = detect_paths_in_string("Error in /home/user/project/src/main.rs");
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].kind, DetectedPathKind::UnixPath);
        assert!(paths[0].path.contains("main.rs"));
    }

    #[test]
    fn test_detect_windows_path() {
        let paths = detect_paths_in_string("File at C:\\Users\\john\\project\\file.rs");
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].kind, DetectedPathKind::WindowsPath);
        assert!(paths[0].path.contains("file.rs"));
    }

    #[test]
    fn test_detect_unc_path() {
        let paths = detect_paths_in_string("Network file \\\\server\\share\\dir\\file.txt");
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].kind, DetectedPathKind::UncPath);
        assert!(paths[0].path.contains("share"));
    }

    #[test]
    fn test_detect_multiple_paths() {
        let text = "Files: /home/user/a.rs and /var/log/b.log";
        let paths = detect_paths_in_string(text);
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn test_no_paths() {
        let paths = detect_paths_in_string("This is just regular text.");
        assert!(paths.is_empty());
    }

    #[test]
    fn test_file_uri_subsumes_unix_path() {
        // The Unix path inside the URI shouldn't be double-detected
        let paths = detect_paths_in_string("file:///home/user/file.rs");
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].kind, DetectedPathKind::FileUri);
    }

    #[test]
    fn test_redact_paths_in_string() {
        let input = "Error in /home/user/project/src/main.rs at line 42";
        let (redacted, count) = redact_paths_in_string(
            input,
            Some("/home/user/project"),
            "<workspace>",
            false,
            None,
        );
        assert_eq!(count, 1);
        assert!(redacted.contains("src/main.rs"));
        assert!(!redacted.contains("/home/user/project"));
    }

    #[test]
    fn test_redact_preserves_surrounding_text() {
        let input = "Before /home/user/project/file.rs After";
        let (redacted, count) = redact_paths_in_string(
            input,
            Some("/home/user/project"),
            "<workspace>",
            false,
            None,
        );
        assert_eq!(count, 1);
        assert!(redacted.starts_with("Before "));
        assert!(redacted.ends_with(" After"));
    }

    #[test]
    fn test_contains_path_pattern() {
        assert!(contains_path_pattern("file:///path"));
        assert!(contains_path_pattern("/home/user/file"));
        assert!(contains_path_pattern("C:\\Users\\file"));
        assert!(contains_path_pattern("\\\\server\\share"));
        assert!(!contains_path_pattern("just text"));
    }

    #[test]
    fn test_path_in_quoted_string() {
        let paths = detect_paths_in_string(r#"Error: "/home/user/file.rs" not found"#);
        assert_eq!(paths.len(), 1);
        // Should not include the quotes
        assert!(!paths[0].path.contains('"'));
    }

    #[test]
    fn test_path_in_json() {
        let paths = detect_paths_in_string(r#"{"path": "/home/user/file.rs"}"#);
        assert_eq!(paths.len(), 1);
    }

    /// Absolute paths outside every directory the old pattern listed
    /// (`/home`, `/Users`, `/var`, `/srv`, `/opt`, `/tmp`, `/etc`), and the
    /// Windows forms, in each place a message puts a path. Under the old
    /// pattern none of the Unix ones and neither `c:/` form was found.
    const ABSOLUTE: &[&str] = &[
        "/dev/shm/r8/ws/src/lib.rs",
        "/mnt/data/x",
        "/nix/store/abc123-pkg/lib/libx.so",
        "/root/.ssh/id_ed25519",
        "/data/x",
        "/proc/1/cwd",
        "/x",
        "/.hidden",
        "C:\\Users\\me\\proj\\a.rs",
        "c:/proj/a.rs",
        "D:\\",
        "\\\\server\\share\\x",
        "file:///dev/shm/x",
        "FILE:///mnt/x",
        "file:/nix/store/x",
        "file://host/share/x",
    ];

    /// The text around a path in the messages the servers write.
    const AROUND: &[(&str, &str)] = &[
        ("", ""),
        ("rebuild of ", " refused"),
        ("dir=", ""),
        ("--cache-dir=", " "),
        ("path:", ""),
        ("expected `", "`"),
        ("(", ")"),
        ("[", "]"),
        ("{", "}"),
        ("'", "'"),
        ("\"", "\""),
        ("a, ", ""),
        ("a; ", ""),
        ("a|", ""),
        ("x\n", "\ny"),
        // A shell redirection or an operator character before the path.
        ("2>", ""),
        ("cmd >", ""),
        ("cat <", ""),
        ("a&", ""),
        ("x+", ""),
        ("%", ""),
        ("#", ""),
        ("$", ""),
        ("*", ""),
        ("@", ""),
        ("!", ""),
        ("?", ""),
        ("^", ""),
        ("path::", ""),
        // A query value or parameter of a URL of another scheme.
        ("ws://h/a?f=", ""),
        ("http://h/api;cache=", ""),
        ("https://h/a?x=1&p=", ""),
    ];

    #[test]
    fn every_absolute_path_is_found_whatever_its_prefix() {
        let mut checked = 0;
        for path in ABSOLUTE {
            for (before, after) in AROUND {
                let text = format!("{before}{path}{after}");
                let found = detect_paths_in_string(&text);
                let found_paths: Vec<&str> = found.iter().map(|p| p.path.as_str()).collect();
                if *path == "D:\\" {
                    // A drive root alone names nothing to redact.
                    assert!(found.is_empty(), "{text:?}: {found_paths:?}");
                    continue;
                }
                assert_eq!(found_paths, [*path], "{text:?}");
                assert_eq!(&text[found[0].start..found[0].end], *path, "{text:?}");
                checked += 1;
            }
        }
        assert_eq!(checked, (ABSOLUTE.len() - 1) * AROUND.len());
    }

    #[test]
    fn two_slashes_open_a_unc_path_and_a_url_is_not_one() {
        let found = detect_paths_in_string("share at //server/share/x now");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, "//server/share/x");
        assert_eq!(found[0].kind, DetectedPathKind::UnixPath);
    }

    /// The other direction: text that is not an absolute path, in the
    /// shapes sqry's responses carry, is left exactly as it is.
    #[test]
    fn what_is_not_an_absolute_path_is_left_as_it_is() {
        let texts = [
            "https://host/home/u/x",
            "http://localhost:8080/srv/data/x",
            "see https://docs.verivus.dev/sqry/query-cost-gate.",
            "git+ssh://host/org/repo.git",
            "sqry://docs/tools",
            "src/lib.rs",
            "./x",
            "../x/y",
            "~/x",
            "a/b",
            "and/or",
            "1/2",
            "github.com/org/pkg",
            "route::GET::/api/users",
            "schema.json#/definitions/Foo",
            "https://json-schema.org/draft#/definitions/x",
            "https://h/app/#/route",
            "https://h/api;v=1?q=x",
            "**/x",
            "src/**/x.rs",
            "a*/b",
            "$HOME/x",
            "include/c++/v1",
            "route::POST::/api/items",
            "</div>",
            "a <b>bold</b> c",
            // sqry's regular-expression literals after `~=`.
            "name~=/.*/",
            "predicate `name~=/.*/` is too broad",
            "name~=/foo/i",
            "name~= /test_.*/ms",
            "'name~=/handler/'",
            "name~=/a\\/b/",
            "name~=/x/.",
            "(name~=/x/m)",
            // Round 8c (N3): a JSON pointer written as a string in code.
            r##""$ref": "#/definitions/X""##,
            r##"{"$ref":"#/$defs/X"}"##,
            "$ref: '#/components/schemas/X'",
            // Every endpoint method sqry's plugins write.
            "route::PUT::/x",
            "route::DELETE::/x",
            "route::PATCH::/x",
            "route::HEAD::/x",
            "route::OPTIONS::/x",
            "route::ALL::/x",
            "`route::GET::/api/users`",
            "/^a+$/i",
            "/[a-z]+/",
            "@scope/pkg",
            "<workspace>/src/lib.rs",
            "<external>/x",
            "<excluded>/[0123abcd]",
            "// a comment",
            "/* a comment */",
            "/^a+$/",
            "a / b",
            "x /= 2",
            "/",
            "C:cache",
            "operator/",
            "crate::foo",
            "S::~S",
            "static_estimate",
            "",
        ];
        for text in texts {
            assert!(
                detect_paths_in_string(text).is_empty(),
                "{text:?}: {:?}",
                detect_paths_in_string(text)
            );
            let (redacted, count) =
                redact_paths_in_string(text, Some("/dev/shm/r8/ws"), "<workspace>", false, None);
            assert_eq!((redacted.as_str(), count), (text, 0), "{text:?}");
        }
    }

    /// A path after a shell redirection, an operator character, `::` or a
    /// one-letter flag, in a URL's query or parameters, or holding a
    /// bracket it opens, is found whole; a sqry regex literal before it does
    /// not hide it. Each was left (or split) before round 8b.
    #[test]
    fn a_path_after_an_operator_or_in_a_query_is_found_whole() {
        for (text, path) in [
            ("2>/dev/shm/secret/log", "/dev/shm/secret/log"),
            ("cmd >/srv/u/out.txt", "/srv/u/out.txt"),
            ("cat </home/u/in", "/home/u/in"),
            ("a&/home/u/x", "/home/u/x"),
            ("x+/home/u/y", "/home/u/y"),
            ("%/home/u/x", "/home/u/x"),
            ("#/home/u/x", "/home/u/x"),
            ("$/home/u/x", "/home/u/x"),
            ("*/home/u/x", "/home/u/x"),
            ("@/home/u/x", "/home/u/x"),
            ("path::/home/u/y", "/home/u/y"),
            ("http://h/api;cache=/home/u/.cache", "/home/u/.cache"),
            ("ws://h/a?f=/home/u/z", "/home/u/z"),
            ("/home/u/My(1)/sub/file", "/home/u/My(1)/sub/file"),
            ("/app/[id]/page.tsx", "/app/[id]/page.tsx"),
            ("(see /mnt/x)", "/mnt/x"),
            ("-I/usr/include", "/usr/include"),
            ("cc -L/opt/lib", "/opt/lib"),
            ("name~=/x/ in /mnt/y", "/mnt/y"),
            ("git+file:///mnt/repo", "file:///mnt/repo"),
            // Round 8c: a written-out escape, an ellipsis, a `~` after a
            // word, and a `::` that is not sqry's endpoint shape.
            ("Error(\"line\\n/home/u/secret\")", "/home/u/secret"),
            ("a\\t/home/u/x", "/home/u/x"),
            ("a\\r/home/u/x", "/home/u/x"),
            ("see.../home/u/x", "/home/u/x"),
            ("x~/home/u/x", "/home/u/x"),
            ("Foo::BAR::/home/u/secret", "/home/u/secret"),
            ("app::route::GET::/home/u/x", "/home/u/x"),
            ("route::FETCH::/home/u/x", "/home/u/x"),
            ("see #/home/u/x", "/home/u/x"),
            // An ambiguous literal after `~=` fails safe: read as a path.
            ("name~=/home/u/x/", "/home/u/x/"),
        ] {
            let found: Vec<String> = detect_paths_in_string(text)
                .into_iter()
                .map(|p| p.path)
                .collect();
            assert_eq!(found, [path], "{text:?}");
        }
    }

    /// Round 8c (N2): a regex literal after `~=` is skipped only when it
    /// closes within its token. An unterminated one, or one holding
    /// whitespace, fails safe: its `/` reads as a path and the text after it
    /// is scanned, so a later host path is found. Before, the skip ran on to
    /// the next `/` anywhere in the text and hid the path after it.
    #[test]
    fn a_regex_literal_never_hides_a_later_path() {
        for (text, found) in [
            (
                "regex name~=/foo failed while reading /home/werner/secret.txt",
                &["/foo", "/home/werner/secret.txt"][..],
            ),
            (
                "unterminated name~=/foo in /mnt/secret/a",
                &["/foo", "/mnt/secret/a"],
            ),
            (
                "name~=/x/ then name~=/y and /home/u/z",
                &["/y", "/home/u/z"],
            ),
            ("name~=/a b/ at /mnt/c", &["/a", "/mnt/c"]),
            ("name~=/x/i, /mnt/d", &["/mnt/d"]),
        ] {
            let paths: Vec<String> = detect_paths_in_string(text)
                .into_iter()
                .map(|p| p.path)
                .collect();
            assert_eq!(paths, found, "{text:?}");
        }
    }

    /// Round 8d: a `~=` literal is skipped only when what follows it cannot
    /// continue into a path. Each accepted follower, both ways: alone after
    /// the literal it keeps the literal, and with a path after it the path
    /// is found. A follower that could continue a path (`./`, `../`, a flag
    /// then `.`, a word character) fails safe: the literal reads as a path
    /// and nothing after it is hidden. Before, `name~=/x/./home/u/secret`
    /// found nothing.
    #[test]
    fn a_literal_ends_only_where_nothing_can_continue_it() {
        let followers = [
            "", " ", "\"", "'", "`", ")", "]", "}", "<", ">", ",", ";", ":", "!", "?", ".", ". ",
            ".\"", ".)",
        ];
        for follower in followers {
            let alone = format!("name~=/x/i{follower}");
            assert!(
                detect_paths_in_string(&alone).is_empty(),
                "{alone:?}: {:?}",
                detect_paths_in_string(&alone)
            );
            let text = format!("name~=/x/i{follower}/home/u/s");
            let (redacted, count) = redact_paths_in_string(&text, None, "<workspace>", false, None);
            assert!(
                count >= 1 && !redacted.contains("/home"),
                "{text:?}: {redacted}"
            );
        }
        for text in [
            "name~=/x/./home/u/secret",
            "name~=/x/i./home/u/s",
            "name~=/x/../home/u/s",
            "name~=/x/.home/u/s",
            "name~=/x/a/home/u/s",
            "name~=/x/-/home/u/s",
        ] {
            let paths = detect_paths_in_string(text);
            assert_eq!(paths.len(), 1, "{text:?}: {paths:?}");
            assert!(
                paths[0].path.starts_with("/x/") && paths[0].path.contains("home/u/s"),
                "{text:?}: {paths:?}"
            );
        }
    }

    /// Round 8d: every escape `Debug`, JSON or C writes out for a control
    /// character starts a token, and a path ends before a backslash that
    /// begins an escape. The other direction: an escaped backslash before a
    /// letter, a backslash before a letter that is no escape, and a short
    /// `\x` or `\u` are no escape, so the path after them reads as relative;
    /// a Windows or UNC path keeps its own backslashes.
    #[test]
    fn written_escapes_bound_a_path_on_both_sides() {
        for escape in [
            "\\n", "\\t", "\\r", "\\0", "\\f", "\\v", "\\b", "\\a", "\\e", "\\x0a", "\\x1F",
            "\\u000a", "\\u001B", "\\u{a}", "\\u{1b}",
        ] {
            let text = format!("line{escape}/home/u/s");
            let found: Vec<String> = detect_paths_in_string(&text)
                .into_iter()
                .map(|p| p.path)
                .collect();
            assert_eq!(found, ["/home/u/s"], "{text:?}");
        }
        for text in [
            "x\\\\n/home/u/s",
            "a\\q/home/u/s",
            "a\\x0/home/u/s",
            "a\\u00a/home/u/s",
        ] {
            assert!(
                detect_paths_in_string(text).is_empty(),
                "{text:?}: {:?}",
                detect_paths_in_string(text)
            );
        }
        for (text, path) in [
            ("Error(\\\"/home/u/s\\\")", "/home/u/s"),
            ("'/home/u/s\\'", "/home/u/s"),
            ("at /x\\n next", "/x"),
            ("at /x\\\\y", "/x\\\\y"),
            ("at /x\\\\ next", "/x"),
            ("at /x\\u0041", "/x"),
            ("at C:\\new\\x.rs", "C:\\new\\x.rs"),
            ("at C:\\Users\\me\\\" x", "C:\\Users\\me"),
            ("\\\\srv\\share\\a\\\"", "\\\\srv\\share\\a"),
        ] {
            let found: Vec<String> = detect_paths_in_string(text)
                .into_iter()
                .map(|p| p.path)
                .collect();
            assert_eq!(found, [path], "{text:?}");
        }
    }

    /// Round 8e: a backslash ends a Unix path only where a real escape
    /// begins (`\"`, `\'`, `\\` that no path character follows, or a
    /// written escape that the end, a non-path character or a `/`
    /// follows). Any other backslash stays in the path and is redacted with
    /// it. At round 8d any `\` before `a b e f n r t v 0 u x \` ended the
    /// path, and the rest (`\alice\secret.txt`) was left readable.
    #[test]
    fn a_backslash_ends_a_path_only_where_an_escape_begins() {
        for (text, found) in [
            (
                "open /mnt/c/Users\\alice\\secret.txt failed",
                &["/mnt/c/Users\\alice\\secret.txt"][..],
            ),
            ("/home/u/s\\backup/more/x", &["/home/u/s\\backup/more/x"]),
            ("/home/u/a\\xyz/secret", &["/home/u/a\\xyz/secret"]),
            ("/home/u/a\\u00g/x", &["/home/u/a\\u00g/x"]),
            // Round 8f: `\n`, `\r`, `\t` end a Unix path whatever follows.
            ("/home/u/a\\n2/x", &["/home/u/a"]),
            ("Error(\\\"/home/u/s\\\")", &["/home/u/s"]),
            ("at /x\\n next", &["/x"]),
            ("at /x\\u0041\"", &["/x"]),
            ("at /x\\u{41} y", &["/x"]),
            ("at /x\\x41 y", &["/x"]),
            ("at /x\\\\", &["/x"]),
            ("at /x\\n/y", &["/x", "/y"]),
        ] {
            let paths: Vec<String> = detect_paths_in_string(text)
                .into_iter()
                .map(|p| p.path)
                .collect();
            assert_eq!(paths, found, "{text:?}");
        }
        for (text, secret) in [
            ("open /mnt/c/Users\\alice\\secret.txt failed", "alice"),
            ("/home/u/s\\backup/more/x", "backup"),
            ("/home/u/a\\xyz/secret", "xyz"),
        ] {
            let (redacted, _) = redact_paths_in_string(text, None, "<workspace>", false, None);
            assert!(!redacted.contains(secret), "{text:?}: {redacted}");
        }
    }

    /// Round 8e: every path start honours a written escape as a boundary,
    /// not only a `/`: a drive path, a `file:` URI and a UNC path after
    /// `\n`, `\t`, `\x0a`, `\u000a` or `\u{a}`. A UNC path as `Debug`
    /// writes it (`\\\\srv\\share`) and a Unix path with JSON-escaped
    /// slashes (`\/home\/u\/s`) are found whole and redacted without
    /// leaving the server or a directory readable. The other direction: a
    /// URL after an escape keeps its own path, and after an escaped
    /// backslash and a letter (`x\\nC:\x`) or a backslash and `/` that
    /// another backslash escapes (`a\\/home`) no path starts.
    #[test]
    fn every_path_start_honours_written_escapes() {
        for (text, found) in [
            ("line\\nC:\\Users\\me\\secret", "C:\\Users\\me\\secret"),
            ("line\\u000aC:\\Users\\me\\secret", "C:\\Users\\me\\secret"),
            ("line\\x0aC:\\Users\\me\\secret", "C:\\Users\\me\\secret"),
            ("line\\u{a}C:\\Users\\me\\secret", "C:\\Users\\me\\secret"),
            ("line\\nfile:///home/u/secret", "file:///home/u/secret"),
            ("line\\tfile:///home/u/secret", "file:///home/u/secret"),
            ("line\\n\\\\srv\\share\\x", "\\\\srv\\share\\x"),
            (
                "\"\\\\\\\\srv\\\\share\\\\secret\"",
                "\\\\\\\\srv\\\\share\\\\secret",
            ),
            ("{\"p\":\"\\/home\\/u\\/s\"}", "\\/home\\/u\\/s"),
            ("line\\nhttps://h/a?f=/home/u/z", "/home/u/z"),
        ] {
            let paths: Vec<String> = detect_paths_in_string(text)
                .into_iter()
                .map(|p| p.path)
                .collect();
            assert_eq!(paths, [found], "{text:?}");
            let (redacted, _) = redact_paths_in_string(text, None, "<workspace>", false, None);
            // The file name stays (`<external>/secret`), as for any path
            // outside the workspace; every directory, user and server goes.
            for secret in ["srv", "share", "home", "Users", "\\me"] {
                assert!(!redacted.contains(secret), "{text:?}: {redacted}");
            }
        }
        for text in [
            "line\\nhttps://h/home/u/x",
            "x\\\\nC:\\Users",
            "a\\\\/home/u/x",
        ] {
            assert!(
                detect_paths_in_string(text).is_empty(),
                "{text:?}: {:?}",
                detect_paths_in_string(text)
            );
        }
    }

    /// Round 8f, regression (a) of round 8e: the second backslash of a `\\`
    /// pair is never read as the start of an escape, so `Debug`'s
    /// `/home/alice\\t/secret` (a literal backslash and `t`) stays one path
    /// and the output keeps no half of the pair. At round 8e the path ended
    /// at the second backslash and the output read `<external>/alice\t/secret`.
    #[test]
    fn an_escaped_backslash_pair_is_never_split() {
        for (raw, found) in [
            ("/home/alice\\t/secret", "/home/alice\\\\t/secret"),
            ("/mnt/c/proj\\b/secret.txt", "/mnt/c/proj\\\\b/secret.txt"),
        ] {
            let text = format!("{raw:?}");
            let paths: Vec<String> = detect_paths_in_string(&text)
                .into_iter()
                .map(|p| p.path)
                .collect();
            assert_eq!(paths, [found], "{text}");
            let (redacted, _) = redact_paths_in_string(&text, None, "<workspace>", false, None);
            let name = raw.rsplit('/').next().expect("a file name");
            assert_eq!(redacted, format!("\"<external>/{name}\""), "{text}");
        }
    }

    /// Round 8f, regression (b) of round 8e: a written `\n`, `\r` or `\t`
    /// ends a Unix path whatever follows, and any written escape ends a UNC
    /// path as `Debug` writes it. At round 8e `secret.rs\nhint:` ran on, so
    /// the file name was replaced by `nhint:` and `hint:` was swallowed;
    /// the `Debug` UNC path likewise read `<network:..>/x/nnext`.
    #[test]
    fn a_line_break_escape_ends_a_unix_or_debug_unc_path() {
        let text = format!(
            "{:?}",
            "failed: \"/home/alice/proj/secret.rs\nhint: try again\""
        );
        let paths: Vec<String> = detect_paths_in_string(&text)
            .into_iter()
            .map(|p| p.path)
            .collect();
        assert_eq!(paths, ["/home/alice/proj/secret.rs"], "{text}");
        let (redacted, _) = redact_paths_in_string(&text, None, "<workspace>", false, None);
        assert!(
            redacted.contains("<external>/secret.rs\\nhint: try again"),
            "{redacted}"
        );
        for (raw, rest) in [("/x/a.rs\rtail", "\\rtail"), ("/x/a.rs\ttail", "\\ttail")] {
            let text = format!("{raw:?}");
            let (redacted, _) = redact_paths_in_string(&text, None, "<workspace>", false, None);
            assert_eq!(redacted, format!("\"<external>/a.rs{rest}\""), "{text}");
        }
        let text = format!("{:?}", "\\\\srv\\share\\x\nnext");
        let paths: Vec<String> = detect_paths_in_string(&text)
            .into_iter()
            .map(|p| p.path)
            .collect();
        assert_eq!(paths, ["\\\\\\\\srv\\\\share\\\\x"], "{text}");
        let (redacted, _) = redact_paths_in_string(&text, None, "<workspace>", false, None);
        assert!(redacted.ends_with("/x\\nnext\""), "{redacted}");
        assert!(
            !redacted.contains("srv") && !redacted.contains("share"),
            "{redacted}"
        );
    }

    /// Round 8f: the limits of free-text detection, frozen for round 8 (a
    /// structural redesign is tracked separately, verivus-oss/sqry#908).
    /// Each assertion pins today's exact output, so any change in what
    /// stays readable is visible. The over-redaction, whitespace and
    /// glued-word limits are pinned in `the_documented_limits_hold`.
    #[test]
    fn the_frozen_limits_hold_exactly() {
        let redact = |text: &str| redact_paths_in_string(text, None, "<workspace>", false, None).0;
        for (text, output) in [
            // A backslash-separated segment that is exactly an escape,
            // followed by `/` or `\`: the path ends there and the rest stays.
            (
                "/home/alice\\t/secret",
                "<external>/alice\\t<external>/secret",
            ),
            (
                "/mnt/c/proj\\b/secret.txt",
                "<external>/proj\\b<external>/secret.txt",
            ),
            (
                "/mnt/c/Users\\t\\alice\\secret.txt",
                "<external>/Users\\t\\alice\\secret.txt",
            ),
            (
                "/home/alice\\\\\\\\secret",
                "<external>/alice\\\\\\\\secret",
            ),
            ("/mnt/c/x\\notes\\a", "<external>/x\\notes\\a"),
            ("/home/u/a\\n2/x", "<external>/a\\n2/x"),
            // JSON-escaped `//`: not found.
            ("\\/\\/srv\\/share\\/secret", "\\/\\/srv\\/share\\/secret"),
            (
                "file:\\/\\/\\/home\\/alice\\/secret",
                "file:\\/\\/\\/home\\/alice\\/secret",
            ),
            // An octal escape is no boundary.
            ("line\\012/home/alice/secret", "line\\012/home/alice/secret"),
            // A doubly escaped UNC path and a raw `\\?\UNC\` path: not found.
            (
                "\\\\\\\\\\\\\\\\srv\\\\\\\\share\\\\\\\\secret",
                "\\\\\\\\\\\\\\\\srv\\\\\\\\share\\\\\\\\secret",
            ),
            (
                "\\\\?\\UNC\\srv\\share\\secret",
                "\\\\?\\UNC\\srv\\share\\secret",
            ),
            // A `Debug` drive path runs on through a written escape.
            ("\"C:\\\\x\\\\y\\nnext\"", "\"<external>/nnext\""),
            // Round 9: a `/` right after a `-` is no token start, so the
            // path after it is not found, wholly readable.
            ("-/home/alice/old.rs", "-/home/alice/old.rs"),
            ("--root=-/home/alice/x", "--root=-/home/alice/x"),
            ("1-/home/alice/x", "1-/home/alice/x"),
            // Round 9: a UNC server name holding `$` (the WSL share) is not
            // found in text, wholly readable.
            (
                "\\\\wsl$\\Ubuntu\\home\\alice\\x",
                "\\\\wsl$\\Ubuntu\\home\\alice\\x",
            ),
        ] {
            assert_eq!(redact(text), output, "{text:?}");
        }
        // Neither round 9 form is detected at all.
        for text in ["-/home/alice/old.rs", "\\\\wsl$\\Ubuntu\\home\\alice\\x"] {
            assert!(detect_paths_in_string(text).is_empty(), "{text:?}");
        }
        // Under a path key the WSL share is redacted as any UNC path is.
        let mut value = serde_json::json!({ "path": "\\\\wsl$\\Ubuntu\\home\\alice\\x" });
        crate::Redactor::with_defaults().redact(&mut value);
        let shown = value["path"].as_str().expect("a string");
        assert!(
            shown.starts_with("<network:")
                && shown.ends_with(">/home/alice/x")
                && !shown.contains("wsl"),
            "{shown}"
        );
        // A UNC path's output carries the `<network:hash>` prefix twice.
        let unc = redact("\\\\srv\\share\\x");
        let prefix = unc.split('>').next().expect("a prefix").to_string() + ">";
        assert!(prefix.starts_with("<network:"), "{unc}");
        assert_eq!(unc, format!("{prefix}{prefix}/x"));
    }

    /// The limits the module documentation states, both ways: a token
    /// that reads as an absolute path is found even when it is not one
    /// (a bare JSON pointer, one after `#` that follows neither a word, a
    /// `/` nor a quote, an unterminated regex literal after `~=`, a
    /// scheme-less URL path, a URL path in a query value, a JS `@/` alias,
    /// a regex between slashes outside `~=` whose first character can
    /// open a path segment, a division); a path ends at whitespace; a path
    /// glued to the end of a word longer than a one-letter flag (a digit
    /// is a word character: `1/home/u/x` reads as relative) and a path in a
    /// URL's own path are not found.
    #[test]
    fn the_documented_limits_hold() {
        for (text, found) in [
            ("at /results/0", "/results/0"),
            ("#/definitions/Foo", "/definitions/Foo"),
            ("name~=/foo", "/foo"),
            ("GET /api/users", "/api/users"),
            ("https://h/login?next=/api/users", "/api/users"),
            ("import '@/components/x'", "/components/x"),
            ("matches /foo/i", "/foo/i"),
            ("x = y /2", "/2"),
            ("read /mnt/My Dir/x", "/mnt/My"),
        ] {
            let paths = detect_paths_in_string(text);
            assert_eq!(paths.len(), 1, "{text:?}: {paths:?}");
            assert_eq!(paths[0].path, found, "{text:?}");
        }
        for text in [
            "-isystem/usr/include",
            "https://host/home/u/x",
            "1/home/u/x",
            "v2/home/u/x",
        ] {
            assert!(
                detect_paths_in_string(text).is_empty(),
                "{text:?}: {:?}",
                detect_paths_in_string(text)
            );
        }
    }

    /// Multiple paths in one text are found in order and never overlap, so
    /// redaction splices each one in place.
    #[test]
    fn paths_are_found_in_order_without_overlap() {
        let text = "a C:/x/y and /mnt/b, file:///nix/c then \\\\s\\h\\d end";
        let paths = detect_paths_in_string(text);
        let found: Vec<&str> = paths.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(found, ["C:/x/y", "/mnt/b,", "file:///nix/c", "\\\\s\\h\\d"]);
        for pair in paths.windows(2) {
            assert!(pair[0].end <= pair[1].start, "{pair:?}");
        }
        let (redacted, count) = redact_paths_in_string(text, None, "<workspace>", false, None);
        assert_eq!(count, 4);
        for leaked in ["C:/x", "/mnt", "/nix", "\\\\s"] {
            assert!(!redacted.contains(leaked), "{redacted}");
        }
    }
}
