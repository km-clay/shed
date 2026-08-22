use nix::{libc::STDIN_FILENO, unistd::isatty};
use std::fmt::{Display, Write};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

use crate::readline::editmode::AddressRange;
use crate::readline::linebuf::HighlightCache;
use crate::readline::linebuf::edit::{
  depth_levels_from_tokens, depth_levels_via_ctx, parse_failed_strict,
};
use crate::state::vars::VarStr;
use crate::util;

use super::types::Grapheme;
use super::{
  CharClass, DEFAULT_VIEWPORT_HEIGHT, Edit, Line, Lines, MotionKind, Pos, SelectMode, ShResult,
  Shed, editcmd::Motion, eval::lex, highlight, ordered, procio::stdin_fileno, sherr, shopt,
  state::terminal::get_win_size, status_msg,
};

use super::char_class::{CharClassIter, CharClassIterRev};
use super::edit::extract_range_contiguous;

pub fn rot13_char(c: char) -> char {
  let offset = if c.is_ascii_lowercase() {
    b'a'
  } else if c.is_ascii_uppercase() {
    b'A'
  } else {
    return c;
  };
  (((c as u8 - offset + 13) % 26) + offset) as char
}

pub fn toggle_case_char(c: char) -> char {
  if c.is_ascii_lowercase() {
    c.to_ascii_uppercase()
  } else if c.is_ascii_uppercase() {
    c.to_ascii_lowercase()
  } else {
    c
  }
}

pub(super) struct Diff {
  start: usize,
  end_old: usize,
  end_new: usize,
}

impl super::LineBuf {
  pub fn mark_insert_mode_start_pos(&mut self) {
    self.insert_mode_start_pos = Some(self.cursor.pos);
  }

  pub fn clear_insert_mode_start_pos(&mut self) {
    self.insert_mode_start_pos = None;
  }

  pub(super) fn replace_range(&mut self, span: (Pos, Pos), new: &str) -> Lines {
    let s = span.0;
    let e = span.1;
    let motion = MotionKind::Char {
      start: s,
      end: e,
      inclusive: true,
    };
    let content = self.extract_range(&motion);
    let row = s.row.min(self.lines.len().saturating_sub(1));
    let col = s.col.min(self.lines[row].len());
    self.cursor.pos = Pos { row, col };
    self.insert_str(new);
    content
  }
  pub fn set_viewport_cap(&mut self, cap: Option<usize>) {
    self.viewport_cap = cap;
  }

  pub fn get_viewport_height(&self) -> usize {
    let raw = Shed::shopts(|o| {
      let height = o.line.viewport_height.as_str();
      if let Ok(num) = height.parse::<usize>() {
        num
      } else if let Some(pre) = height.strip_suffix('%')
        && let Ok(num) = pre.parse::<usize>()
      {
        if !isatty(stdin_fileno()).unwrap_or_default() {
          return DEFAULT_VIEWPORT_HEIGHT;
        }
        let (_, rows) = get_win_size(STDIN_FILENO);
        (f64::from(rows) * (num as f64 / 100.0)).round() as usize
      } else {
        log::warn!(
          "Invalid viewport height shopt value: '{height}', using 50% of terminal height as default",
        );
        if !isatty(stdin_fileno()).unwrap_or_default() {
          return DEFAULT_VIEWPORT_HEIGHT;
        }
        let (_, rows) = get_win_size(STDIN_FILENO);
        (f64::from(rows) * 0.5).round() as usize
      }
    });
    let buf_lines = self.lines.len();
    let hint_lines = self.hint.as_ref().map_or(0, |h| h.lines().len());
    let combined = if hint_lines == 0 {
      buf_lines
    } else {
      buf_lines + hint_lines
    };
    let mut out = (raw.min(100)).min(combined);
    if let Some(cap) = self.viewport_cap {
      out = out.min(cap);
    }
    out.max(1)
  }
  pub fn update_scroll_offset(&mut self) {
    let height = self.get_viewport_height();
    let scrolloff = shopt!(line.scroll_offset);
    if self.cursor.pos.row < self.scroll_offset + scrolloff {
      self.scroll_offset = self.cursor.pos.row.saturating_sub(scrolloff);
    }
    if self.cursor.pos.row + scrolloff >= self.scroll_offset + height {
      self.scroll_offset = self.cursor.pos.row + scrolloff + 1 - height;
    }

    let max_offset = self.lines.len().saturating_sub(height);
    self.scroll_offset = self.scroll_offset.min(max_offset);
  }

  fn refresh_highlight_cache(&mut self, joined: &str) {
    use std::hash::{Hash, Hasher};
    // Incremental relexing only pays off on large buffers, where a full relex
    // would be noticeably slow. Below this line count, the full relex is cheap
    // enough that it isn't worth the incremental path's edge cases.
    const INCREMENTAL_MIN_LINES: usize = 200;

    let mut h = std::collections::hash_map::DefaultHasher::new();
    joined.hash(&mut h);
    let new_hash = h.finish();

    // Cache hit. buffer unchanged since last lex.
    if self
      .highlight_cache
      .as_ref()
      .is_some_and(|c| c.hash == new_hash)
    {
      return;
    }

    // Cold start or post-bailout. full lex
    let Some(_existing) = self.highlight_cache.as_ref() else {
      let tokens = crate::readline::context::get_context_tokens(joined);
      self.highlight_cache = Some(super::HighlightCache {
        joined: joined.to_string(),
        hash: new_hash,
        tokens,
      });
      return;
    };

    // Existing cache + buffer changed. On large buffers, try the incremental
    // path; otherwise fall through to a full relex.
    if self.lines.len() >= INCREMENTAL_MIN_LINES
      && let Some(cache) = self.try_incremental_relex(joined, new_hash)
    {
      self.highlight_cache = Some(cache);
      return;
    }

    // Small buffer, or incremental bailed out: full relex.
    let tokens = crate::readline::context::get_context_tokens(joined);
    self.highlight_cache = Some(super::HighlightCache {
      joined: joined.to_string(),
      hash: new_hash,
      tokens,
    });
  }

