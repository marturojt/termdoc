//! The command-line surface.

use clap::{ArgAction, Parser, ValueEnum};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum When {
    Auto,
    Always,
    Never,
}

/// Whether the first record of a CSV is a header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CsvHeader {
    /// A first record made only of numbers is data; anything else is a header.
    Auto,
    Yes,
    No,
}

/// Parses `--delimiter`: a single ASCII character, or a name (`comma`, `semicolon`, `tab`,
/// `pipe`, `space`), or `\t`.
fn parse_delimiter(value: &str) -> Result<u8, String> {
    let byte = match value.to_ascii_lowercase().as_str() {
        "comma" => b',',
        "semicolon" => b';',
        "tab" | "\\t" => b'\t',
        "pipe" => b'|',
        "space" => b' ',
        other => match other.as_bytes() {
            [c] if c.is_ascii() => *c,
            _ => {
                return Err(format!(
                    "'{value}' is not a delimiter; use one ASCII character or one of comma, \
                     semicolon, tab, pipe, space"
                ));
            }
        },
    };
    if matches!(byte, b'"' | b'\n' | b'\r') {
        return Err("a quote or a line break cannot be a delimiter".into());
    }
    Ok(byte)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BackendChoice {
    /// Chooses between ANSI and plain text based on the terminal.
    Auto,
    Ansi,
    Plain,
}

#[derive(Debug, Parser)]
#[command(
    name = "termdoc",
    version,
    about = "A universal document viewer for the terminal",
    long_about = "Reads any document and renders it as well as the terminal allows.\n\n\
                  When the output is a terminal, it is colorized and fitted to the available \
                  width. When it is a pipe or a redirection, the output is clean and \
                  composable: stdout carries only the document, and warnings go to stderr.",
    after_help = "Examples:\n  \
                  termdoc README.md\n  \
                  termdoc README.md | head -40\n  \
                  cat README.md | termdoc\n  \
                  termdoc --explain file.dat"
)]
pub struct Cli {
    /// Documents to display. With no arguments, or with '-', input is read from stdin.
    pub files: Vec<String>,

    /// Overrides format detection.
    #[arg(short = 'f', long = "from", value_name = "FMT")]
    pub from: Option<String>,

    /// Output backend.
    #[arg(short = 't', long = "to", value_enum, default_value = "auto")]
    pub to: BackendChoice,

    /// When to colorize the output.
    #[arg(long, value_enum, default_value = "auto", value_name = "WHEN")]
    pub color: When,

    /// Width in columns. Defaults to the terminal's.
    #[arg(long, value_name = "N")]
    pub width: Option<usize>,

    /// Override encoding detection (utf-8, latin1, windows-1252, shift_jis, ...).
    #[arg(long, value_name = "ENC")]
    pub encoding: Option<String>,

    /// Field delimiter of CSV-like input, instead of detecting it (a character, or comma,
    /// semicolon, tab, pipe, space).
    #[arg(long, value_name = "CHAR", value_parser = parse_delimiter)]
    pub delimiter: Option<u8>,

    /// Whether the first CSV record is a header. `auto` treats a first record made only of
    /// numbers as data.
    #[arg(
        long = "csv-header",
        value_enum,
        default_value = "auto",
        value_name = "WHEN"
    )]
    pub csv_header: CsvHeader,

    /// Use ASCII only: no box-drawing and no typographic bullets.
    #[arg(long, action = ArgAction::SetTrue)]
    pub ascii: bool,

    /// Number the lines of preformatted content and code blocks.
    #[arg(short = 'n', long = "line-numbers", action = ArgAction::SetTrue)]
    pub line_numbers: bool,

    /// Show only the document's metadata.
    #[arg(long, action = ArgAction::SetTrue)]
    pub meta: bool,

    /// Explain how the format was detected, then exit.
    #[arg(long, action = ArgAction::SetTrue)]
    pub explain: bool,

    /// List the formats this binary can read, then exit.
    #[arg(long, action = ArgAction::SetTrue)]
    pub formats: bool,

    /// Turn warnings into errors.
    #[arg(long, action = ArgAction::SetTrue)]
    pub strict: bool,

    /// Do not print the filename header when showing several files.
    #[arg(long = "no-header", action = ArgAction::SetTrue)]
    pub no_header: bool,
}

impl Cli {
    /// `true` when stdin has to be read.
    pub fn reads_stdin(&self) -> bool {
        self.files.is_empty() || self.files.iter().any(|f| f == "-")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_cli_definition_is_valid() {
        // clap's `debug_assert`: catches name and short-flag conflicts.
        Cli::command().debug_assert();
    }

    #[test]
    fn no_arguments_means_read_stdin() {
        let cli = Cli::parse_from(["termdoc"]);
        assert!(cli.reads_stdin());
    }

    #[test]
    fn a_lone_dash_means_stdin() {
        let cli = Cli::parse_from(["termdoc", "-"]);
        assert!(cli.reads_stdin());
    }

    #[test]
    fn with_a_file_stdin_is_not_read() {
        let cli = Cli::parse_from(["termdoc", "a.md"]);
        assert!(!cli.reads_stdin());
        assert_eq!(cli.files, vec!["a.md"]);
    }

    #[test]
    fn the_delimiter_flag_takes_names_and_characters() {
        for (arg, byte) in [
            ("tab", b'\t'),
            ("TAB", b'\t'),
            ("\\t", b'\t'),
            (";", b';'),
            ("semicolon", b';'),
            ("pipe", b'|'),
            ("space", b' '),
        ] {
            let cli = Cli::parse_from(["termdoc", "--delimiter", arg, "a.csv"]);
            assert_eq!(cli.delimiter, Some(byte), "{arg}");
        }
        assert_eq!(Cli::parse_from(["termdoc", "a.csv"]).delimiter, None);
    }

    #[test]
    fn a_nonsense_delimiter_is_a_usage_error() {
        for bad in ["ab", "é", "\"", ""] {
            assert!(
                Cli::try_parse_from(["termdoc", "--delimiter", bad, "a.csv"]).is_err(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn the_csv_header_flag_does_not_collide_with_no_header() {
        // `--no-header` hides filename banners and has nothing to do with CSV.
        let cli = Cli::parse_from(["termdoc", "--no-header", "--csv-header", "no", "a.csv"]);
        assert!(cli.no_header);
        assert_eq!(cli.csv_header, CsvHeader::No);
        assert_eq!(
            Cli::parse_from(["termdoc", "a.csv"]).csv_header,
            CsvHeader::Auto
        );
    }

    #[test]
    fn it_accepts_an_encoding_override() {
        let cli = Cli::parse_from(["termdoc", "--encoding", "latin1", "a.txt"]);
        assert_eq!(cli.encoding.as_deref(), Some("latin1"));
    }

    #[test]
    fn it_accepts_several_files() {
        let cli = Cli::parse_from(["termdoc", "a.md", "b.md"]);
        assert_eq!(cli.files.len(), 2);
    }

    #[test]
    fn the_defaults_match_the_design() {
        let cli = Cli::parse_from(["termdoc", "x"]);
        assert_eq!(cli.color, When::Auto);
        assert_eq!(cli.to, BackendChoice::Auto);
        assert!(!cli.ascii);
        assert!(cli.width.is_none());
        assert!(cli.encoding.is_none());
    }
}
