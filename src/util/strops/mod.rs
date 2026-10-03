//! Shell-aware byte-string utilities
//!
//! escape/quote-aware splitting and delimiter scanning, byte cursors, size/time/mode
//! parsing and formatting, natural-language time parsing, and edit distance.

mod convert;
mod distance;
mod format;
mod num;
mod quote;
mod scan;

pub(crate) use convert::{
  TimeReader, apply_mode_clauses, dur_delta, format_mode, format_size, format_time,
  parse_mode_clauses, parse_paren_strftime, parse_size, strftime,
};
pub(crate) use distance::{EDIT_WEIGHT, levenshtein};
pub(crate) use format::{
  Base, Case, Count, Field, FieldParams, FmtFlags, Sign, StrFmt, StrFormatter,
};
pub(crate) use num::{ParseRadix, VarStrDisplay};
pub(crate) use quote::QuoteState;
pub(crate) use scan::{
  ByteCursor, SliceCursor, ends_with_unescaped, has_unescaped, scan_brackets, scan_param_exp,
  scan_parens, split_all_with, split_assignment_raw, split_at_unescaped, split_tk,
};
