//! The highlighting engine: grammars, the per-line lexer, and what a scope means.
//!
//! # What `syntect` is used for
//!
//! Only its *parser*. `syntect` can also colour text with a TextMate theme, and this does not:
//! a theme's colours are RGB, and termdoc's whole approach to colour is the opposite — roles
//! the terminal's own palette answers to, degrading through the colour ladder (docs/DESIGN.md
//! §5). So the grammar is run to find the scopes in force at each point of a line, and each
//! scope is mapped to a [`TokenRole`]. The theme (`Theme::token_style`) says what a role looks
//! like, exactly as it does for JSON keys.
//!
//! # What it costs, measured
//!
//! (Apple silicon, release build, `two-face`'s extended grammar set, pure-Rust `fancy-regex`.)
//!
//! | what | cost |
//! |---|---|
//! | loading the grammar set | ~2 ms, once, and only when code is actually highlighted |
//! | parsing, steady state | ~0.5 MB/s: a 31 KB Rust file takes ~58 ms |
//! | the first lines of a language | slower: its regexes compile on first use |
//!
//! The grammars are not in the startup path: nothing here runs for a document without code,
//! and output that carries no colour (a pipe) skips highlighting altogether.
//!
//! Parsing is slow enough that an unbounded file would be a hang, so it is bounded twice, in
//! the same spirit as the JSON and CSV ceilings. [`MAX_HIGHLIGHTED_BYTES`] of a document are
//! highlighted — about a second — and the rest is shown as it is, with a warning. A single line
//! longer than [`MAX_LINE`] (minified code) is not parsed at all, because one regex over a
//! megabyte line is the expensive case, and it is shown plain without ending the highlighting.

use std::ops::Range;
use std::sync::OnceLock;

use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};
use termdoc_core::Source;
use termdoc_core::TokenRole;
use termdoc_core::highlight::Piece;

/// How much of one document is highlighted. At ~0.5 MB/s, about a second.
pub const MAX_HIGHLIGHTED_BYTES: usize = 512 * 1024;

/// A line longer than this is shown as it is, without being parsed.
pub const MAX_LINE: usize = 16 * 1024;

static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();

/// The grammars, loaded on first use.
pub(crate) fn syntaxes() -> &'static SyntaxSet {
    SYNTAXES.get_or_init(two_face::syntax::extra_newlines)
}

/// "Plain Text" is a grammar that recognises nothing. Treating it as a find would turn every
/// ```text block into a highlighted one for no gain.
fn real(syntax: &'static SyntaxReference) -> Option<&'static SyntaxReference> {
    (syntax.name != "Plain Text").then_some(syntax)
}

/// The grammar for the language named on a code fence: `rust`, `py`, `c++`, `rust,no_run`.
pub(crate) fn syntax_for_token(info: &str) -> Option<&'static SyntaxReference> {
    let token = info
        .split(|c: char| c.is_whitespace() || c == ',' || c == '{')
        .next()?
        .trim_start_matches('.');
    if token.is_empty() {
        return None;
    }
    syntaxes().find_syntax_by_token(token).and_then(real)
}

/// The grammar for a source file: by its name, then its extension, then a `#!` line.
pub(crate) fn syntax_for_source(src: &Source) -> Option<&'static SyntaxReference> {
    let set = syntaxes();
    if let Some(path) = src.path() {
        let by_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| set.find_syntax_by_extension(n));
        let by_ext = || {
            path.extension()
                .and_then(|e| e.to_str())
                .and_then(|e| set.find_syntax_by_extension(e))
        };
        if let Some(found) = by_name.or_else(by_ext).and_then(real) {
            return Some(found);
        }
    }
    let first_line = src.probe().split(|b| *b == b'\n').next()?;
    let first_line = std::str::from_utf8(first_line).ok()?;
    set.find_syntax_by_first_line(first_line).and_then(real)
}