  /// Try a partial relex of the input
  ///
  /// Attempts to lex only the content between the two closest separators (`;`, `\n`, etc)
  fn try_incremental_relex(&mut self, new_joined: &str, new_hash: u64) -> Option<HighlightCache> {
    use crate::eval::lex::Span;
    use crate::readline::context::{CtxTkRule, get_context_tokens};

    let cache = self.highlight_cache.as_ref()?;

    // Diff old vs new to localize the edit. Bail if the change covers
    // basically the whole buffer.
    let Diff {
      start,
      end_old,
      end_new,
    } = find_diff_range(&cache.joined, new_joined);
    let delta = end_new as isize - end_old as isize;
    if start == 0 && end_old == cache.joined.len() {
      return None;
    }

    // find the byte indexes of any surrounding separators
    let left = cache
      .tokens
      .iter()
      .filter(|t| matches!(t.class(), CtxTkRule::Separator) && t.range().end <= start)
      .map(|t| t.range().end)
      .next_back()
      .unwrap_or(0);

    let right_old = cache
      .tokens
      .iter()
      .find(|t| matches!(t.class(), CtxTkRule::Separator) && t.range().start >= end_old)
      .map_or(cache.joined.len(), |t| t.range().start);

    // new separator position
    let right_new = (right_old as isize + delta) as usize;

    if right_new > new_joined.len() || left > right_new {
      return None;
    }

    // "master span", other spans are rebased into this one
    let outer_span = Span::new(0..new_joined.len(), new_joined.into());

    // lex the chunk; rebase its tokens into the full-buffer coord
    // space (offset by `left`) AND onto the outer source.
    let chunk = new_joined.get(left..right_new)?;
    let mut chunk_tokens = get_context_tokens(chunk);
    for t in &mut chunk_tokens {
      t.rebase_into(&outer_span, left);
    } // now we have the new tokens, time to replace the old ones

    let mut cache = self.highlight_cache.take()?;

    // find top-level tokens to replace
    let first = cache
      .tokens
      .iter()
      .position(|t| t.range().start >= left)
      .unwrap_or(cache.tokens.len());
    let last = cache
      .tokens
      .iter()
      .position(|t| t.range().start >= right_old)
      .unwrap_or(cache.tokens.len());

    // shift the spans of each trailing token by the edit delta
    // and then rebase them into the new buffer
    for t in &mut cache.tokens[last..] {
      t.shift_by(delta);
      t.rebase_into(&outer_span, 0);
    }

    // replace the old tokens with the new chunk
    cache.tokens.splice(first..last, chunk_tokens);

    cache.joined = new_joined.to_string();
    cache.hash = new_hash;

    // done
    Some(cache)
  }

  pub fn display_window_joined(&mut self) -> String {
    use std::fmt::Write;
    let mut joined = String::with_capacity(self.lines().byte_len());
    write!(joined, "{self}").ok();

    let do_hl = shopt!(highlight.enable);
    if !do_hl {
      return joined;
    }

    let palette = highlight::Palette::new();
    let mut select_spans = self.search_match_spans();
    select_spans.extend(self.select_range_byte_pos());

    self.refresh_highlight_cache(&joined);
    let Some(cache) = self.highlight_cache.as_ref() else {
      return joined;
    };
    let mut highlighted = String::new();

    highlight::highlight(
      &mut highlighted,
      &joined,
      &cache.tokens,
      &palette,
      self.cursor_to_flat(),
      &select_spans,
    )
    .ok();

    let hint = self.get_hint_text();
    let lines = Lines::to_lines(&format!("{highlighted}{hint}"));

    let offset = self.scroll_offset.min(lines.len());
    let (_, mid) = lines.split_at(offset);

    let height = self.get_viewport_height().min(mid.len());
    let (mid, _) = mid.split_at(height);

    Lines(mid.to_vec()).join()
  }
  pub fn trim_range(&mut self, range: AddressRange, trim_leading: bool) -> ShResult<()> {
    let (start, end) = match range {
      AddressRange::Single(line_addr) => {
        let Some(addr) = self.resolve_line_addr(&line_addr)? else {
          return Ok(());
        };
        util::ordered(addr, addr)
      }
      AddressRange::Range(start, end) => {
        let Some(start) = self.resolve_line_addr(&start)? else {
          return Ok(());
        };
        let Some(end) = self.resolve_line_addr(&end)? else {
          return Ok(());
        };
        util::ordered(start, end)
      }
    };

    // resolve_line_addr returns an inclusive index; make the end exclusive.
    let mut end = (end + 1).min(self.lines.len());

    // Strip trailing whitespace on every line in the range.
    for line in self.lines.iter_mut().take(end).skip(start) {
      while line.0.last().is_some_and(Grapheme::is_ws) {
        line.0.pop();
      }
    }

    // Remove the run of blank lines trailing the range's last content line.
    let mut cut = end;
    while cut > start && self.lines.get(cut - 1).is_some_and(|l| l.0.is_empty()) {
      cut -= 1;
    }
    while self.lines.get(cut).is_some_and(|l| l.0.is_empty()) {
      self.lines.remove(cut);
    }
    end = cut; // the range's content ends here now

    if !trim_leading {
      return Ok(());
    }
    // :trim! has been called

    // lift the common leading-whitespace prefix off the range,
    // keeping relative indentation
    let mut prefix: Option<Vec<Grapheme>> = None;
    for line in self.lines.iter().take(end).skip(start) {
      if line.0.is_empty() {
        continue;
      }
      let lead: Vec<Grapheme> = line.0.iter().take_while(|g| g.is_ws()).cloned().collect();
      prefix = Some(match prefix {
        None => lead,
        Some(prev) => {
          let keep = prev.iter().zip(&lead).take_while(|(a, b)| a == b).count();
          lead.into_iter().take(keep).collect()
        }
      });
      if prefix.as_ref().is_some_and(Vec::is_empty) {
        break;
      }
    }
    if let Some(prefix) = prefix.filter(|p| !p.is_empty()) {
      for line in self.lines.iter_mut().take(end).skip(start) {
        if !line.0.is_empty() {
          line.0.drain(0..prefix.len());
        }
      }
    }

    // remove the run of blank lines leading the range's first content
    // line, extending before the range to the previous content or buffer start.
    let mut first = start;
    while first < end && self.lines.get(first).is_some_and(|l| l.0.is_empty()) {
      first += 1;
    }
    while first > 0 && self.lines.get(first - 1).is_some_and(|l| l.0.is_empty()) {
      self.lines.remove(first - 1);
      first -= 1;
    }

    Ok(())
  }
  pub fn trim(&mut self) {
    // trim empty lines
    while self.lines.first().is_some_and(|l| l.0.is_empty()) {
      self.lines.remove(0);
    }
    while self.lines.last().is_some_and(|l| l.0.is_empty()) {
      self.lines.pop();
    }

    // trim whitespace
    for (i, line) in self.lines.iter_mut().enumerate() {
      if i == 0 && !shopt!(history.ignore_space) {
        while line.0.first().is_some_and(Grapheme::is_ws) {
          line.0.remove(0);
        }
      }
      while line.0.last().is_some_and(Grapheme::is_ws) {
        line.0.pop();
      }
    }

    // trim empty lines again
    while self.lines.first().is_some_and(|l| l.0.is_empty()) {
      self.lines.remove(0);
    }
    while self.lines.last().is_some_and(|l| l.0.is_empty()) {
      self.lines.pop();
    }
  }
  pub fn window_slice_to_cursor(&self) -> String {
    let mut result = String::new();
    let start_row = self.scroll_offset;

    for i in start_row..self.cursor.pos.row {
      write!(result, "{}", self.lines[i]).ok();
      result.push('\n');
    }
    let line = &self.lines[self.cursor.pos.row];
    let col = self.cursor.pos.col.min(line.len());
    for g in &line.graphemes()[..col] {
      write!(result, "{g}").ok();
    }
    result
  }
  pub(super) fn parse_pos(&self, pos: &str) -> ShResult<Pos> {
    if let Some((row, col)) = pos.split_once(':')
      && let Ok(row) = row.parse::<usize>()
      && let Ok(col) = col.parse::<usize>()
    {
      Ok(Pos { row, col })
    } else if let Ok(num) = pos.parse::<usize>() {
      Ok(self.pos_from_flat(num))
    } else {
      Err(sherr!(
        ParseErr,
        "Invalid position format: '{pos}'. Expected 'row:col' or grapheme index.",
      ))
    }
  }
  pub(super) fn insert_lines_at(&mut self, pos: Pos, mut lines: Lines) {
    if lines.is_empty() {
      return;
    }
    let row = pos.row;
    let col = pos.col;

    // Split the current line at the insertion point
    let mut right = self.lines[row].split_off(col);

    let last = lines.len() - 1;

    // First line appends to current line at the split point
    self.lines[row].append(&mut lines[0]);

    // Middle + last lines get inserted after
    for (i, line) in lines.0[1..].iter().cloned().enumerate() {
      self.lines.insert(row + 1 + i, line);
    }

    // Reattach right half to the last inserted line
    self.lines[row + last].append(&mut right);
  }
  pub(super) fn remove_at(&mut self, pos: Pos) -> Option<Grapheme> {
    let Pos { row, col } = pos;
    let line = self.lines.get_mut(row)?;

    line.0.get(col).is_some().then(|| line.0.remove(col))
  }
  pub(super) fn insert_at(&mut self, pos: Pos, gr: Grapheme) {
    if gr.is_lf() {
      self.break_line_at(pos);
      return;
    }

    let row = pos.row;
    let col = pos.col;
    self.lines[row].insert(col, gr);
    self.indent_cache = None;
    let pos = pos.col_add(1);

    // Cheap test first: only consider dedenting if the line's trimmed content
    // is exactly a closer keyword. Skips the depth query for 99% of typing.
    let line = self.cur_line().to_string();
    let trimmed = line.trim();
    let is_closer = lex::CLOSERS
      .iter()
      .chain(lex::MIDDLES.iter())
      .any(|closer| trimmed == *closer);

    if is_closer {
      let (start, end) = self.indent_levels_for_row(pos.row);
      let is_middle = lex::MIDDLES.contains(&trimmed);
      // Align the line with its opener: a real closer pops a block (end < start)
      // and lands at end; a middle (else/elif) is depth-neutral but renders one
      // level out. The absolute target makes a trailing space idempotent.
      let target = if is_middle && start > 0 {
        Some(start - 1)
      } else if !is_middle && end < start {
        Some(end)
      } else {
        None
      };

      if let Some(target) = target {
        let line = self.cur_line_mut();
        let current = line
          .0
          .iter()
          .take_while(|c| c.as_char() == Some('\t'))
          .count();
        for _ in target..current {
          line.0.remove(0);
        }
      }
    }
  }
  pub(super) fn insert(&mut self, gr: Grapheme) {
    self.insert_at(self.cursor.pos, gr);
  }
  pub(super) fn insert_str(&mut self, s: &str) {
    for gr in s.graphemes(true) {
      let gr = Grapheme::from(gr);
      if gr.is_lf() {
        self.break_line_unchecked();
      } else {
        self.insert(gr);
        self.cursor.pos.col += 1;
      }
    }
  }

