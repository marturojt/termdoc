//! termdoc's layout engine.
//!
//! This is the stage the original pipeline was missing, and where the hard work lives:
//! Unicode-aware wrapping, table column allocation, list indentation, and choosing glyphs
//! based on what the terminal supports.
//!
//! Sharing it is what makes **every** format inherit good tables and good wrapping without
//! reimplementing them. A reader only describes the document; deciding how that looks in 40
//! columns is this crate's business.
//!
//! LAYERING INVARIANT: depends on `core` and `term`. It knows about no backend and no
//! reader.

#![warn(missing_debug_implementations)]

mod engine;
mod glyphs;
mod table;
mod theme;
mod wrap;

pub use engine::{Layout, LayoutOptions, glyphs_for};
pub use glyphs::{ASCII, Glyphs, UNICODE};
pub use table::TableBuilder;
pub use theme::Theme;
pub use wrap::{Part, TAB_WIDTH, WrapBuffer, display_width, hard_wrap, hard_wrap_parts};
