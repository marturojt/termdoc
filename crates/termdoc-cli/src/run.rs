//! Wiring the pipeline: source → detection → reader → layout → backend → output.
//!
//! This is the only module that sees every stage at once. The stages themselves still know
//! nothing about each other; they are merely plugged together here.

use std::io::Write;

use termdoc_backend::{AnsiBackend, PlainBackend};
use termdoc_core::{
    Backend, Diagnostic, Error, FormatId, ReadContext, Registry, Result, Severity, Source, exit,
};
use termdoc_layout::{Layout, LayoutOptions, Theme};
use termdoc_term::{Caps, ColorDepth, Fidelity, MAX_COMFORTABLE_WIDTH, UnicodeLevel};

use crate::cli::{BackendChoice, Cli, When};

/// Builds the registry with everything this binary can read.
pub fn build_registry() -> Registry {
    let mut registry = Registry::new();
    // Detection first, then readers. The two are independent: `termdoc-detect` names formats
    // this build may have no reader for, which is handled by the fallback in `pick_reader`.
    termdoc_detect::register(&mut registry);
    termdoc_read_text::register(&mut registry);
    termdoc_read_data::register(&mut registry);
    // In M4, discovered plugins get added here. Order matters: because they register later,
    // a plugin can deliberately replace a built-in.
    registry
}

/// Formats that are readable as plain text when their own reader is not available.
///
/// The detection engine can name more formats than there are readers for — that is the normal
/// state of a project mid-roadmap. Refusing to show a `.json` file merely because its dedicated
/// reader has not landed yet would be a regression against just treating it as text, and it
/// contradicts the point of a universal viewer. So the fallback shows the file and says why it
/// looks plainer than it should.
fn readable_as_text(format: FormatId) -> bool {
    matches!(
        format,
        FormatId::Toml
            | FormatId::Xml
            | FormatId::Csv
            | FormatId::Html
            | FormatId::SourceCode
            | FormatId::PlainText
            | FormatId::Log
            | FormatId::Markdown
    )
}

/// Resolves the reader, degrading to plain text when there is none yet.
///
/// Returns the reader together with a diagnostic to report when a fallback happened.
fn pick_reader(
    registry: &Registry,
    format: FormatId,
) -> Result<(&dyn termdoc_core::DocumentReader, Option<Diagnostic>)> {
    if let Some(reader) = registry.reader_for(format) {
        return Ok((reader, None));
    }

    if readable_as_text(format) {
        let reader = registry
            .reader_for(FormatId::PlainText)
            .ok_or_else(|| Error::Unsupported("no plain-text reader in this build".into()))?;
        return Ok((
            reader,
            Some(Diagnostic::warning(format!(
                "detected {format}, but this build has no {format} reader yet; showing it as \
                 plain text"
            ))),
        ));
    }

    Err(Error::Unsupported(format!(
        "'{format}' cannot be displayed by this build"
    )))
}

/// Resolves the effective capabilities by combining what was detected with the flags.
///
/// Flags always win: if the user asks for `--ascii` in a UTF-8 terminal, it is because they
/// know something detection does not.
pub fn resolve_fidelity(cli: &Cli, caps: Caps) -> Fidelity {
    let mut fidelity = caps.fidelity;

    match cli.color {
        When::Never => fidelity.color = ColorDepth::None,
        When::Always => {
            if fidelity.color == ColorDepth::None {
                // With no information from the terminal, 16 colors is the only universal
                // choice.
                fidelity.color = ColorDepth::Ansi16;
            }
        }
        When::Auto => {}
    }

    if cli.ascii {
        fidelity.unicode = UnicodeLevel::Ascii;
    }

    // A plain-text backend can emit neither color nor links, so asking for it means dropping
    // to the lowest rung.
    if cli.to == BackendChoice::Plain {
        fidelity.color = ColorDepth::None;
        fidelity.hyperlinks = false;
    }

    if fidelity.color == ColorDepth::None {
        fidelity.hyperlinks = false;
    }

    fidelity
}

pub fn resolve_width(cli: &Cli, caps: Caps) -> usize {
    match cli.width {
        Some(w) => w.max(1),
        // On a very wide monitor a 300-column line is unreadable: the eye loses the row. The
        // cap applies only when the width comes from the terminal, never when the user asks
        // for it explicitly.
        None => caps.width.clamp(1, MAX_COMFORTABLE_WIDTH),
    }
}

fn make_backend(cli: &Cli, fidelity: Fidelity) -> Box<dyn Backend> {
    match cli.to {
        BackendChoice::Plain => Box::new(PlainBackend::new()),
        BackendChoice::Ansi => Box::new(AnsiBackend::new(fidelity)),
        BackendChoice::Auto => termdoc_backend::for_fidelity(fidelity),
    }
}

/// Opens a source from a command-line argument.
fn open_source(arg: &str) -> Result<Source> {
    if arg == "-" {
        Source::from_stdin()
    } else {
        Source::open(arg)
    }
}