  /// Insert `s` verbatim with no auto-indent or dedent, in O(n).
  pub(super) fn insert_str_verbatim(&mut self, s: &str) {
    for gr in s.graphemes(true) {
      let gr = Grapheme::from(gr);
      let pos = self.cursor.pos;
      if gr.is_lf() {
        let rest = self.lines[pos.row].split_off(pos.col);
        self.lines.insert(pos.row + 1, rest);
        self.cursor.pos.set(pos.row + 1, 0);
      } else {
        self.lines[pos.row].insert(pos.col, gr);
        self.cursor.pos.col += 1;
      }
    }
    self.indent_cache = None;
  }
  pub fn pop_left(&mut self) -> bool {
    let Some(pos) = self.concat_points.pop_front() else {
      return false;
    };
    self.lines = self.lines.split_lines_at(pos);
    self.fix_cursor();
    true
  }
  pub fn pop_right(&mut self) -> bool {
    let Some(pos) = self.concat_points.pop_back() else {
      return false;
    };
    self.lines.split_lines_at(pos);
    self.fix_cursor();
    true
  }
  pub fn clear_concats(&mut self) {
    self.concat_points.clear();
  }
  /// Concatenate a string onto the left side of the buffer with a separator
  pub fn concat_left(&mut self, sep: &str, other: &str) {
    if self.is_empty() {
      self.lines = Lines::to_lines(other);
      return;
    }
    let joined = self.to_string();
    let Some(first) = self.lines.first_mut() else {
      self.lines = Lines::to_lines(other);
      return;
    };
    let mut new_lines = Lines::to_lines(other);
    if new_lines.is_empty() {
      return;
    }
    while first.0.first().is_some_and(Grapheme::is_ws) {
      first.0.remove(0);
    }
    let Some(new_last) = new_lines.last_mut() else {
      unreachable!()
    };
    if !joined.trim_end().ends_with(sep.trim()) {
      new_last.push_str(sep);
    }
    let mut last = new_lines.pop().unwrap();
    let splice_pos = Pos {
      row: new_lines.len(),
      col: last.len(),
    };
    last.append(first);
    self.lines[0] = last;
    if !new_lines.is_empty() {
      for line in new_lines.0.into_iter().rev() {
        self.lines.insert(0, line);
      }
    }
    self.concat_points.push_front(splice_pos);
  }
  /// Concatenate a string onto the right side of the buffer with a separator
  pub fn concat_right(&mut self, sep: &str, other: &str) {
    if self.is_empty() {
      self.lines = Lines::to_lines(other);
      return;
    }
    let joined = self.to_string();
    let last_row = self.lines.len() - 1;
    let Some(last) = self.lines.last_mut() else {
      self.lines = Lines::to_lines(other);
      return;
    };
    let mut new_lines = Lines::to_lines(other);
    if new_lines.is_empty() {
      return;
    }
    while last.0.last().is_some_and(Grapheme::is_ws) {
      last.0.pop();
    }
    let Some(new_first) = new_lines.first_mut() else {
      unreachable!()
    };
    if !joined.trim_end().ends_with(sep.trim()) {
      new_first.insert_str(0, sep);
    }
    let splice_pos = Pos {
      row: last_row,
      col: last.len(),
    };
    let mut first = new_lines.remove(0);
    last.append(&mut first);
    self.lines.extend(new_lines.0);
    self.concat_points.push_back(splice_pos);
  }
  pub fn cursor_in_leading_ws(&self) -> bool {
    let line = self.line(self.row());
    let col = self.col();

    let only_ws = self
      .lines()
      .iter()
      .all(|ln| ln.0.iter().all(Grapheme::is_ws));

    if only_ws {
      return false;
    }

    line
      .0
      .get(..col)
      .is_none_or(|grs| grs.iter().all(Grapheme::is_ws))
  }

