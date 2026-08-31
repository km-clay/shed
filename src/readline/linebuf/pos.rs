use super::Pos;

#[derive(Debug, Clone)]
pub(crate) enum MotionKind {
  /// A flat range from one grapheme position to another
  /// `start` is not necessarily less than `end`. `start` in most cases
  /// is the cursor's position.
  Char {
    start: Pos,
    end: Pos,
    inclusive: bool,
  },
  /// A range of whole lines.
  Line {
    start: usize,
    end: usize,
    inclusive: bool,
  },
  Block {
    start: Pos,
    end: Pos,
  },
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Cursor {
  pub pos: Pos,
  pub exclusive: bool,
}
