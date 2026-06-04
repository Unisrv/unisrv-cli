//! Cargo-style error reports for `unisrv.hcl` parse / validation failures.
//!
//! Wraps the raw `hcl-rs` error (or one of our own validation messages) with
//! the file path and source so we can render a multi-line message with a line
//! pointer, mirroring how `cargo` and `rustc` surface errors.

use std::fmt;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use console::Style;

#[derive(Debug)]
pub struct ConfigParseError {
    path: PathBuf,
    source: String,
    kind: ParseErrorKind,
}

#[derive(Debug)]
enum ParseErrorKind {
    /// Lexer/parser failure — `hcl-rs` gives us an exact location.
    Syntax {
        message: String,
        line: usize,
        column: usize,
    },
    /// Deserialization or semantic error. Location is best-effort: we scan the
    /// source for a hint string (e.g. an offending field name) when we have one.
    Located {
        message: String,
        location: Option<Span>,
        notes: Vec<String>,
    },
}

#[derive(Debug)]
struct Span {
    line: usize,
    column: usize,
    width: usize,
}

/// Where in the source to scan for the offending token. `occurrence` lets us
/// point at the *second* duplicate block instead of the first, etc.
pub struct Locator<'a> {
    needle: &'a str,
    occurrence: usize,
    kind: LocatorKind,
}

enum LocatorKind {
    /// Match anywhere — used for short literal strings like `service "web"`.
    Substring,
    /// Match only at identifier boundaries and only when followed by `=`,
    /// `{`, or `"` (a label start). Used for HCL field names.
    Field,
}

impl<'a> Locator<'a> {
    pub fn field(needle: &'a str) -> Self {
        Self {
            needle,
            occurrence: 0,
            kind: LocatorKind::Field,
        }
    }

    pub fn substring(needle: &'a str) -> Self {
        Self {
            needle,
            occurrence: 0,
            kind: LocatorKind::Substring,
        }
    }

    pub fn nth(mut self, n: usize) -> Self {
        self.occurrence = n;
        self
    }
}

impl ConfigParseError {
    pub fn from_hcl(path: &Path, source: &str, err: hcl::Error) -> Self {
        if let hcl::Error::Parse(pe) = &err {
            let loc = pe.location();
            // The Display of hcl-rs's parse error is multi-line and has the
            // human-readable reason on a `  = ...` line at the bottom.
            let display = err.to_string();
            let message = display
                .lines()
                .find_map(|l| l.strip_prefix("  = "))
                .map(str::to_string)
                .unwrap_or_else(|| "syntax error".to_string());
            return Self {
                path: path.to_path_buf(),
                source: source.to_string(),
                kind: ParseErrorKind::Syntax {
                    message,
                    line: loc.line(),
                    column: loc.column(),
                },
            };
        }

        // Decode / serde error. No structural location, but we can sometimes
        // dig the offending field name out of the message and locate it.
        let raw = err.to_string();
        let (message, notes, hint) = parse_decode_message(&raw);
        let location = hint
            .as_deref()
            .and_then(|h| locate(source, &Locator::field(h)));
        Self {
            path: path.to_path_buf(),
            source: source.to_string(),
            kind: ParseErrorKind::Located {
                message,
                location,
                notes,
            },
        }
    }

    pub fn validation(
        path: &Path,
        source: &str,
        message: impl Into<String>,
        locator: Option<Locator<'_>>,
    ) -> Self {
        let location = locator.and_then(|l| locate(source, &l));
        Self {
            path: path.to_path_buf(),
            source: source.to_string(),
            kind: ParseErrorKind::Located {
                message: message.into(),
                location,
                notes: Vec::new(),
            },
        }
    }