  pub fn cursor_is_escaped(&self) -> bool {
    if self.cursor.pos.col == 0 {
      return false;
    }
    let line = &self.lines[self.cursor.pos.row];
    if self.cursor.pos.col > line.len() {
      return false;
    }
    line
      .graphemes()
      .get(self.cursor.pos.col.saturating_sub(1))
      .is_some_and(|g| g.is_char('\\'))
  }

  pub fn take_buf(&mut self) -> String {
    let result = self.to_string();
    self.lines = Lines::default();
    self.cursor.pos = Pos { row: 0, col: 0 };
    result
  }

  pub(super) fn get(&mut self, pos: Pos) -> Option<Grapheme> {
    self
      .lines
      .get(pos.row)
      .and_then(|line| line.graphemes().get(pos.col))
      .cloned()
  }

  pub(super) fn grapheme_before_cursor(&mut self) -> Option<Grapheme> {
    self.get(self.cursor.pos.col_add_signed(-1))
  }

  pub(crate) fn pos_to_flat(&self, pos: Pos) -> usize {
    let mut offset = 0;
    let row = pos.row.min(self.lines.len().saturating_sub(1));
    for i in 0..row {
      offset += self.lines[i].len() + 1; // +1 for '\n'
    }
    offset + pos.col.min(self.lines[row].len())
  }

  pub(super) fn pos_from_flat(&self, mut flat: usize) -> Pos {
    for (i, line) in self.lines.iter().enumerate() {
      if flat <= line.len() {
        return Pos { row: i, col: flat };
      }
      flat = flat.saturating_sub(line.len() + 1); // +1 for '\n'
    }
    // If we exceed the total length, clamp to end
    let last_row = self.lines.len().saturating_sub(1);
    let last_col = self.lines[last_row].len();
    Pos {
      row: last_row,
      col: last_col,
    }
  }

  pub fn cursor_to_flat(&self) -> usize {
    self.pos_to_flat(self.cursor.pos)
  }

  pub fn anchor_to_flat(&self) -> Option<usize> {
    self.select_mode.map(|r| match r {
      SelectMode::Char(pos) | SelectMode::Block(pos) | SelectMode::Line(pos) => {
        self.pos_to_flat(pos)
      }
    })
  }

  pub fn set_cursor_from_flat(&mut self, flat: usize) {
    self.cursor.pos = self.pos_from_flat(flat);
    self.fix_cursor();
  }
  pub fn set_anchor_from_flat(&mut self, flat: usize) {
    let new_pos = self.pos_from_flat(flat);
    self.set_anchor(new_pos);
  }
  pub fn set_anchor(&mut self, new_pos: Pos) {
    match self.select_mode.as_mut() {
      Some(SelectMode::Line(pos) | SelectMode::Block(pos) | SelectMode::Char(pos)) => {
        *pos = new_pos;
      }
      None => unreachable!(),
    }
  }

  pub fn with_initial(mut self, s: &str, cursor_pos: usize) -> Self {
    self.set_buffer(s);
    // In the flat model, cursor_pos was a flat offset. Map to col on row .
    self.cursor.pos = Pos {
      row: 0,
      col: cursor_pos.min(s.len()),
    };
    self
  }

  pub fn move_cursor_to_end(&mut self) {
    self.set_cursor(Pos::MAX);
  }

  pub fn cursor_max(&self) -> usize {
    // In single-line mode this is the length of the first line
    // In multi-line mode this returns total grapheme count (for flat compat)
    if self.lines.len() == 1 {
      self.lines[0].len()
    } else {
      self.count_graphemes()
    }
  }

  pub fn cursor_at_max(&self) -> bool {
    let last_row = self.lines.len().saturating_sub(1);
    let max = if self.cursor.exclusive {
      self.lines[last_row].len().saturating_sub(1)
    } else {
      self.lines[last_row].len()
    };
    self.cursor.pos.row == last_row && self.cursor.pos.col >= max
  }

  pub fn set_cursor_clamp(&mut self, exclusive: bool) {
    self.cursor.exclusive = exclusive;
  }

  pub fn start_of_line(&self) -> usize {
    // Return 0-based flat offset of start of current row
    let mut offset = 0;
    for i in 0..self.cursor.pos.row {
      offset += self.lines[i].len() + 1; // +1 for '\n'
    }
    offset
  }

  pub fn on_last_line(&self) -> bool {
    self.cursor.pos.row == self.lines.len().saturating_sub(1)
      && self.hint.as_ref().is_none_or(|h| h.lines().len() <= 1)
  }

