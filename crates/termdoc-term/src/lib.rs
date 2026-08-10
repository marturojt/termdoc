//! The terminal's real capabilities, and the `Fidelity` type that summarizes them.
//!
//! Graceful degradation is a project principle, so it is modeled as an *input* to the
//! layout and the backend rather than as `if`s scattered through the code. That is what
//! makes each rung verifiable with a snapshot test.
//!
//! See docs/DESIGN.md §5.

#![warn(missing_debug_implementations)]

use std::io::IsTerminal;

/// Fallback width when there is no way to find out. 80 is the convention.
pub const DEFAULT_WIDTH: usize = 80;
/// Width cap, so very wide monitors do not produce unreadably long lines.
pub const MAX_COMFORTABLE_WIDTH: usize = 120;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum ColorDepth {
    /// No color sequences. Bold and underline are still allowed.
    #[default]
    None,
    /// The 16 ANSI names. Respects the user's own palette.
    Ansi16,
    /// The 256-color indexed palette.
    Ansi256,
    /// 24-bit color.
    TrueColor,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum UnicodeLevel {
    /// ASCII only: borders with `+-|`, bullets with `*`.
    Ascii,
    /// Box-drawing characters and typographic bullets.
    #[default]
    Full,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GraphicsProto {
    #[default]
    None,
    Halfblocks,
    Sixel,
    ITerm2,
    Kitty,
}

/// A summary of what the terminal supports. Consumed by `termdoc-layout` (to pick glyphs)
/// and `termdoc-backend` (to pick escape sequences).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fidelity {
    pub color: ColorDepth,
    pub unicode: UnicodeLevel,
    pub graphics: GraphicsProto,
    pub hyperlinks: bool,
}

impl Default for Fidelity {
    fn default() -> Self {
        Fidelity {
            color: ColorDepth::Ansi16,
            unicode: UnicodeLevel::Full,
            graphics: GraphicsProto::None,
            hyperlinks: false,
        }
    }
}

impl Fidelity {
    /// The lowest rung: pure text. This is what a pipe gets.
    pub const PLAIN: Fidelity = Fidelity {
        color: ColorDepth::None,
        unicode: UnicodeLevel::Ascii,
        graphics: GraphicsProto::None,
        hyperlinks: false,
    };

    /// Everything available. Useful in tests to pin the highest rung.
    pub const FULL: Fidelity = Fidelity {
        color: ColorDepth::TrueColor,
        unicode: UnicodeLevel::Full,
        graphics: GraphicsProto::Kitty,
        hyperlinks: true,
    };

    pub fn is_plain(&self) -> bool {
        self.color == ColorDepth::None && self.unicode == UnicodeLevel::Ascii
    }
}

/// The detected capabilities, geometry included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    pub is_tty: bool,
    pub width: usize,
    pub height: usize,
    pub fidelity: Fidelity,
}

/// Environment access, injectable so detection can be tested without a real terminal.
pub trait Env {
    fn var(&self, key: &str) -> Option<String>;
    fn stdout_is_tty(&self) -> bool;
    fn terminal_size(&self) -> Option<(usize, usize)>;
}

/// The real process environment.
#[derive(Debug, Default)]
pub struct SystemEnv;

impl Env for SystemEnv {
    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }

    fn stdout_is_tty(&self) -> bool {
        std::io::stdout().is_terminal()
    }

    fn terminal_size(&self) -> Option<(usize, usize)> {
        terminal_size::terminal_size().map(|(w, h)| (w.0 as usize, h.0 as usize))
    }
}

/// Detects the current environment's capabilities.
pub fn detect() -> Caps {
    detect_with(&SystemEnv)
}

