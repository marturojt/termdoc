//! termdoc's output backends.
//!
//! A backend receives a `Line` —already laid out, with widths resolved— and turns it into
//! bytes. It does not know what format the document had or which reader produced it, and
//! that ignorance is what allows formats and backends to be added independently.
//!
//! LAYERING INVARIANT: depends on `core` and `term`. It sees neither readers nor the
//! layout engine.

#![warn(missing_debug_implementations)]

mod ansi;
mod color;
mod plain;

pub use ansi::AnsiBackend;
pub use color::{bg_code, fg_code, indexed_to_named, rgb_to_256, rgb_to_named};
pub use plain::PlainBackend;

use termdoc_term::{ColorDepth, Fidelity};

/// The backend to use for the detected capabilities.
///
/// Without color there is no point paying for the ANSI backend: the plain one produces
/// exactly the same bytes with less work.
pub fn for_fidelity(fidelity: Fidelity) -> Box<dyn termdoc_core::Backend> {
    if fidelity.color == ColorDepth::None && !fidelity.hyperlinks {
        Box::new(PlainBackend::new())
    } else {
        Box::new(AnsiBackend::new(fidelity))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_capabilities_selects_plain_text() {
        assert!(!for_fidelity(Fidelity::PLAIN).caps().styled);
    }

    #[test]
    fn truecolor_selects_ansi() {
        assert!(for_fidelity(Fidelity::FULL).caps().styled);
    }
}