/// Resolves the format and configures the source's encoding.
///
/// `--from` wins over detection, but the encoding is still detected either way: the two are
/// independent questions, and someone forcing `--from json` should not thereby lose latin-1
/// decoding.
///
/// Takes `&mut Source` because applying an encoding must happen before any reader borrows the
/// source. That is enforced by the borrow checker rather than by a comment.
fn resolve_format(cli: &Cli, registry: &Registry, src: &mut Source) -> Result<FormatId> {
    let detection = termdoc_detect::prepare(registry, src, cli.encoding.as_deref())?;

    if let Some(name) = &cli.from {
        return FormatId::parse(name).ok_or_else(|| {
            Error::Usage(format!(
                "unknown format '{name}'; valid ones are: {}",
                FormatId::all_names().join(", ")
            ))
        });
    }
    Ok(detection.format)
}

pub fn run(cli: &Cli, out: &mut dyn Write, err: &mut dyn Write) -> Result<i32> {
    let registry = build_registry();

    if cli.formats {
        for name in registry.formats() {
            writeln!(out, "{name}")?;
        }
        return Ok(exit::OK);
    }

    let caps = termdoc_term::detect();
    let fidelity = resolve_fidelity(cli, caps);
    let width = resolve_width(cli, caps);

    let targets: Vec<String> = if cli.reads_stdin() && cli.files.is_empty() {
        vec!["-".to_string()]
    } else {
        cli.files.clone()
    };

    let show_headers = targets.len() > 1 && !cli.no_header;
    let mut worst = Severity::Info;
    let mut any_diagnostic = false;

    for (idx, target) in targets.iter().enumerate() {
        let mut src = open_source(target)?;
        let format = resolve_format(cli, &registry, &mut src)?;
        let src = src;

        if cli.explain {
            explain(&registry, &src, format, cli, out)?;
            continue;
        }

        let (reader, fallback) = pick_reader(&registry, format)?;
        if let Some(d) = &fallback {
            report(d, &src, err)?;
            any_diagnostic = true;
            worst = worst.max(d.severity);
        }

        let ctx = ReadContext {
            metadata_only: cli.meta,
            ..ReadContext::default()
        };
        let events = reader.read(&src, &ctx)?;

        if cli.meta {
            print_metadata(events, &src, format, out)?;
            continue;
        }

        if show_headers {
            if idx > 0 {
                writeln!(out)?;
            }
            writeln!(out, "==> {} <==", src.display_name())?;
        }

        let opts = LayoutOptions {
            width,
            fidelity,
            theme: if fidelity.color == ColorDepth::None {
                Theme::plain()
            } else {
                Theme::default()
            },
            line_numbers: cli.line_numbers,
            // In a pipe, warnings must not contaminate stdout: they go to stderr.
            inline_diagnostics: caps.is_tty,
        };

        let mut layout = Layout::new(events, opts);
        let mut backend = make_backend(cli, fidelity);
        backend.begin(out)?;

        loop {
            match layout.next() {
                None => break,
                Some(Ok(line)) => backend.write_line(&line, out)?,
                Some(Err(e)) => {
                    backend.finish(out)?;
                    return Err(e);
                }
            }
        }
        backend.finish(out)?;

        // Diagnostics the layout did not emit inline are reported on stderr now.
        if !caps.is_tty {
            for d in layout.diagnostics() {
                report(d, &src, err)?;
                any_diagnostic = true;
                worst = worst.max(d.severity);
            }
        } else if !layout.diagnostics().is_empty() {
            any_diagnostic = true;
            worst = layout
                .diagnostics()
                .iter()
                .map(|d| d.severity)
                .max()
                .unwrap_or(worst)
                .max(worst);
        }
    }

    if cli.strict && any_diagnostic && worst >= Severity::Warning {
        return Ok(exit::GENERIC);
    }
    Ok(exit::OK)
}

fn report(d: &Diagnostic, src: &Source, err: &mut dyn Write) -> Result<()> {
    let label = match d.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "note",
    };
    writeln!(
        err,
        "termdoc: {label}: {}: {}",
        src.display_name(),
        d.message
    )?;
    Ok(())
}

fn explain(
    registry: &Registry,
    src: &Source,
    chosen: FormatId,
    cli: &Cli,
    out: &mut dyn Write,
) -> Result<()> {
    writeln!(out, "source:   {}", src.display_name())?;
    writeln!(out, "size:     {} bytes", src.len())?;
    writeln!(out, "format:   {chosen}")?;
    writeln!(out, "encoding: {}", termdoc_detect::charset_reason(src))?;

    if cli.from.is_some() {
        writeln!(out, "reason:   forced by --from (confidence 100)")?;
        return Ok(());
    }

    let candidates = registry.detect_all(src);
    if candidates.is_empty() {
        writeln!(
            out,
            "reason:   no detector claimed it; the fallback was applied"
        )?;
    } else {
        writeln!(out, "candidates:")?;
        for d in &candidates {
            writeln!(out, "  {:>3}  {:<10} {}", d.confidence, d.format, d.reason)?;
        }
    }
    Ok(())
}

