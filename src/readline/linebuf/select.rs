use std::ops::Range;

use super::{
  Pos, SignedPos,
  editcmd::{LineAddr, Motion},
  ordered,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SelectMode {
  Char(Pos),
  Line(Pos),
  Block(Pos),
}

impl SelectMode {
  pub fn shape(&self, other: Pos) -> SelectShape {
    match self {
      SelectMode::Char(pos) => {
        let (s, e) = ordered(*pos, other);
        // offset points from lower end (s) to upper end (e) - always non-negative
        SelectShape::Char(e.difference(&s))
      }
      SelectMode::Line(pos) => {
        let (s, e) = ordered(*pos, other);
        SelectShape::Line(e.difference(&s))
      }
      SelectMode::Block(pos) => {
        let (s, e) = ordered(*pos, other);
        SelectShape::Block(e.difference(&s))
      }
    }
  }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SelectShape {
  Char(SignedPos),
  Line(SignedPos),
  Block(SignedPos),
}

impl SelectShape {
  pub fn pos(&self) -> SignedPos {
    match self {
      SelectShape::Char(pos) | SelectShape::Line(pos) | SelectShape::Block(pos) => *pos,
    }
  }

  pub fn into_select_mode(self, resolved: Pos) -> SelectMode {
    match self {
      SelectShape::Char(_) => SelectMode::Char(resolved),
      SelectShape::Line(_) => SelectMode::Line(resolved),
      SelectShape::Block(_) => SelectMode::Block(resolved),
    }
  }
}

impl super::LineBuf {
  pub fn start_char_select(&mut self) {
    self.select_mode = Some(SelectMode::Char(self.cursor.pos));
  }

  pub fn start_line_select(&mut self) {
    self.select_mode = Some(SelectMode::Line(self.cursor.pos));
  }

  pub fn set_select_anchor(&mut self, pos: Pos) {
    if let Some(mode) = self.select_mode.as_mut() {
      match mode {
        SelectMode::Char(a) | SelectMode::Line(a) | SelectMode::Block(a) => *a = pos,
      }
    }
  }

  pub fn stop_selecting(&mut self) {
    if self.select_mode.is_some() {
      self.last_selection = self.select_mode.map(|m| {
        let anchor = match m {
          SelectMode::Char(a) | SelectMode::Block(a) | SelectMode::Line(a) => a,
        };
        (m, anchor)
      });
    }
    self.select_mode = None;
  }

  pub fn select_range(&self) -> Option<Motion> {
    let mode = self.select_mode.as_ref()?;
    Some(self.evaluate_selection(mode))
  }

  pub fn selection_str(&self) -> Option<String> {
    let (start, end) = match self.select_range()? {
      Motion::CharRange(s, e) => {
        let (s, e) = ordered(s, e);
        // exclusive end: include the grapheme under the end cursor if present
        let end = if e.col < self.line(e.row).len() {
          Pos {
            row: e.row,
            col: e.col + 1,
          }
        } else {
          e
        };
        (s, end)
      }
      Motion::LineRange(s, e) => {
        let s = self.resolve_line_addr(&s).ok()??;
        let e = self.resolve_line_addr(&e).ok()??;
        let (s, e) = ordered(s, e);
        (
          Pos { row: s, col: 0 },
          Pos {
            row: e,
            col: self.line(e).len(),
          },
        )
      }
      Motion::BlockRange(..) => return None,
      _ => unreachable!(),
    };
    Some(self.pos_slice_exclusive(start, end))
  }

  pub fn select_range_byte_pos(&mut self) -> Option<Range<usize>> {
    match self.select_range()? {
      Motion::CharRange(s, e) => {
        let (s, e) = ordered(s, e);
        let start = self.pos_to_byte(s)?;
        let mut end = self.pos_to_byte(e)?;
        // Charwise selections are inclusive of the grapheme under the cursor,
        // matching how the operator path treats a CharRange motion.
        if let Some(gr) = self.lines.get(e.row).and_then(|line| line.0.get(e.col)) {
          end += gr.len_utf8();
        }
        Some(start..end)
      }
      Motion::LineRange(s, e) => {
        let s = self.resolve_line_addr(&s).ok()??;
        let e = self.resolve_line_addr(&e).ok()??;
        let s = self.pos_to_byte(Pos { row: s, col: 0 });
        let e = self.pos_to_byte(Pos {
          row: e,
          col: self.lines[e].len(),
        });
        let (s, e) = ordered(s?, e?);
        Some(s..e)
      }
      Motion::BlockRange(..) => todo!(),
      _ => unreachable!(),
    }
  }

  pub fn evaluate_selection(&self, mode: &SelectMode) -> Motion {
    match mode {
      SelectMode::Char(pos) => {
        let (s, e) = ordered(self.cursor.pos, *pos);
        Motion::CharRange(s, e)
      }
      SelectMode::Line(pos) => {
        let (s, e) = ordered(self.row() + 1, pos.row + 1);
        Motion::LineRange(LineAddr::Number(s), LineAddr::Number(e))
      }
      SelectMode::Block(pos) => {
        let (s, e) = ordered(self.cursor.pos, *pos);
        Motion::BlockRange(s, e)
      }
    }
  }

  pub fn evaluate_select_shape(&self, shape: &SelectShape) -> Motion {
    let offset = shape.pos();
    let anchor = self.cursor.pos.add_signed(offset);
    assert!(anchor > self.cursor.pos);
    let mode = shape.into_select_mode(anchor);
    self.evaluate_selection(&mode)
  }

  pub fn select_mode(&self) -> Option<Motion> {
    self
      .select_mode
      .as_ref()
      .map(|m| Motion::Selection(m.shape(self.cursor.pos)))
  }

  pub fn is_selecting(&self) -> bool {
    self.select_mode.is_some()
  }
}
