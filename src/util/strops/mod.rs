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
  FromBytes, ParseBytes, TimeReader, apply_mode_clauses, dur_delta, format_mode, format_size,
  format_time, human_size, parse_mode_clauses, parse_paren_strftime, parse_size, strftime,
};
pub(crate) use distance::{EDIT_WEIGHT, levenshtein};
pub(crate) use format::{
  Base, Case, Count, Field, FieldParams, FmtFlags, Sign, StrFmt, StrFormatter,
};
pub(crate) use num::{Endian, FloatSpec, IntSpec, ParseRadix, ShStrDisplay};
pub(crate) use quote::QuoteState;
pub(crate) use scan::{
  ByteCursor, ByteSet, QuotePolicy, SliceCursor, assignment_value, ends_with_unescaped,
  has_unescaped, ifs_split, is_array_literal_assignment, scan_brackets, scan_param_exp,
  scan_paren_body, scan_parens, split_all_with, split_assignment, split_assignment_raw,
  split_at_byteset_with, split_at_pat_with, split_at_unescaped, split_tk,
};

/// Folds a slice into an accumulator. Pushes a space (`b' '`) if `i != 0`.
///
/// Used with enumerated iterators and `.fold()` to join a list of byte slices into a single `Vec<u8>`.
pub(crate) fn fold_bytes(mut acc: Vec<u8>, (i, slice): (usize, impl AsRef<[u8]>)) -> Vec<u8> {
  if i != 0 {
    acc.push(b' ');
  }
  acc.extend_from_slice(slice.as_ref());
  acc
}