  pub fn cursor_byte_pos(&self) -> usize {
    let mut pos = 0;
    for i in 0..self.cursor.pos.row {
      pos += self.lines[i].to_string().len() + 1; // +1 for '\n'
    }
    let line_str = self.lines[self.cursor.pos.row].to_string();
    let col = self
      .cursor
      .pos
      .col
      .min(self.lines[self.cursor.pos.row].len());
    // Sum bytes of graphemes up to col
    let mut byte_count = 0;
    for (i, g) in line_str.graphemes(true).enumerate() {
      if i >= col {
        break;
      }
      byte_count += g.len();
    }
    pos + byte_count
  }
  pub fn clear_buffer(&mut self) {
    self.lines = Lines::default();
    self.clear_concats();
    self.fix_cursor();
  }
  pub fn set_buffer(&mut self, s: &str) {
    self.lines = Lines::to_lines(s);
    if self.lines.is_empty() {
      self.lines.push(Line::default());
    }
    self.clear_concats();
    self.fix_cursor();
  }
  pub fn fix_cursor(&mut self) {
    // we are now going to enforce some invariants and do some bookkeeping
    if self.lines.is_empty() {
      // self.lines must always have at least one line
      self.lines.push(Line::default());
    }
    if self.cursor.pos.row >= self.lines.len() {
      // clamp this now so self.cur_line() cannot panic
      self.cursor.pos.row = self.lines.len().saturating_sub(1);
    }
    if self.cursor.exclusive {
      let line = self.cur_line();
      let col = self.col();
      if col > 0 && col >= line.len() {
        self.cursor.pos.col = line.len().saturating_sub(1);
      }
    } else {
      let line = self.cur_line();
      let col = self.col();
      if col > 0 && col > line.len() {
        self.cursor.pos.col = line.len();
      }
    }

    // update viewport scroll offset
    self.update_scroll_offset();
  }
  pub fn stop_undo_merge(&mut self) {
    self.merging_undos = false;
    self.set_top_merging(false);
    // Barrier so the next edit can't fold into the just-closed group.
    self.undo_stack.push(Edit::barrier(self.cursor.pos));
  }
  pub fn start_undo_merge(&mut self) {
    self.merging_undos = true;
    self.set_top_merging(true);
  }
  pub fn equalize_rows(&mut self, line_nums: Vec<usize>) {
    for row in line_nums {
      let (start, end) = self.indent_levels_for_row(row);
      let num_tabs = start.min(end);

      let line = self.line_mut(row);
      while line.0.first().is_some_and(Grapheme::is_ws) {
        line.0.remove(0);
      }
      for tab in std::iter::repeat_n(Grapheme::from('\t'), num_tabs) {
        line.insert(0, tab);
      }
    }
  }
  pub fn indent_levels_for_row(&mut self, row: usize) -> (usize, usize) {
    self.indent_levels().get(row).copied().unwrap_or_default()
  }
  /// Returns (depth-at-cursor, parse-failed). Computed from the prefix
  /// up to the cursor — reflects whether we're inside an open block.
  pub fn cursor_indent_level(&mut self) -> (usize, bool) {
    let (to_cursor, _) = self.lines.clone().split_lines(self.cursor.pos);
    let raw = to_cursor.join();
    let depth = depth_levels_via_ctx(&raw)
      .last()
      .copied()
      .unwrap_or_default()
      .1;
    (depth, parse_failed_strict(&raw))
  }
  pub fn indent_levels(&mut self) -> &[(usize, usize)] {
    if self.indent_cache.is_none() {
      let joined = self.to_string();
      // Reuse the highlighter's token cache (rebuilt only on a buffer change).
      self.refresh_highlight_cache(&joined);
      let levels = self
        .highlight_cache
        .as_ref()
        .map(|c| depth_levels_from_tokens(&c.tokens, &joined))
        .unwrap_or_default();
      self.indent_cache = Some(levels);
    }
    self.indent_cache.as_ref().unwrap()
  }
  pub(super) fn delete_range(&mut self, motion: &MotionKind) -> Lines {
    self.extract_range(motion)
  }
  pub(super) fn yank_range(&self, motion: &MotionKind) -> Lines {
    let mut tmp = Self {
      lines: self.lines.clone(),
      cursor: self.cursor,
      ..Default::default()
    };
    tmp.extract_range(motion)
  }
  pub(super) fn extract_range(&mut self, motion: &MotionKind) -> Lines {
    let extracted = match motion {
      MotionKind::Char {
        start,
        end,
        inclusive,
      } => self.extract_span((*start, *end), *inclusive),
      MotionKind::Line {
        start,
        end,
        inclusive,
      } => {
        let end = if *inclusive {
          *end
        } else {
          end.saturating_sub(1)
        };
        self.lines.drain(*start..=end).collect()
      }
      MotionKind::Block { start, end } => {
        let (s, e) = ordered(*start, *end);
        (s.row..=e.row)
          .map(|row| {
            let sc = s.col.min(self.lines[row].len());
            let ec = (e.col + 1).min(self.lines[row].len());
            Line(self.lines[row].0.drain(sc..ec).collect())
          })
          .collect()
      }
    };
    if self.lines.is_empty() {
      self.lines.push(Line::default());
    }
    extracted
  }
  pub(super) fn yank_span(&self, span: (Pos, Pos), inclusive: bool) -> Lines {
    let mut tmp = Self {
      lines: self.lines.clone(),
      cursor: self.cursor,
      ..Default::default()
    };
    tmp.extract_span(span, inclusive)
  }
  pub(super) fn extract_span(&mut self, span: (Pos, Pos), inclusive: bool) -> Lines {
    let (s, e) = ordered(span.0, span.1);
    let end = if inclusive {
      Pos {
        row: e.row,
        col: e.col + 1,
      }
    } else {
      e
    };
    let mut buf = std::mem::take(&mut self.lines);
    let extracted = extract_range_contiguous(&mut buf, s, end);
    self.lines = buf;
    extracted
  }
  pub(super) fn move_to_start(&mut self, motion: &MotionKind) {
    match motion {
      MotionKind::Char { start, end, .. } => {
        let (s, _) = ordered(start, end);
        self.set_cursor(*s);
      }
      MotionKind::Line { start, end, .. } => {
        let (s, _) = ordered(start, end);
        self.set_cursor(Pos { row: *s, col: 0 });
      }
      MotionKind::Block { .. } => unimplemented!(),
    }
  }
  pub fn get_matching_lines(
    &self,
    constraint: &Motion,
    re: &str,
    negated: bool,
  ) -> ShResult<Vec<usize>> {
    let (s, e) = match constraint {
      Motion::LineRange(s, e) => {
        let Some(s) = self.resolve_line_addr(s)? else {
          return Ok(vec![]);
        };
        let Some(e) = self.resolve_line_addr(e)? else {
          return Ok(vec![]);
        };
        ordered(s, e)
      }
      Motion::Line(addr) => {
        let Some(line) = self.resolve_line_addr(addr)? else {
          return Ok(vec![]);
        };
        (line, line)
      }
      _ => (0, self.lines.len().saturating_sub(1)),
    };

    let re = match Shed::meta_mut(|m| m.get_regex(re)) {
      Ok(re) => re,
      Err(e) => {
        status_msg!("{e}");
        return Ok(vec![]);
      }
    };
    let mut acc = 0;
    let mut lines = vec![];

    loop {
      if !(s..=e).contains(&acc) {
        acc += 1 % self.lines.len();
        continue;
      }
      let Some(line) = self.get_row(acc) else { break };
      let line_str = line.to_string();
      if re.is_match(&line_str) != negated {
        lines.push(acc);
      }

      if acc == self.lines.len().saturating_sub(1) {
        break;
      }
      acc += 1 % self.lines.len();
    }

    Ok(lines)
  }
  pub(super) fn calc_cursor_display_col(&self) -> usize {
    self.calc_display_col_for(self.cursor.pos)
  }
  pub(super) fn calc_display_col_for(&self, pos: Pos) -> usize {
    let tab_width = shopt!(line.tab_width);
    let line = self.line(pos.row);
    let mut col = 0;
    for gr in &line.0[..pos.col] {
      let Some(ch) = gr.as_char() else {
        col += gr.width();
        continue;
      };

      match ch {
        '\t' => {
          col += tab_width - (col % tab_width);
        }
        c => {
          col += c.width().unwrap_or(0);
        }
      }
    }

    col
  }
  pub fn pos_to_byte(&mut self, pos: Pos) -> Option<usize> {
    if let Some(positions) = &self.byte_positions {
      positions
        .iter()
        .find_map(|(b, p)| (*p >= pos).then_some(*b))
    } else {
      self.byte_positions = Some(self.byte_positions());
      self.pos_to_byte(pos)
    }
  }
  pub fn byte_to_pos(&mut self, byte_offset: usize) -> Option<Pos> {
    if let Some(positions) = &self.byte_positions {
      positions
        .iter()
        .find_map(|(b, p)| (*b >= byte_offset).then_some(*p))
    } else {
      self.byte_positions = Some(self.byte_positions());
      self.byte_to_pos(byte_offset)
    }
  }
  /// map every valid Pos in the buffer to a corresponding byte position in the string
  pub(super) fn byte_positions(&self) -> Vec<(usize, Pos)> {
    let mut positions = vec![];
    let mut acc = 0;

    for (row, line) in self.lines.iter().enumerate() {
      for (col, gr) in line.0.iter().enumerate() {
        positions.push((acc, Pos { row, col }));
        acc += gr.len_utf8();
      }
      positions.push((
        acc,
        Pos {
          row,
          col: line.0.len(),
        },
      ));
      acc += 1; // for the newline
    }

    positions
  }
  pub(super) fn display_col_to_index(&self, row: usize, target: usize) -> usize {
    let tab_width = shopt!(line.tab_width);
    let line = self.line(row);
    let mut col = 0;
    for (i, gr) in line.0.iter().enumerate() {
      if col >= target {
        return i;
      }
      let Some(ch) = gr.as_char() else {
        col += gr.width();
        continue;
      };

      match ch {
        '\t' => {
          col += tab_width - (col % tab_width);
        }
        c => {
          col += c.width().unwrap_or(0);
        }
      }
    }

    line.0.len()
  }
  pub(super) fn get_row(&self, row: usize) -> Option<&Line> {
    self.lines.get(row)
  }
  pub fn slice_pos(&self, a: Pos, b: Pos) -> String {
    let (lo, hi) = if (a.row, a.col) <= (b.row, b.col) {
      (a, b)
    } else {
      (b, a)
    };
    self.pos_slice_exclusive(lo, hi)
  }
  pub(super) fn pos_slice_exclusive(&self, s: Pos, end: Pos) -> String {
    let mut out = String::new();
    if s.row == end.row {
      let line = &self.lines[s.row].0;
      let hi = end.col.min(line.len());
      let lo = s.col.min(hi);
      for g in &line[lo..hi] {
        out.push_str(&g.to_string());
      }
      return out;
    }
    let first = &self.lines[s.row].0;
    let lo = s.col.min(first.len());
    for g in &first[lo..] {
      out.push_str(&g.to_string());
    }
    for row in (s.row + 1)..end.row {
      out.push('\n');
      for g in &self.lines[row].0 {
        out.push_str(&g.to_string());
      }
    }
    out.push('\n');
    let last = &self.lines[end.row].0;
    let hi = end.col.min(last.len());
    for g in &last[..hi] {
      out.push_str(&g.to_string());
    }
    out
  }
  pub(super) fn pos_slice_str(&self, s: Pos, e: Pos) -> String {
    let (s, e) = ordered(s, e);
    if s.row == e.row {
      self.lines[s.row].0[s.col..=e.col]
        .iter()
        .map(ToString::to_string)
        .collect()
    } else {
      let mut result = String::new();
      // First line from s.col to end
      for g in &self.lines[s.row].0[s.col..] {
        result.push_str(&g.to_string());
      }
      // Middle lines
      for line in &self.lines.0[s.row + 1..e.row] {
        result.push('\n');
        result.push_str(&line.to_string());
      }
      // Last line from start to e.col
      result.push('\n');
      for g in &self.lines[e.row].0[..=e.col] {
        result.push_str(&g.to_string());
      }
      result
    }
  }
  /// Returns the start/end span of a number at a given position, if any
  pub(super) fn number_at(&self, mut pos: Pos) -> Option<(Pos, Pos)> {
    pos = self.clamp_pos(pos);

    // Radix-prefixed literals (0x.., 0o.., 0b..) are checked first
    // because their digit predicates overlap with plain decimal,
    // cursor on the leading `0` of `0x...` would otherwise be parsed
    // as a standalone "0" with the decimal scanner.
    if let Some(range) = self.try_radix_literal_at(pos) {
      return Some(range);
    }

    let is_number_char = |gr: &Grapheme| {
      gr.as_char()
        .is_some_and(|c| c == '.' || c == '-' || c.is_ascii_digit())
    };
    let is_digit = |gr: &Grapheme| gr.as_char().is_some_and(|c| c.is_ascii_digit());

    if !is_number_char(self.gr_at(pos)?) {
      return None;
    }

    // If cursor is on '-', advance to the first digit
    if self.gr_at(pos)?.as_char() == Some('-') {
      pos = pos.col_add(1);
    }

    let mut start = self
      .scan_backward_from(pos, |g| !is_digit(g))
      .map_or(Pos::MIN, |pos| Pos {
        row: pos.row,
        col: pos.col + 1,
      });
    let end = self.scan_forward_from(pos, |g| !is_digit(g)).map_or(
      Pos {
        row: pos.row,
        col: self.lines[pos.row].len().saturating_sub(1),
      },
      |pos| Pos {
        row: pos.row,
        col: pos.col.saturating_sub(1),
      },
    );

    if start > Pos::MIN && self.lines[start.row][start.col.saturating_sub(1)].as_char() == Some('-')
    {
      start.col -= 1;
    }

    Some((start, end))
  }
  // What predicate to use for digit bodies depends on the marker.
  fn predicate_for(marker: char) -> Option<fn(char) -> bool> {
    match marker {
      'x' | 'X' => Some(|c: char| c.is_ascii_hexdigit()),
      'o' | 'O' => Some(|c: char| matches!(c, '0'..='7')),
      'b' | 'B' => Some(|c: char| c == '0' || c == '1'),
      _ => None,
    }
  }