    fn headline(&self, styles: &ParseErrorStyles) -> impl fmt::Display + '_ {
        styles
            .error
            .apply_to(format!("Unable to parse {}", self.path.display()))
    }

    fn render(&self, styles: &ParseErrorStyles) -> String {
        let mut out = String::new();
        match &self.kind {
            ParseErrorKind::Syntax {
                message,
                line,
                column,
            } => {
                let _ = writeln!(out, "{}: {}", self.headline(styles), message);
                let _ = writeln!(
                    out,
                    "  {} {}:{}:{}",
                    styles.gutter.apply_to("-->"),
                    self.path.display(),
                    line,
                    column,
                );
                write_snippet(&mut out, &self.source, *line, *column, 1, styles);
            }
            ParseErrorKind::Located {
                message,
                location,
                notes,
            } => {
                let _ = writeln!(out, "{}: {}", self.headline(styles), message);
                if let Some(span) = location {
                    let _ = writeln!(
                        out,
                        "  {} {}:{}:{}",
                        styles.gutter.apply_to("-->"),
                        self.path.display(),
                        span.line,
                        span.column,
                    );
                    write_snippet(
                        &mut out,
                        &self.source,
                        span.line,
                        span.column,
                        span.width,
                        styles,
                    );
                } else {
                    let _ = writeln!(
                        out,
                        "  {} {}",
                        styles.gutter.apply_to("-->"),
                        self.path.display(),
                    );
                }
                for note in notes {
                    let _ = writeln!(out, "  {} {}", styles.gutter.apply_to("="), note);
                }
            }
        }
        out
    }
}

impl fmt::Display for ConfigParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let styles = if console::Term::stderr().features().colors_supported() {
            ParseErrorStyles::colored()
        } else {
            ParseErrorStyles::plain()
        };
        f.write_str(&self.render(&styles))
    }
}

impl std::error::Error for ConfigParseError {}

struct ParseErrorStyles {
    error: Style,
    gutter: Style,
    caret: Style,
}

impl ParseErrorStyles {
    fn colored() -> Self {
        Self {
            error: Style::new().red().bold(),
            gutter: Style::new().blue().bold(),
            caret: Style::new().red().bold(),
        }
    }

    fn plain() -> Self {
        Self {
            error: Style::new(),
            gutter: Style::new(),
            caret: Style::new(),
        }
    }
}

fn write_snippet(
    out: &mut String,
    source: &str,
    line_num: usize,
    column: usize,
    width: usize,
    styles: &ParseErrorStyles,
) {
    let line_text = source.lines().nth(line_num.saturating_sub(1)).unwrap_or("");
    let gutter_w = line_num.to_string().len();
    let pipe = styles.gutter.apply_to("|");
    let _ = writeln!(out, "{:>w$} {}", "", pipe, w = gutter_w);
    let _ = writeln!(
        out,
        "{} {} {}",
        styles.gutter.apply_to(line_num),
        pipe,
        line_text,
    );
    let pad = " ".repeat(column.saturating_sub(1));
    let carets = styles.caret.apply_to("^".repeat(width.max(1)));
    let _ = writeln!(out, "{:>w$} {} {}{}", "", pipe, pad, carets, w = gutter_w);
}

/// Best-effort dissection of an `hcl-rs` decode/serde message.
/// Returns `(headline, notes, locator_hint)`.
fn parse_decode_message(raw: &str) -> (String, Vec<String>, Option<String>) {
    if let Some(field) = take_backtick_after(raw, "unknown field `") {
        let head = format!("unknown field `{field}`");
        let notes = expected_tail(raw)
            .map(|tail| vec![format!("expected {tail}")])
            .unwrap_or_default();
        return (head, notes, Some(field));
    }
    if let Some(field) = take_backtick_after(raw, "missing field `") {
        return (format!("missing field `{field}`"), Vec::new(), None);
    }
    (raw.to_string(), Vec::new(), None)
}

fn take_backtick_after(haystack: &str, prefix: &str) -> Option<String> {
    let after = haystack.split_once(prefix)?.1;
    let end = after.find('`')?;
    Some(after[..end].to_string())
}

fn expected_tail(raw: &str) -> Option<String> {
    let (_, tail) = raw.split_once(", expected")?;
    Some(tail.trim_start().trim_end().to_string())
}