pub fn detect_with(env: &dyn Env) -> Caps {
    let is_tty = env.stdout_is_tty();
    let (width, height) = geometry(env, is_tty);

    Caps {
        is_tty,
        width,
        height,
        fidelity: Fidelity {
            color: color_depth(env, is_tty),
            unicode: unicode_level(env),
            // The active graphics probe lands in M3, with images. Until then we do not
            // advertise what we cannot paint.
            graphics: GraphicsProto::None,
            hyperlinks: is_tty && supports_hyperlinks(env),
        },
    }
}

fn geometry(env: &dyn Env, is_tty: bool) -> (usize, usize) {
    // `COLUMNS` outranks the ioctl: it is how a script forces the width.
    if let Some(cols) = env.var("COLUMNS").and_then(|v| v.parse::<usize>().ok())
        && cols > 0
    {
        let rows = env
            .var("LINES")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(24);
        return (cols, rows);
    }
    if is_tty
        && let Some((w, h)) = env.terminal_size()
        && w > 0
    {
        return (w, h);
    }
    (DEFAULT_WIDTH, 24)
}

fn color_depth(env: &dyn Env, is_tty: bool) -> ColorDepth {
    // https://no-color.org — any non-empty value disables color.
    if env.var("NO_COLOR").is_some_and(|v| !v.is_empty()) {
        return ColorDepth::None;
    }
    // `CLICOLOR_FORCE` forces color even without a TTY: that is how you colorize through
    // something like `less -R`.
    let forced = env.var("CLICOLOR_FORCE").is_some_and(|v| v != "0");
    if !is_tty && !forced {
        return ColorDepth::None;
    }
    if env.var("CLICOLOR").is_some_and(|v| v == "0") && !forced {
        return ColorDepth::None;
    }

    if let Some(ct) = env.var("COLORTERM")
        && (ct.contains("truecolor") || ct.contains("24bit"))
    {
        return ColorDepth::TrueColor;
    }

    match env.var("TERM").as_deref() {
        None => {
            // A TTY but no TERM: assume the safe minimum.
            if forced {
                ColorDepth::Ansi16
            } else {
                ColorDepth::None
            }
        }
        Some("dumb") => ColorDepth::None,
        Some(t) if t.contains("direct") => ColorDepth::TrueColor,
        Some(t) if t.contains("256") => ColorDepth::Ansi256,
        Some(_) => ColorDepth::Ansi16,
    }
}

fn unicode_level(env: &dyn Env) -> UnicodeLevel {
    for key in ["LC_ALL", "LC_CTYPE", "LANG"] {
        if let Some(v) = env.var(key) {
            if v.is_empty() {
                continue;
            }
            // Any locale that does not declare UTF-8 is treated as ASCII, `C` and `POSIX`
            // included. Without a UTF-8 guarantee, emitting box-drawing produces
            // mojibake, which is worse than a border made of hyphens.
            let up = v.to_ascii_uppercase();
            return if up.contains("UTF-8") || up.contains("UTF8") {
                UnicodeLevel::Full
            } else {
                UnicodeLevel::Ascii
            };
        }
    }
    // macOS often does not export LANG in non-interactive sessions and is UTF-8 anyway.
    // So is Windows Terminal. Assuming UTF-8 is right more often than it is wrong.
    UnicodeLevel::Full
}