/// Scope prefix → role. The innermost scope with a matching rule decides; among rules that match
/// one scope, the longest wins. A scope with no rule defers to the one around it, which is how
/// text inside `meta.function-call` still takes the colour of the `string` it sits in.
///
/// Deliberately absent: `meta.*`. Those scopes wrap whole constructs, arguments and all, and
/// colouring them would paint everything inside.
const RULES: &[(&str, TokenRole)] = &[
    ("comment", TokenRole::Comment),
    ("string", TokenRole::String),
    ("constant.numeric", TokenRole::Number),
    ("constant", TokenRole::Constant),
    ("keyword.operator", TokenRole::Operator),
    // `new`, `sizeof`, `and`: operators by grammar, words to a reader.
    ("keyword.operator.word", TokenRole::Keyword),
    ("keyword", TokenRole::Keyword),
    ("storage", TokenRole::Keyword),
    // The `u8` of `42u8`.
    ("storage.type.numeric", TokenRole::Number),
    ("entity.name.function", TokenRole::Function),
    ("entity.name.tag", TokenRole::Name),
    ("entity.name.type", TokenRole::Type),
    ("entity.name.class", TokenRole::Type),
    ("entity.name.struct", TokenRole::Type),
    ("entity.name.enum", TokenRole::Type),
    ("entity.name.trait", TokenRole::Type),
    ("entity.name.interface", TokenRole::Type),
    ("entity.name.namespace", TokenRole::Type),
    ("entity.other.attribute-name", TokenRole::Attribute),
    ("entity.other.inherited-class", TokenRole::Type),
    ("support.function", TokenRole::Function),
    ("support.type", TokenRole::Type),
    ("support.class", TokenRole::Type),
    ("support.constant", TokenRole::Constant),
    ("variable.function", TokenRole::Function),
    ("variable.type", TokenRole::Type),
    ("variable.language", TokenRole::Keyword),
    ("variable.annotation", TokenRole::Attribute),
    ("punctuation", TokenRole::Punctuation),
    // The quotes and comment markers belong to what they delimit, not to "punctuation".
    ("punctuation.definition.string", TokenRole::String),
    ("punctuation.definition.comment", TokenRole::Comment),
    ("punctuation.definition.annotation", TokenRole::Attribute),
];

fn rules() -> &'static [(Scope, TokenRole)] {
    static COMPILED: OnceLock<Vec<(Scope, TokenRole)>> = OnceLock::new();
    COMPILED.get_or_init(|| {
        let mut rules: Vec<(Scope, TokenRole)> = RULES
            .iter()
            .map(|(scope, role)| (Scope::new(scope).expect("a valid scope"), *role))
            .collect();
        // Most specific first, so the first match is the longest.
        rules.sort_by_key(|(scope, _)| std::cmp::Reverse(scope.len()));
        rules
    })
}

/// `storage.type.<language>`, with nothing between: `i32`, `str`, `int`, `unsigned`.
///
/// Grammars file the primitive types there and the keywords that introduce a definition
/// (`fn`, `struct`, `def`, `class`) one atom deeper, as `storage.type.function.rust`. A prefix
/// rule cannot tell the two apart, so this one counts atoms.
fn is_primitive_type(scope: Scope) -> bool {
    static STORAGE_TYPE: OnceLock<Scope> = OnceLock::new();
    let prefix = STORAGE_TYPE.get_or_init(|| Scope::new("storage.type").expect("a valid scope"));
    scope.len() == 3 && prefix.is_prefix_of(scope)
}

/// Words that grammars file under `storage.type.<language>` although they declare something
/// rather than name a type — Rust's `let` is scoped exactly like its `i32`.
const DECLARATION_WORDS: &[&str] = &[
    "let",
    "var",
    "val",
    "const",
    "fn",
    "func",
    "function",
    "def",
    "class",
    "struct",
    "enum",
    "trait",
    "impl",
    "interface",
    "type",
    "mod",
    "namespace",
    "typedef",
];

/// The role of the text under `stack`, if it has one. `text` is the span being coloured, for the
/// few cases where the scope alone cannot say.
pub(crate) fn role_of(stack: &ScopeStack, text: &str) -> Option<TokenRole> {
    for scope in stack.as_slice().iter().rev() {
        if is_primitive_type(*scope) {
            return Some(if DECLARATION_WORDS.contains(&text) {
                TokenRole::Keyword
            } else {
                TokenRole::Type
            });
        }
        if let Some((_, role)) = rules().iter().find(|(rule, _)| rule.is_prefix_of(*scope)) {
            return Some(*role);
        }
    }
    None
}

/// Lexes a document line by line with one grammar, carrying the parser state between lines.
pub(crate) struct Lexer {
    /// `None` is the plain lexer: it colours nothing.
    parse: Option<ParseState>,
    stack: ScopeStack,
    scratch: String,
    seen: usize,
    /// How much of the document is highlighted before the rest is left plain.
    limit: usize,
    capped: bool,
    notice: Option<String>,
}

