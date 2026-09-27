//! Quote-state tracking for escape/quote-aware scanning.

/// Used to track whether the lexer is currently inside a quote, and if so, which type
#[derive(Default, Copy, Debug, PartialEq, Clone)]
pub(crate) enum QuoteState {
  #[default]
  Outside,
  Single,
  Double,
}

impl QuoteState {
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
  /// Toggles whether we are in a double quote. If self = `QuoteState::Single` or `QuoteState::Backtick,` this does nothing, since double quotes inside those quotes are just literal characters
  pub(crate) fn toggle_double(&mut self) {
    match self {
      QuoteState::Outside => *self = QuoteState::Double,
      QuoteState::Double => *self = QuoteState::Outside,
      QuoteState::Single => {}
    }
  }
  /// Toggles whether we are in a single quote. If self == `QuoteState::Double` or `QuoteState::Backtick,` this does nothing, since single quotes inside those quotes are just literal characters
  pub(crate) fn toggle_single(&mut self) {
    match self {
      QuoteState::Outside => *self = QuoteState::Single,
      QuoteState::Single => *self = QuoteState::Outside,
      QuoteState::Double => {}
    }
  }
}
