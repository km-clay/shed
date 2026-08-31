use crate::util::pos::Pos;

use super::{Grapheme, Lines};

#[derive(Default, PartialEq, Eq, Debug, Clone, Copy)]
pub enum CharClass {
  #[default]
  Alphanum,
  Symbol,
  Whitespace,
  Other,
}

impl CharClass {
  pub fn is_other_class(self, other: CharClass) -> bool {
    !self.eq(&other)
  }
  pub fn is_other_class_or_ws(self, other: CharClass) -> bool {
    if self.is_ws() || other.is_ws() {
      true
    } else {
      self.is_other_class(other)
    }
  }
  pub fn is_ws(self) -> bool {
    self == CharClass::Whitespace
  }
}

impl From<&Grapheme> for CharClass {
  fn from(g: &Grapheme) -> Self {
    match g {
      Grapheme::Single(ch) => {
        if ch.is_alphanumeric() || ch == &'_' {
          CharClass::Alphanum
        } else if ch.is_whitespace() {
          CharClass::Whitespace
        } else if !ch.is_alphanumeric() {
          CharClass::Symbol
        } else {
          CharClass::Other
        }
      }
      Grapheme::Cluster(cluster) => {
        let Some(first) = cluster.chars().next() else {
          return Self::Other;
        };
        if first.is_alphanumeric()
          && cluster[1..]
            .chars()
            .all(|c| c.is_ascii_punctuation() || c == '\u{0301}' || c == '\u{0308}')
        {
          // Handles things like `ï`, `é`, etc., by manually allowing common diacritics
          return CharClass::Alphanum;
        }

        if cluster.chars().all(|c| c.is_alphanumeric() || c == '_') {
          CharClass::Alphanum
        } else if cluster.chars().all(char::is_whitespace) {
          CharClass::Whitespace
        } else if cluster.chars().all(|c| !c.is_alphanumeric()) {
          CharClass::Symbol
        } else {
          CharClass::Other
        }
      }
    }
  }
}

pub(super) struct CharClassIter<'a> {
  lines: &'a Lines,
  row: usize,
  col: usize,
  exhausted: bool,
  at_boundary: bool,
}

impl<'a> CharClassIter<'a> {
  pub fn new(lines: &'a Lines, start_pos: Pos) -> Self {
    Self {
      lines,
      row: start_pos.row,
      col: start_pos.col,
      exhausted: false,
      at_boundary: false,
    }
  }
  fn get_pos(&self) -> Pos {
    Pos {
      row: self.row,
      col: self.col,
    }
  }
}

impl Iterator for CharClassIter<'_> {
  type Item = (Pos, CharClass);
  fn next(&mut self) -> Option<(Pos, CharClass)> {
    if self.exhausted {
      return None;
    }

    // Synthetic whitespace for line boundary
    if self.at_boundary {
      self.at_boundary = false;
      let pos = self.get_pos();
      return Some((pos, CharClass::Whitespace));
    }

    if self.row >= self.lines.len() {
      self.exhausted = true;
      return None;
    }

    if self.row >= self.lines.len() {
      self.exhausted = true;
      return None;
    }

    let line = &self.lines[self.row];
    // Empty line = whitespace
    if line.is_empty() {
      let pos = Pos {
        row: self.row,
        col: 0,
      };
      self.row += 1;
      self.col = 0;
      return Some((pos, CharClass::Whitespace));
    }

    if self.col >= line.len() {
      self.row += 1;
      self.col = 0;
      self.at_boundary = self.row < self.lines.len();
      return self.next();
    }

    let pos = self.get_pos();
    let class = line[self.col].class();

    self.col += 1;
    if self.col >= line.len() {
      self.row += 1;
      self.col = 0;
      self.at_boundary = self.row < self.lines.len();
    }

    Some((pos, class))
  }
}

pub(super) struct CharClassIterRev<'a> {
  lines: &'a Lines,
  row: usize,
  col: usize,
  exhausted: bool,
  at_boundary: bool,
}

impl<'a> CharClassIterRev<'a> {
  pub fn new(lines: &'a Lines, start_pos: Pos) -> Self {
    let row = start_pos.row.min(lines.len().saturating_sub(1));
    let col = if lines.is_empty() || lines[row].is_empty() {
      0
    } else {
      start_pos.col.min(lines[row].len().saturating_sub(1))
    };
    Self {
      lines,
      row,
      col,
      exhausted: false,
      at_boundary: false,
    }
  }
  fn get_pos(&self) -> Pos {
    Pos {
      row: self.row,
      col: self.col,
    }
  }
}

impl Iterator for CharClassIterRev<'_> {
  type Item = (Pos, CharClass);
  fn next(&mut self) -> Option<(Pos, CharClass)> {
    if self.exhausted {
      return None;
    }

    // Synthetic whitespace for line boundary
    if self.at_boundary {
      self.at_boundary = false;
      let pos = self.get_pos();
      return Some((pos, CharClass::Whitespace));
    }

    if self.row >= self.lines.len() {
      self.exhausted = true;
      return None;
    }

    let line = &self.lines[self.row];
    // Empty line = whitespace
    if line.is_empty() {
      let pos = Pos {
        row: self.row,
        col: 0,
      };
      if self.row == 0 {
        self.exhausted = true;
      } else {
        self.row -= 1;
        self.col = self.lines[self.row].len().saturating_sub(1);
      }
      return Some((pos, CharClass::Whitespace));
    }

    let pos = self.get_pos();
    let class = line[self.col].class();

    if self.col == 0 {
      if self.row == 0 {
        self.exhausted = true;
      } else {
        self.row -= 1;
        self.col = self.lines[self.row].len().saturating_sub(1);
        self.at_boundary = true;
      }
    } else {
      self.col -= 1;
    }

    Some((pos, class))
  }
}