impl Lexer {
    pub(crate) fn new(syntax: &'static SyntaxReference) -> Self {
        Lexer {
            parse: Some(ParseState::new(syntax)),
            stack: ScopeStack::new(),
            scratch: String::new(),
            seen: 0,
            limit: MAX_HIGHLIGHTED_BYTES,
            capped: false,
            notice: None,
        }
    }

    /// The same lexer with a different ceiling, for tests that should not need half a megabyte.
    #[cfg(test)]
    pub(crate) fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// A lexer that colours nothing, with an optional warning to raise on the first line.
    pub(crate) fn plain(notice: Option<String>) -> Self {
        Lexer {
            parse: None,
            stack: ScopeStack::new(),
            scratch: String::new(),
            seen: 0,
            limit: MAX_HIGHLIGHTED_BYTES,
            capped: false,
            notice,
        }
    }

    /// A warning waiting to be shown, once.
    pub(crate) fn take_notice(&mut self) -> Option<String> {
        self.notice.take()
    }

    /// Colours one line, given without its terminator. The pieces cover it exactly.
    pub(crate) fn lex(&mut self, body: &str) -> Vec<Piece> {
        let whole = || vec![(0..body.len(), None)];
        self.seen += body.len() + 1;

        let Some(parse) = self.parse.as_mut() else {
            return whole();
        };
        if self.seen > self.limit {
            if !self.capped {
                self.capped = true;
                self.notice = Some(format!(
                    "highlighting stops after {} KiB; the rest is shown without it",
                    self.limit / 1024
                ));
            }
            return whole();
        }
        if body.len() > MAX_LINE {
            return whole();
        }

        // The grammars are written for lines that end in a newline.
        self.scratch.clear();
        self.scratch.push_str(body);
        self.scratch.push('\n');
        let Ok(ops) = parse.parse_line(&self.scratch, syntaxes()) else {
            return whole();
        };

        let mut pieces: Vec<Piece> = Vec::new();
        let mut last = 0;
        for (pos, op) in ops {
            let pos = pos.min(body.len());
            if pos > last {
                push(
                    &mut pieces,
                    last..pos,
                    role_of(&self.stack, &body[last..pos]),
                );
                last = pos;
            }
            if self.stack.apply(&op).is_err() {
                // A grammar and its own scope stack disagreeing: start the stack over rather
                // than colour the rest of the file by it.
                self.stack = ScopeStack::new();
            }
        }
        if last < body.len() {
            push(
                &mut pieces,
                last..body.len(),
                role_of(&self.stack, &body[last..]),
            );
        }
        pieces
    }
}