fn supports_hyperlinks(env: &dyn Env) -> bool {
    if env.var("TERM").as_deref() == Some("dumb") {
        return false;
    }
    // Terminals known to implement OSC 8. One that is not on this list gets numbered
    // references: correct, just less pretty.
    matches!(
        env.var("TERM_PROGRAM").as_deref(),
        Some("iTerm.app" | "WezTerm" | "ghostty" | "vscode" | "Hyper")
    ) || env.var("KITTY_WINDOW_ID").is_some()
        || env
            .var("VTE_VERSION")
            .is_some_and(|v| v.parse::<u32>().map(|n| n >= 5000).unwrap_or(false))
        || env.var("WT_SESSION").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct FakeEnv {
        vars: HashMap<String, String>,
        tty: bool,
        size: Option<(usize, usize)>,
    }

    impl FakeEnv {
        fn new() -> Self {
            FakeEnv::default()
        }
        fn tty(mut self, v: bool) -> Self {
            self.tty = v;
            self
        }
        fn set(mut self, k: &str, v: &str) -> Self {
            self.vars.insert(k.to_string(), v.to_string());
            self
        }
        fn size(mut self, w: usize, h: usize) -> Self {
            self.size = Some((w, h));
            self
        }
    }

    impl Env for FakeEnv {
        fn var(&self, key: &str) -> Option<String> {
            self.vars.get(key).cloned()
        }
        fn stdout_is_tty(&self) -> bool {
            self.tty
        }
        fn terminal_size(&self) -> Option<(usize, usize)> {
            self.size
        }
    }

    #[test]
    fn no_tty_means_no_color() {
        // The invariant that makes `termdoc x.md > f` produce a clean file.
        let env = FakeEnv::new().tty(false).set("COLORTERM", "truecolor");
        assert_eq!(detect_with(&env).fidelity.color, ColorDepth::None);
    }

    #[test]
    fn no_color_outranks_everything() {
        let env = FakeEnv::new()
            .tty(true)
            .set("NO_COLOR", "1")
            .set("COLORTERM", "truecolor")
            .set("CLICOLOR_FORCE", "1");
        assert_eq!(detect_with(&env).fidelity.color, ColorDepth::None);
    }

    #[test]
    fn clicolor_force_colorizes_without_a_tty() {
        let env = FakeEnv::new()
            .tty(false)
            .set("CLICOLOR_FORCE", "1")
            .set("TERM", "xterm-256color");
        assert_eq!(detect_with(&env).fidelity.color, ColorDepth::Ansi256);
    }

    #[test]
    fn term_dumb_has_neither_color_nor_links() {
        let env = FakeEnv::new().tty(true).set("TERM", "dumb");
        let caps = detect_with(&env);
        assert_eq!(caps.fidelity.color, ColorDepth::None);
        assert!(!caps.fidelity.hyperlinks);
    }

    #[test]
    fn color_rungs() {
        let base = || FakeEnv::new().tty(true);
        assert_eq!(
            detect_with(&base().set("TERM", "xterm")).fidelity.color,
            ColorDepth::Ansi16
        );
        assert_eq!(
            detect_with(&base().set("TERM", "xterm-256color"))
                .fidelity
                .color,
            ColorDepth::Ansi256
        );
        assert_eq!(
            detect_with(&base().set("COLORTERM", "truecolor").set("TERM", "xterm"))
                .fidelity
                .color,
            ColorDepth::TrueColor
        );
    }

    #[test]
    fn columns_outranks_the_ioctl() {
        let env = FakeEnv::new().tty(true).size(200, 50).set("COLUMNS", "40");
        assert_eq!(detect_with(&env).width, 40);
    }

    #[test]
    fn without_a_tty_or_columns_it_uses_80() {
        let env = FakeEnv::new().tty(false);
        assert_eq!(detect_with(&env).width, DEFAULT_WIDTH);
    }

    #[test]
    fn the_c_locale_degrades_to_ascii() {
        let env = FakeEnv::new().tty(true).set("LANG", "C");
        assert_eq!(detect_with(&env).fidelity.unicode, UnicodeLevel::Ascii);

        let env = FakeEnv::new().tty(true).set("LANG", "en_US.UTF-8");
        assert_eq!(detect_with(&env).fidelity.unicode, UnicodeLevel::Full);
    }

    #[test]
    fn hyperlinks_only_in_known_terminals() {
        let env = FakeEnv::new().tty(true).set("TERM_PROGRAM", "iTerm.app");
        assert!(detect_with(&env).fidelity.hyperlinks);

        let env = FakeEnv::new().tty(true).set("TERM", "xterm");
        assert!(!detect_with(&env).fidelity.hyperlinks);
    }
}
