//! Quote-state tracking for escape/quote-aware scanning.

use crate::expand::stream::Quote;

impl From<u8> for QuoteState {
  fn from(value: u8) -> Self {
    match value {
      b'\'' => QuoteState::Single,
      b'"'  => QuoteState::Double,
      _     => panic!("char::as_quote_state called on non-quote character: {value:?}"),
    }
  }
}
impl From<char> for QuoteState {
  fn from(value: char) -> Self {
    Self::from(value as u8)
  }
}

impl From<Quote> for QuoteState {
  fn from(value: Quote) -> Self {
    match value {
      Quote::Single => QuoteState::Single,
      Quote::Double => QuoteState::Double,
    }
  }
}

/// Used to track whether the lexer is currently inside a quote, and if so, which type
#[derive(Default, Copy, Debug, PartialEq, Clone)]
pub(crate) enum QuoteState {
  #[default]
  Outside,
  Single,
  Double,
}

impl QuoteState {
  pub(crate) fn reset(&mut self) {
    *self = QuoteState::Outside;
  }
  pub(crate) fn outside(self) -> bool {
    matches!(self, QuoteState::Outside)
  }
  pub(crate) fn in_single(self) -> bool {
    matches!(self, QuoteState::Single)
  }
  pub(crate) fn in_double(self) -> bool {
    matches!(self, QuoteState::Double)
  }
  pub(crate) fn in_quote(self) -> bool {
    !self.outside()
  }
  pub(crate) fn toggle_from<Q: Into<QuoteState>>(&mut self, state: Q) -> bool {
    match state.into() {
      QuoteState::Single  => self.toggle_single(),
      QuoteState::Double  => self.toggle_double(),
      QuoteState::Outside => false,
    }
  }
  /// Toggles whether we are in a double quote. If self = `QuoteState::Single` or `QuoteState::Backtick,` this does nothing, since double quotes inside those quotes are just literal characters
  pub(crate) fn toggle_double(&mut self) -> bool {
    match self {
      QuoteState::Outside => *self = QuoteState::Double,
      QuoteState::Double  => *self = QuoteState::Outside,
      QuoteState::Single  => return false,
    }
    true
  }
  /// Toggles whether we are in a single quote. If self == `QuoteState::Double` or `QuoteState::Backtick,` this does nothing, since single quotes inside those quotes are just literal characters
  pub(crate) fn toggle_single(&mut self) -> bool {
    match self {
      QuoteState::Outside => *self = QuoteState::Single,
      QuoteState::Single  => *self = QuoteState::Outside,
      QuoteState::Double  => return false,
    }
    true
  }
}