  /// If `pos` is on (or inside) a `0x.../0o.../0b...` literal, return
  /// the literal's full span. Handles the three cursor positions:
  ///   - on the leading `0` (next char is the marker)
  ///   - on the marker `x`/`o`/`b` itself
  ///   - on any body digit (scan backward through digit chars to the
  ///     marker, verify a `0` precedes it)
  fn try_radix_literal_at(&self, pos: Pos) -> Option<(Pos, Pos)> {
    let line = &self.lines[pos.row].0;
    let row = pos.row;
    let char_at = |col: usize| -> Option<char> { line.get(col)?.as_char() };

    let cur = char_at(pos.col)?;
    let marker_col: usize = if cur == '0'
      && char_at(pos.col + 1).is_some_and(|c| {
        Self::predicate_for(c).is_some() || matches!(c, 'x' | 'X' | 'o' | 'O' | 'b' | 'B')
      }) {
      // Cursor on leading '0'; next char is the marker
      let next = char_at(pos.col + 1)?;
      if !matches!(next, 'x' | 'X' | 'o' | 'O' | 'b' | 'B') {
        return None;
      }
      pos.col + 1
    } else if matches!(cur, 'x' | 'X' | 'o' | 'O' | 'b' | 'B')
      && pos.col > 0
      && char_at(pos.col - 1) == Some('0')
    {
      // Cursor directly on the marker, preceded by '0'
      pos.col
    } else if cur.is_ascii_hexdigit() {
      // Cursor on a body digit — scan back through possible-hex chars
      // for the marker. We use hex-digit because it's the most
      // permissive of the three formats and any oct/bin digit is
      // also a hex digit.
      let mut col = pos.col;
      while col > 0 && char_at(col - 1).is_some_and(|c| c.is_ascii_hexdigit()) {
        col -= 1;
      }
      // `col` is now the leftmost char in the digit run. To be inside
      // a radix literal, `col` must be preceded by a marker, which
      // must in turn be preceded by '0'.
      if col < 2 {
        return None;
      }
      let marker = char_at(col - 1)?;
      if !matches!(marker, 'x' | 'X' | 'o' | 'O' | 'b' | 'B') {
        return None;
      }
      if char_at(col - 2) != Some('0') {
        return None;
      }
      col - 1
    } else {
      return None;
    };

    let marker = char_at(marker_col)?;
    let pred = Self::predicate_for(marker)?;

    // Walk forward through digits matching the format's predicate.
    let mut end_col = marker_col + 1;
    while end_col < line.len() && char_at(end_col).is_some_and(pred) {
      end_col += 1;
    }
    if end_col == marker_col + 1 {
      // No digits after the prefix = not a complete literal.
      return None;
    }

    let start = Pos {
      row,
      col: marker_col - 1, // the leading '0'
    };
    let end = Pos {
      row,
      col: end_col - 1,
    };
    Some((start, end))
  }
  pub(super) fn number_at_cursor(&self) -> Option<(Pos, Pos)> {
    self.number_at(self.cursor.pos)
  }
  pub(super) fn clamp_pos(&self, mut pos: Pos) -> Pos {
    pos.clamp_row(&self.lines);
    pos.clamp_col(&self.lines[pos.row].0, false);
    pos
  }
  pub(super) fn gr_at(&self, pos: Pos) -> Option<&Grapheme> {
    self.lines.get(pos.row)?.0.get(pos.col)
  }
  pub(super) fn char_classes_backward_from(
    &self,
    pos: Pos,
  ) -> impl Iterator<Item = (Pos, CharClass)> {
    CharClassIterRev::new(&self.lines, pos)
  }
  pub(super) fn char_classes_forward_from(
    &self,
    pos: Pos,
  ) -> impl Iterator<Item = (Pos, CharClass)> {
    CharClassIter::new(&self.lines, pos)
  }
  pub(super) fn scan_backward_from<F: FnMut(&Grapheme) -> bool>(
    &self,
    mut pos: Pos,
    mut f: F,
  ) -> Option<Pos> {
    pos.clamp_row(&self.lines);
    pos.clamp_col(&self.lines[pos.row].0, false);
    let Pos { mut row, mut col } = pos;

    loop {
      let line = &self.lines[row];
      if !line.is_empty() && f(&line[col]) {
        return Some(Pos { row, col });
      }
      if col > 0 {
        col -= 1;
      } else if row > 0 {
        row -= 1;
        col = self.lines[row].len().saturating_sub(1);
      } else {
        return None;
      }
    }
  }
  pub(super) fn scan_backward<F: FnMut(&Grapheme) -> bool>(&self, f: F) -> Option<Pos> {
    self.scan_backward_from(self.cursor.pos.col_add_signed(-1), f)
  }
  pub(super) fn scan_forward_from<F: FnMut(&Grapheme) -> bool>(
    &self,
    mut pos: Pos,
    mut f: F,
  ) -> Option<Pos> {
    pos.clamp_row(&self.lines);
    pos.clamp_col(&self.lines[pos.row].0, false);
    let Pos { mut row, mut col } = pos;

    loop {
      let line = &self.lines[row];
      if col >= line.len() {
        if row < self.lines.len() - 1 {
          row += 1;
          col = 0;
          continue;
        }
        return None;
      }
      if !line.is_empty() && f(&line[col]) {
        return Some(Pos { row, col });
      }
      if col < self.lines[row].len().saturating_sub(1) {
        col += 1;
      } else if row < self.lines.len().saturating_sub(1) {
        row += 1;
        col = 0;
      } else {
        return None;
      }
    }
  }
  pub(super) fn scan_forward<F: FnMut(&Grapheme) -> bool>(&self, f: F) -> Option<Pos> {
    self.scan_forward_from(self.cursor.pos, f)
  }
  pub(super) fn line_to_pos(&self, pos: Pos) -> &[Grapheme] {
    let line = &self.lines[pos.row];
    let col = pos.col.min(line.len());
    &line[..col]
  }
  pub(super) fn line_from_pos(&self, pos: Pos) -> &[Grapheme] {
    let line = &self.lines[pos.row];
    let col = pos.col.min(line.len());
    &line[col..]
  }
  pub fn row(&self) -> usize {
    self.cursor.pos.row
  }
  pub(super) fn offset_row(&self, offset: isize) -> usize {
    let mut row = self.cursor.pos.row.saturating_add_signed(offset);
    row = row.clamp(0, self.lines.len().saturating_sub(1));
    row
  }
  pub(super) fn col(&self) -> usize {
    self.cursor.pos.col
  }
  pub(crate) fn offset_col(&self, row: usize, offset: isize) -> usize {
    let mut col = self.cursor.pos.col.saturating_add_signed(offset);
    let max = if self.cursor.exclusive {
      self.lines[row].len().saturating_sub(1)
    } else {
      self.lines[row].len()
    };
    col = col.clamp(0, max);
    col
  }
  pub(crate) fn offset_col_absolute(&self, row: usize, offset: isize) -> usize {
    let mut col = self.cursor.pos.col.saturating_add_signed(offset);
    let max = self.lines[row].len();
    col = col.clamp(0, max);
    col
  }
  pub(super) fn offset_col_wrapping_at(
    &self,
    row: usize,
    offset: isize,
    pos: Pos,
  ) -> (usize, usize) {
    let mut row = row;
    let mut col = pos.col as isize + offset;

    while col < 0 {
      if row == 0 {
        col = 0;
        break;
      }
      row -= 1;
      col += self.lines[row].len() as isize + 1;
    }
    while col > self.lines[row].len() as isize {
      if row >= self.lines.len() - 1 {
        col = self.lines[row].len() as isize;
        break;
      }
      col -= self.lines[row].len() as isize + 1;
      row += 1;
    }

    (row, col as usize)
  }
  pub(super) fn offset_col_wrapping(&self, row: usize, offset: isize) -> (usize, usize) {
    self.offset_col_wrapping_at(row, offset, self.cursor.pos)
  }
  pub(super) fn cursor_on_ws(&self) -> bool {
    let line = self.cur_line();
    let col = self.cursor.pos.col;
    line.graphemes().get(col).is_some_and(Grapheme::is_ws)
  }
  pub fn set_cursor(&mut self, mut pos: Pos) {
    pos.clamp_row(&self.lines);
    pos.clamp_col(&self.lines[pos.row].0, self.cursor.exclusive);
    self.cursor.pos = pos;
  }
  pub(super) fn set_row(&mut self, row: usize) {
    self.set_cursor(Pos {
      row,
      col: self.saved_col.unwrap_or(self.cursor.pos.col),
    });
  }
  pub(super) fn offset_cursor(&self, row_offset: isize, col_offset: isize) -> Pos {
    let row = self.offset_row(row_offset);
    let col = self.offset_col(row, col_offset);
    Pos { row, col }
  }
  pub(super) fn offset_cursor_wrapping(&self, row_offset: isize, col_offset: isize) -> Pos {
    let row = self.offset_row(row_offset);
    let (row, col) = self.offset_col_wrapping(row, col_offset);
    Pos { row, col }
  }
  pub(super) fn break_line_unchecked(&mut self) {
    self.break_line_at_unchecked(self.cursor.pos);
  }
  pub(super) fn break_line_at(&mut self, pos: Pos) {
    self.break_line_at_inner(pos, true);
  }
  pub(super) fn break_line_at_unchecked(&mut self, pos: Pos) {
    self.break_line_at_inner(pos, false);
  }
  pub(super) fn break_line_at_inner(&mut self, pos: Pos, invalidate_cache: bool) {
    let Pos { row, col } = pos;
    let rest = self.lines[row].split_off(col);

    self.lines.insert(row + 1, rest);
    if invalidate_cache {
      self.indent_cache = None;
    }
    let (_, end) = self.indent_levels_for_row(row + 1);
    let new_line = self.lines.get_mut(row + 1).unwrap();

    let mut col = 0;
    for tab in std::iter::repeat_n(Grapheme::from('\t'), end) {
      new_line.insert(0, tab);
      col += 1;
    }

    self.cursor.pos.set(row + 1, col);
  }
  pub(super) fn line_iter_mut(&mut self, span: (usize, usize)) -> impl Iterator<Item = &mut Line> {
    let (start, end) = ordered(span.0, span.1);
    self.lines.iter_mut().take(end + 1).skip(start)
  }
  pub(super) fn line_mut(&mut self, row: usize) -> &mut Line {
    &mut self.lines[row]
  }
  pub(super) fn line(&self, row: usize) -> &Line {
    &self.lines[row]
  }
  pub(super) fn cur_line_mut(&mut self) -> &mut Line {
    &mut self.lines[self.cursor.pos.row]
  }
  pub(super) fn cur_line(&self) -> &Line {
    &self.lines[self.cursor.pos.row]
  }
  pub fn count_graphemes(&self) -> usize {
    self.lines.iter().map(Line::len).sum()
  }
  pub fn is_empty(&self) -> bool {
    self.lines.len() == 0 || (self.lines.len() == 1 && self.count_graphemes() == 0)
  }
  pub fn clear_pending_search(&mut self) {
    self.pending_search = None;
  }
  pub fn update_pending_search(&mut self, new: Option<VarStr>) {
    let Some(new) = new else { return };
    self.pending_search = (!new.is_empty()).then_some(new);
  }
}

