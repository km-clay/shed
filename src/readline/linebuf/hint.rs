use std::cmp::Ordering;

use crate::readline::{highlight, linebuf::Line};

use super::{Lines, Pos, shopt};

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) enum Hint {
  Override(Lines),
  History(Lines),
  /// `token_start` is the byte offset (into the flat buffer) where the
  /// completed token began. Used to detect when the user has backspaced
  /// past the token boundary, at which point the hint is contextually
  /// stale even if the buffer still happens to be a literal prefix of
  /// the hint text.
  Completion {
    lines: Lines,
    token_start: usize,
  },
}

impl Hint {
  pub fn lines(&self) -> &Lines {
    match self {
      Self::Override(lines) | Self::History(lines) | Self::Completion { lines, .. } => lines,
    }
  }
  pub fn raw(&self) -> String {
    self.lines().join()
  }
  pub fn take_lines(&mut self) -> Lines {
    match self {
      Self::Override(lines) | Self::History(lines) | Self::Completion { lines, .. } => {
        std::mem::take(lines)
      }
    }
  }
  pub fn display(&self, prefix: Option<&str>) -> String {
    let mut text = self.raw();
    if let Some(prefix) = prefix
      && let Some(rest) = text.strip_prefix(prefix)
    {
      text = highlight::visualize_controls_str(rest);
    }

    format!("\x1b[90m{text}\x1b[0m").replace('\n', "\n\x1b[90m")
  }
  pub fn is_empty(&self) -> bool {
    self.lines().is_empty() || (self.lines().len() == 1 && self.lines()[0].is_empty())
  }
}

impl PartialOrd for Hint {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

impl Ord for Hint {
  /// Defines priority for hint types.
  ///
  /// If a new hint would overwrite an old hint, the 'lesser' hint loses
  ///
  /// 'greater' hints and 'equal' hints both overwrite.
  fn cmp(&self, other: &Self) -> Ordering {
    match self {
      Self::Override(_) => {
        if matches!(other, Self::Override(_)) {
          Ordering::Equal
        } else {
          Ordering::Greater
        }
      }
      Self::History(_) => match other {
        Self::Override(_) => Ordering::Less,
        Self::History(_) => Ordering::Equal,
        Self::Completion { .. } => Ordering::Greater,
      },
      Self::Completion { .. } => {
        if matches!(other, Self::Completion { .. }) {
          Ordering::Equal
        } else {
          Ordering::Less
        }
      }
    }
  }
}

impl super::LineBuf {
  /// Perform an operation that incrementally accepts the hint if the cursor moves into it
  ///
  /// Process:
  /// * take the lines out of self.hint directly
  /// * mark end of buffer position, append hint lines to self.lines
  /// * call the function
  /// * split the buffer at `old_end_pos.max(new_cursor_pos)`
  ///
  /// Notes:
  /// * The size of the hint can never grow as a result of this function. It will only ever stay the same size or shrink.
  pub fn with_hint<F, T>(&mut self, f: F) -> T
  where
    F: FnOnce(&mut Self) -> T,
  {
    let Some(hint) = self.hint.as_mut() else {
      return f(self);
    };
    let last_row = self.lines.len().saturating_sub(1);

    // find end of the buffer, start of the hint
    let first_hint_pos = Pos::new(last_row, self.lines.get(last_row).map_or(0, Line::len));

    // replace our buffer with the full hint
    self.lines = hint.lines().clone();

    // track old/new cursor position
    let old_cursor_pos = self.cursor.pos;
    let result = f(self); // do our operation
    let new_cursor_pos = self.cursor.pos;

    // figure out if we moved into the hint
    let is_past_end = if self.cursor.exclusive {
      new_cursor_pos >= first_hint_pos
    } else {
      new_cursor_pos > first_hint_pos
    };

    // figure out where to split the buffer
    let split_pos = if new_cursor_pos > old_cursor_pos && is_past_end {
      // our cursor moved into the hint
      // so we split on the cursor's new position
      let old_len = self.count_graphemes();
      if shopt!(prompt.expand_aliases) {
        self.attempt_alias_expansion();
      }
      let new_len = self.count_graphemes();
      let delta = new_len as isize - old_len as isize;

      if self.cursor.exclusive {
        new_cursor_pos.col_add_signed(delta + 1)
      } else {
        new_cursor_pos.col_add_signed(delta)
      }
    } else {
      // the cursor did not move into the hint
      // so we split on the hint's first grapheme
      first_hint_pos
    };

    // do the split
    // hint only changes if it becomes empty.
    // buffer changes to end at the cursor's new position.
    let hint_lines = self.lines.split_lines_at(split_pos);
    if hint_lines.is_empty() {
      self.hint = None;
    }

    result
  }

  pub fn clear_hint(&mut self) {
    self.hint = None;
  }

  pub fn set_hint(&mut self, hint: Option<Hint>) {
    let is_override = matches!(&hint, Some(Hint::Override(_)));

    // Override hints are explicit (typically socket-driven), so they
    // bypass the empty-buffer and auto_suggest gates that normally
    // suppress hints. Other hints are heuristic and respect both.
    if !is_override && self.is_empty() {
      self.hint = None;
      return;
    }

    let Some(hint) = hint else {
      let keep = self.hint.as_ref().is_some_and(|h| match h {
        Hint::Override(_) => self.lines.is_prefix_lines(h.lines()),
        Hint::Completion { lines, token_start } => {
          self.lines.is_prefix_lines(lines) && self.to_string().len() > *token_start
        }
        Hint::History(_) => false,
      });
      if !keep {
        self.hint = None;
      }
      return;
    };

    if !is_override && !self.lines.is_prefix_lines(hint.lines()) {
      return;
    }

    if let Some(old_hint) = self.hint.as_ref()
      && *old_hint > hint
    {
      // order comparisons on hints are priority checks
      // if older hint has higher priority, keep it instead of replacing with lower-priority new hint
      return;
    }

    if !is_override && !shopt!(line.auto_suggest) {
      self.hint = None;
      return;
    }

    self.hint = (!hint.is_empty()).then_some(hint);
  }

  pub fn has_hint(&self) -> bool {
    self
      .hint
      .as_ref()
      .is_some_and(|h| !h.lines().is_empty() && h.lines().iter().any(|l| !l.is_empty()))
  }

  pub fn get_hint_text(&self) -> String {
    self.try_get_hint_text().unwrap_or_default()
  }

  pub fn try_get_hint_text(&self) -> Option<String> {
    self
      .hint
      .as_ref()
      .map(|h| h.display(Some(&self.to_string())))
  }

  pub fn try_join_hint(&self) -> Option<String> {
    self.hint.as_ref().map(Hint::raw)
  }

  pub fn accept_hint(&mut self) {
    let Some(mut hint) = self.hint.take() else {
      return;
    };
    let Some(mut hint_lines) = hint.take_lines().strip_prefix_lines(&self.lines) else {
      return;
    };
    self.lines.attach_lines(&mut hint_lines);
    if shopt!(prompt.expand_aliases) {
      self.attempt_alias_expansion_all();
    }

    self.set_cursor(Pos::MAX);
    self.fix_cursor();
  }
}