/// Appends a run, joining it to the previous one when they would be coloured alike.
fn push(pieces: &mut Vec<Piece>, range: Range<usize>, role: Option<TokenRole>) {
    if let Some((prev, prev_role)) = pieces.last_mut()
        && *prev_role == role
        && prev.end == range.start
    {
        prev.end = range.end;
        return;
    }
    pieces.push((range, role));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A line as `{role|text}` runs, so a test reads like the colouring it checks.
    fn show(lexer: &mut Lexer, line: &str) -> String {
        lexer
            .lex(line)
            .into_iter()
            .map(|(r, role)| match role {
                Some(role) => format!("{{{role:?}|{}}}", &line[r]),
                None => line[r].to_string(),
            })
            .collect()
    }

    fn lexer(token: &str) -> Lexer {
        Lexer::new(syntax_for_token(token).unwrap_or_else(|| panic!("no grammar for {token}")))
    }

    fn one(token: &str, line: &str) -> String {
        show(&mut lexer(token), line)
    }

    #[test]
    fn rust_tokens_get_their_roles() {
        let out = one("rust", "pub fn main() { let x: i32 = 42; } // done");
        for wanted in [
            "{Keyword|pub}",
            "{Keyword|fn}",
            "{Function|main}",
            "{Keyword|let}",
            "{Type|i32}",
            "{Number|42}",
            "{Comment|// done}",
        ] {
            assert!(out.contains(wanted), "{wanted} not in {out}");
        }
    }

    #[test]
    fn primitive_types_are_types_and_the_keywords_that_introduce_a_definition_are_not() {
        // Both live under `storage.type.*`; only the depth tells them apart.
        assert!(one("rust", "struct S { x: u8 }").contains("{Type|u8}"));
        assert!(one("rust", "struct S { x: u8 }").contains("{Keyword|struct}"));
        assert!(one("c", "unsigned long x;").contains("{Type|unsigned}"));
        assert!(one("python", "def f(): pass").contains("{Keyword|def}"));
    }

    #[test]
    fn quotes_belong_to_their_string_and_escapes_stand_out() {
        let out = one("rust", r#"let s = "a\nb";"#);
        assert!(out.contains("{String|\"a}"), "{out}");
        assert!(out.contains("{Constant|\\n}"), "{out}");
        assert!(out.contains("{String|b\"}"), "{out}");
    }

    #[test]
    fn a_word_operator_is_a_keyword_and_a_symbol_is_an_operator() {
        assert!(one("js", "x = new Foo();").contains("{Keyword|new}"));
        assert!(one("js", "a && b").contains("{Operator|&&}"));
    }

    #[test]
    fn state_carries_across_lines() {
        let mut lx = lexer("rust");
        assert_eq!(show(&mut lx, "/* first"), "{Comment|/* first}");
        // What would be code anywhere else is comment here.
        assert_eq!(
            show(&mut lx, "fn not_code() {}"),
            "{Comment|fn not_code() {}}"
        );
        assert!(show(&mut lx, "end */ fn real() {}").starts_with("{Comment|end */}"));

        let mut py = lexer("python");
        show(&mut py, "s = \"\"\"line one");
        assert_eq!(show(&mut py, "def x(): pass"), "{String|def x(): pass}");
    }

    #[test]
    fn the_pieces_always_cover_the_line() {
        // Whatever the grammar decides, no byte may be lost: this is a viewer.
        let lines = [
            "",
            "   ",
            "\t\tfn  weird\t( ) {",
            "let é = \"日本語 — café\";",
            "'unterminated",
            "\"also unterminated",
            "/* open",
            "}}}} ))) ]]]",
        ];
        for token in ["rust", "python", "js", "c", "sh", "json", "html", "go"] {
            let mut lx = lexer(token);
            for line in lines {
                let joined: String = lx.lex(line).into_iter().map(|(r, _)| &line[r]).collect();
                assert_eq!(joined, line, "{token}: {line:?}");
            }
        }
    }

    #[test]
    fn a_lexer_without_a_grammar_colours_nothing_and_says_so_once() {
        let mut lx = Lexer::plain(Some("no grammar".into()));
        assert_eq!(show(&mut lx, "fn main() {}"), "fn main() {}");
        assert_eq!(lx.take_notice().as_deref(), Some("no grammar"));
        assert_eq!(lx.take_notice(), None);
    }

    #[test]
    fn a_very_long_line_is_not_parsed_and_does_not_end_the_highlighting() {
        let mut lx = lexer("rust");
        let long = format!("let s = \"{}\";", "x".repeat(MAX_LINE));
        assert_eq!(show(&mut lx, &long), long, "plain, unparsed");
        assert!(show(&mut lx, "fn f() {}").contains("{Keyword|fn}"));
        assert_eq!(
            lx.take_notice(),
            None,
            "one long line is not a reason to warn"
        );
    }

    #[test]
    fn highlighting_stops_at_the_ceiling_with_one_warning() {
        let line = "fn f() { let x = 1; }";
        let mut lx = lexer("rust").with_limit(10 * (line.len() + 1));
        for _ in 0..10 {
            assert!(show(&mut lx, line).contains("{Keyword|fn}"));
            assert_eq!(lx.take_notice(), None);
        }
        // Past it: plain, and told once.
        assert_eq!(show(&mut lx, line), line);
        let notice = lx.take_notice().expect("a warning");
        assert!(notice.contains("highlighting stops"), "{notice}");
        show(&mut lx, line);
        assert_eq!(lx.take_notice(), None);
    }

    #[test]
    fn fence_info_strings_are_understood() {
        for info in [
            "rust",
            "rs",
            "py",
            "python",
            "js",
            "c++",
            "rust,no_run",
            "rust {1,3}",
            ".rust",
        ] {
            assert!(syntax_for_token(info).is_some(), "{info}");
        }
        // These must not turn a block into a highlighted one.
        for info in ["", "text", "plain", "nonsense-lang-xyz"] {
            assert!(syntax_for_token(info).is_none(), "{info:?}");
        }
    }

    #[test]
    fn a_shebang_names_the_language_when_there_is_no_path() {
        let src = Source::from_bytes("script", "#!/usr/bin/env python3\nprint(1)\n");
        assert_eq!(
            syntax_for_source(&src).map(|s| s.name.as_str()),
            Some("Python")
        );
        let none = Source::from_bytes("notes", "just some prose\n");
        assert!(syntax_for_source(&none).is_none());
    }
}