impl Display for super::LineBuf {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", self.lines())
  }
}

/// Classic diff algorithm
pub(super) fn find_diff_range(old: &str, new: &str) -> Diff {
  let (oa, na) = (old.as_bytes(), new.as_bytes());
  let mut left = 0;
  let max_left = oa.len().min(na.len());
  while left < max_left && oa[left] == na[left] {
    left += 1;
  }
  let mut right_old = oa.len();
  let mut right_new = na.len();
  while right_old > left && right_new > left && oa[right_old - 1] == na[right_new - 1] {
    right_old -= 1;
    right_new -= 1;
  }
  Diff {
    start: left,
    end_old: right_old,
    end_new: right_new,
  }
}

#[cfg(test)]
mod indent_tests {
  use crate::readline::LineBuf;
  use crate::tests::testutil::TestGuard;

  fn last_line(buf: &LineBuf) -> String {
    buf
      .to_string()
      .lines()
      .last()
      .unwrap_or_default()
      .to_string()
  }

  #[test]
  fn closer_dedent_is_idempotent() {
    // Typing fi aligns it with if; a trailing space must not dedent it again.
    let _g = TestGuard::new();
    let mut buf = LineBuf::default();
    buf.set_buffer("if true; then\n\techo\n\t");
    buf.move_cursor_to_end();
    buf.insert_str("fi");
    assert_eq!(last_line(&buf), "fi", "fi should dedent to the if's level");

    buf.move_cursor_to_end();
    buf.insert_str(" ");
    assert_eq!(
      last_line(&buf),
      "fi ",
      "trailing space must not dedent again"
    );

    buf.move_cursor_to_end();
    buf.insert_str(" ");
    assert_eq!(
      last_line(&buf),
      "fi  ",
      "still idempotent on further spaces"
    );
  }