fn locate(source: &str, locator: &Locator<'_>) -> Option<Span> {
    let mut search_from = 0;
    let mut hit = 0usize;
    while let Some(rel) = source[search_from..].find(locator.needle) {
        let abs = search_from + rel;
        let accepted = match locator.kind {
            LocatorKind::Substring => true,
            LocatorKind::Field => looks_like_field_use(source, abs, locator.needle),
        };
        if accepted {
            if hit == locator.occurrence {
                return Some(offset_to_span(source, abs, locator.needle.chars().count()));
            }
            hit += 1;
        }
        search_from = abs + locator.needle.len().max(1);
    }
    None
}

fn looks_like_field_use(source: &str, pos: usize, needle: &str) -> bool {
    let bytes = source.as_bytes();
    let before_ok = pos == 0 || !is_ident_byte(bytes[pos - 1]);
    let after_idx = pos + needle.len();
    let after_ok = after_idx >= bytes.len() || !is_ident_byte(bytes[after_idx]);
    if !(before_ok && after_ok) {
        return false;
    }
    // Must be followed (after whitespace) by `=`, `{`, or a label start `"`.
    let tail = &source[after_idx..];
    let trimmed = tail.trim_start_matches([' ', '\t']);
    matches!(
        trimmed.as_bytes().first(),
        Some(b'=') | Some(b'{') | Some(b'"')
    )
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

fn offset_to_span(source: &str, offset: usize, width: usize) -> Span {
    let mut line = 1usize;
    let mut line_start = 0usize;
    for (i, b) in source.as_bytes().iter().enumerate() {
        if i == offset {
            break;
        }
        if *b == b'\n' {
            line += 1;
            line_start = i + 1;
        }
    }
    let col = source[line_start..offset].chars().count() + 1;
    Span {
        line,
        column: col,
        width,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_plain(err: &ConfigParseError) -> String {
        err.render(&ParseErrorStyles::plain())
    }

    #[test]
    fn parses_unknown_field_message() {
        let (head, notes, hint) =
            parse_decode_message("unknown field `build`, expected one of `service`, `port`");
        assert_eq!(head, "unknown field `build`");
        assert_eq!(hint.as_deref(), Some("build"));
        assert_eq!(notes, vec!["expected one of `service`, `port`"]);
    }

    #[test]
    fn parses_missing_field_message() {
        let (head, notes, hint) = parse_decode_message("missing field `image`");
        assert_eq!(head, "missing field `image`");
        assert!(notes.is_empty());
        assert!(hint.is_none());
    }

    #[test]
    fn locator_field_finds_first_occurrence() {
        let src = "deployment \"d\" {\n  build = \"x\"\n}\n";
        let span = locate(src, &Locator::field("build")).unwrap();
        assert_eq!(span.line, 2);
        assert_eq!(span.column, 3);
        assert_eq!(span.width, 5);
    }

    #[test]
    fn locator_field_skips_substring_inside_other_idents() {
        // `rebuild` should NOT match `build`.
        let src = "rebuild = true\nbuild = \"x\"\n";
        let span = locate(src, &Locator::field("build")).unwrap();
        assert_eq!(span.line, 2);
    }

    #[test]
    fn locator_substring_with_occurrence() {
        let src = "service \"web\" { }\nservice \"web\" { }\n";
        let first = locate(src, &Locator::substring("service \"web\"")).unwrap();
        let second = locate(src, &Locator::substring("service \"web\"").nth(1)).unwrap();
        assert_eq!(first.line, 1);
        assert_eq!(second.line, 2);
    }

    #[test]
    fn renders_syntax_error_with_pointer() {
        let bad = "project = \"x\"\nservice \"web\" {\n  host = \"unterminated\n}\n";
        let err = hcl::from_str::<hcl::Body>(bad).unwrap_err();
        let pe = ConfigParseError::from_hcl(Path::new("unisrv.hcl"), bad, err);
        let out = render_plain(&pe);
        assert!(out.starts_with("Unable to parse unisrv.hcl: "), "{out}");
        assert!(out.contains("--> unisrv.hcl:"), "{out}");
        assert!(out.contains("|"), "{out}");
        assert!(out.contains("^"), "{out}");
    }
}