fn print_metadata(
    events: termdoc_core::Events<'_>,
    src: &Source,
    format: FormatId,
    out: &mut dyn Write,
) -> Result<()> {
    use termdoc_core::{Event, Tag};

    // Metadata arrives in the very first event, so the body never has to be read.
    let mut meta = None;
    for event in events {
        if let Ok(spanned) = event
            && let Event::Start(Tag::Document(m)) = spanned.node
        {
            meta = Some(m);
            break;
        }
    }

    writeln!(out, "file:     {}", src.display_name())?;
    writeln!(out, "format:   {format}")?;
    writeln!(out, "size:     {} bytes", src.len())?;

    if let Some(m) = meta {
        if let Some(t) = &m.title {
            writeln!(out, "title:    {t}")?;
        }
        if !m.authors.is_empty() {
            writeln!(out, "authors:  {}", m.authors.join(", "))?;
        }
        if let Some(d) = &m.date {
            writeln!(out, "date:     {d}")?;
        }
        if let Some(p) = m.page_count {
            writeln!(out, "pages:    {p}")?;
        }
        for (k, v) in &m.custom {
            writeln!(out, "{k}: {v}")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    fn cli(args: &[&str]) -> Cli {
        let mut full = vec!["termdoc"];
        full.extend_from_slice(args);
        Cli::parse_from(full)
    }

    fn caps(tty: bool, color: ColorDepth, width: usize) -> Caps {
        Caps {
            is_tty: tty,
            width,
            height: 24,
            fidelity: Fidelity {
                color,
                unicode: UnicodeLevel::Full,
                graphics: termdoc_term::GraphicsProto::None,
                hyperlinks: tty,
            },
        }
    }

    #[test]
    fn color_never_overrides_detection() {
        let f = resolve_fidelity(
            &cli(&["--color", "never"]),
            caps(true, ColorDepth::TrueColor, 80),
        );
        assert_eq!(f.color, ColorDepth::None);
        assert!(!f.hyperlinks, "no color also means no OSC 8");
    }

    #[test]
    fn color_always_colorizes_without_a_tty() {
        let f = resolve_fidelity(
            &cli(&["--color", "always"]),
            caps(false, ColorDepth::None, 80),
        );
        assert_eq!(f.color, ColorDepth::Ansi16);
    }

    #[test]
    fn ascii_degrades_the_unicode_level() {
        let f = resolve_fidelity(&cli(&["--ascii"]), caps(true, ColorDepth::TrueColor, 80));
        assert_eq!(f.unicode, UnicodeLevel::Ascii);
        assert_eq!(
            f.color,
            ColorDepth::TrueColor,
            "--ascii does not touch color"
        );
    }

    #[test]
    fn the_plain_backend_turns_off_color_and_links() {
        let f = resolve_fidelity(
            &cli(&["-t", "plain"]),
            caps(true, ColorDepth::TrueColor, 80),
        );
        assert_eq!(f.color, ColorDepth::None);
        assert!(!f.hyperlinks);
    }

    #[test]
    fn the_terminal_width_is_capped_for_legibility() {
        assert_eq!(
            resolve_width(&cli(&[]), caps(true, ColorDepth::Ansi16, 300)),
            MAX_COMFORTABLE_WIDTH
        );
    }

    #[test]
    fn an_explicit_width_is_not_capped() {
        // If the user asks for 300, they want 300.
        assert_eq!(
            resolve_width(
                &cli(&["--width", "300"]),
                caps(true, ColorDepth::Ansi16, 80)
            ),
            300
        );
    }

    #[test]
    fn the_width_is_never_zero() {
        assert_eq!(
            resolve_width(&cli(&["--width", "0"]), caps(true, ColorDepth::Ansi16, 80)),
            1
        );
        assert_eq!(
            resolve_width(&cli(&[]), caps(false, ColorDepth::None, 0)),
            1
        );
    }

    #[test]
    fn an_unknown_from_is_a_usage_error() {
        let registry = build_registry();
        let src = Source::from_bytes("t", "x");
        let mut src = src;
        let err =
            resolve_format(&cli(&["--from", "nonexistent"]), &registry, &mut src).unwrap_err();
        assert_eq!(err.exit_code(), exit::USAGE);
        // The message must list what is valid: an error with no way out is useless.
        assert!(err.to_string().contains("markdown"), "{err}");
    }

    #[test]
    fn a_valid_from_overrides_detection() {
        let registry = build_registry();
        // Content that would be detected as Markdown.
        let src = Source::from_bytes("t", "# Title\n");
        let mut src = src;
        let f = resolve_format(&cli(&["--from", "text"]), &registry, &mut src).unwrap();
        assert_eq!(f, FormatId::PlainText);
    }
}