  #[test]
  fn middle_keyword_dedents_one_level() {
    let _g = TestGuard::new();
    let mut buf = LineBuf::default();
    buf.set_buffer("if true; then\n\techo\n\t");
    buf.move_cursor_to_end();
    buf.insert_str("else");
    assert_eq!(last_line(&buf), "else", "else should align with if");
  }

  #[test]
  fn stray_closer_does_not_dedent() {
    // A } that closes no open brace shouldn't dedent.
    let _g = TestGuard::new();
    let mut buf = LineBuf::default();
    buf.set_buffer("echo hi\n\t");
    buf.move_cursor_to_end();
    buf.insert_str("}");
    assert_eq!(
      last_line(&buf),
      "\t}",
      "a brace that closes nothing is left in place"
    );
  }
}

#[cfg(test)]
mod trim_range_tests {
  use crate::readline::LineBuf;
  use crate::readline::editcmd::LineAddr;
  use crate::readline::editmode::AddressRange;
  use crate::tests::testutil::TestGuard;

  fn trimmed(content: &str, lo: usize, hi: usize, bang: bool) -> String {
    let mut b = LineBuf::default();
    b.set_buffer(content);
    b.trim_range(
      AddressRange::Range(LineAddr::Number(lo), LineAddr::Number(hi)),
      bang,
    )
    .unwrap();
    b.to_string()
  }

  #[test]
  fn removes_trailing_blank_run_past_range() {
    let _g = TestGuard::new();
    // range is only line 1; the trailing blank run (outside it) is still removed
    assert_eq!(trimmed("foo\n\n\n", 1, 1, false), "foo");
  }

  #[test]
  fn keeps_interior_blank_lines() {
    let _g = TestGuard::new();
    assert_eq!(trimmed("foo\n\nbar\n\n", 1, 3, false), "foo\n\nbar");
  }

  #[test]
  fn strips_trailing_whitespace() {
    let _g = TestGuard::new();
    assert_eq!(trimmed("foo   \nbar", 1, 1, false), "foo\nbar");
  }

  #[test]
  fn non_bang_keeps_leading_blank_run() {
    let _g = TestGuard::new();
    assert_eq!(trimmed("\n\nfoo", 3, 3, false), "\n\nfoo");
  }

  #[test]
  fn bang_removes_leading_blank_run() {
    let _g = TestGuard::new();
    assert_eq!(trimmed("\n\nfoo", 3, 3, true), "foo");
  }

  #[test]
  fn bang_dedents_common_prefix() {
    let _g = TestGuard::new();
    assert_eq!(trimmed("  a\n    b", 1, 2, true), "a\n  b");
  }
}
