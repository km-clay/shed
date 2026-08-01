use crate::{queue_term, state::terminal::Terminal};

use super::{
  Candidate, CompMatch, CompResponse, Completer, K as KeyEvent, ShResult, Shed, SimpleCompleter,
  fuzzy::{ClampedUsize, emphasize_grid, one_line},
  key,
  state::terminal::calc_str_width,
  write_term,
};

/// Truncate `s` (as display width) to at most `max_width` columns. Stops
/// before adding a character that would push past the limit. Used when a
/// description doesn't fit even after eating all the available padding —
/// the caller appends an ellipsis after.
pub(crate) fn truncate_to_width(s: &str, max_width: usize) -> String {
  let mut out = String::with_capacity(s.len());
  let mut w = 0;
  for ch in s.chars() {
    let cw = calc_str_width(&ch.to_string());
    if w + cw > max_width {
      break;
    }
    out.push(ch);
    w += cw;
  }
  out
}

/// Records the row count a selector drew, so `clear()` wipes exactly that many.
/// The windowed layout itself is recomputed each draw by `pack_columns`.
#[derive(Debug, Default, Clone)]
pub(crate) struct GridLayout {
  /// Total rows drawn below the prompt, so `clear()` wipes exactly what we drew.
  rows: usize,
}

impl GridLayout {
  pub(crate) const COL_GAP: usize = 2;
  /// Each grid column is this many rows tall; beyond a screenful the selector
  /// scrolls horizontally to keep the cursor visible.
  pub const MAX_VISIBLE_ROWS: usize = 10;
}

/// Greedily pack fixed-height (`rows`-tall) columns, column-major, starting at
/// column `scroll_col`, until the terminal width is used up. `col_width(start,
/// end)` measures the column spanning candidate indices `start..end`. Returns
/// each visible column's width. O(visible cells), never O(total candidates).
pub(crate) fn pack_columns(
  n: usize,
  scroll_col: usize,
  rows: usize,
  t_cols: usize,
  col_width: impl Fn(usize, usize) -> usize,
) -> Vec<usize> {
  let mut widths = Vec::new();
  let mut used = 0usize;
  let mut col = scroll_col;
  while col * rows < n {
    let start = col * rows;
    let end = (start + rows).min(n);
    // A column can never be wider than the terminal.
    let w = col_width(start, end).min(t_cols);
    let gap = if widths.is_empty() {
      0
    } else {
      GridLayout::COL_GAP
    };
    // Always keep at least one column, even if it alone overflows the width.
    if !widths.is_empty() && used + gap + w > t_cols {
      break;
    }
    used += gap + w;
    widths.push(w);
    col += 1;
  }
  widths
}

/// Scroll `scroll_col` horizontally so the cursor's column stays on screen.
pub(crate) fn scroll_into_view(
  cursor: usize,
  scroll_col: &mut usize,
  n: usize,
  rows: usize,
  t_cols: usize,
  col_width: impl Fn(usize, usize) -> usize,
) {
  if n == 0 {
    *scroll_col = 0;
    return;
  }
  let cursor_col = cursor / rows;
  if cursor_col < *scroll_col {
    *scroll_col = cursor_col;
  }
  while cursor_col
    >= *scroll_col
      + pack_columns(n, *scroll_col, rows, t_cols, &col_width)
        .len()
        .max(1)
  {
    *scroll_col += 1;
    if *scroll_col >= cursor_col {
      *scroll_col = cursor_col;
      break;
    }
  }
}

/// Move one column left/right, staying on the same row and optionally wrapping.
/// The last column may be short, so plain `±rows` drifts by the shortfall on
/// wrap; this lands on the same row in the target column instead.
pub(crate) fn step_column(cursor: usize, n: usize, rows: usize, right: bool, wrap: bool) -> usize {
  if n == 0 {
    return 0;
  }
  let row = cursor % rows;
  let col = cursor / rows;
  let num_cols = n.div_ceil(rows);
  if right {
    let next = col + 1;
    if next < num_cols && next * rows + row < n {
      next * rows + row
    } else if wrap {
      // Wrap to the first column at this row (always present for `row < rows`).
      row.min(n - 1)
    } else {
      cursor
    }
  } else if col > 0 {
    (col - 1) * rows + row
  } else if wrap {
    // Wrap to the last column that actually has a cell at this row.
    let mut c = num_cols - 1;
    while c * rows + row >= n {
      c -= 1;
    }
    c * rows + row
  } else {
    cursor
  }
}

#[derive(Debug, Default, Clone)]
pub(crate) struct GridSelector {
  candidates: Vec<Candidate>,
  cursor: ClampedUsize,
  old_layout: Option<GridLayout>,
  /// Index of the leftmost visible column (each column `MAX_VISIBLE_ROWS` tall).
  scroll_col: usize,
  /// Column to return to after drawing
  prompt_cursor_col: usize,
  /// True once the user has stepped into the menu (first Tab/arrow).
  has_selection: bool,
  /// The token being completed; its common prefix with each candidate is
  /// emphasized in the grid.
  prefix: String,
}

/// Number of leading characters `a` and `b` share, case-insensitively.
fn common_prefix_len(a: &str, b: &str) -> usize {
  a.chars()
    .zip(b.chars())
    .take_while(|(x, y)| x.eq_ignore_ascii_case(y))
    .count()
}

impl GridSelector {
  pub fn new() -> Self {
    Self::default()
  }
  fn reset(&mut self) {
    *self = Self::new();
  }

  pub fn activate(&mut self, candidates: Vec<Candidate>) {
    self.cursor = ClampedUsize::new(0, candidates.len(), true);
    self.candidates = candidates;
    self.old_layout = None;
    self.scroll_col = 0;
    self.has_selection = false;
  }

  pub fn selected_candidate(&self) -> Option<Candidate> {
    if !self.has_selection {
      return None;
    }
    self.candidates.get(self.cursor.get()).cloned()
  }

  pub fn next_candidate(&mut self) {
    if self.has_selection {
      self.cursor.wrap_add(1);
    } else {
      self.has_selection = true;
    }
  }

  pub fn prev_candidate(&mut self) {
    if !self.has_selection {
      self.has_selection = true;
    }
    self.cursor.wrap_sub(1);
  }

  /// Move one column left/right, wrapping at the ends.
  pub fn move_col(&mut self, right: bool) {
    if !self.has_selection {
      self.has_selection = true;
      return;
    }
    let next = step_column(
      self.cursor.get(),
      self.candidates.len(),
      GridLayout::MAX_VISIBLE_ROWS,
      right,
      true,
    );
    self.cursor.set(next);
  }

  pub fn set_prompt_line_context(&mut self, _line_width: usize, cursor_col: usize) {
    self.prompt_cursor_col = cursor_col;
  }

  pub fn set_prefix(&mut self, prefix: String) {
    self.prefix = prefix;
  }

  pub fn clear(&mut self) {
    if let Some(layout) = self.old_layout.take() {
      for _ in 0..layout.rows {
        queue_term!(TermCtl::Cursor(Down(1)), TermCtl::Clear(WholeLine)).ok();
      }
      // Move back up to the prompt row and right to the original column.
      queue_term!(TermCtl::Cursor(Up(layout.rows as u16))).ok();
      if self.prompt_cursor_col > 0 {
        queue_term!(TermCtl::Cursor(Forward(self.prompt_cursor_col as u16))).ok();
      }
    }
  }

  /// `(max name width, max desc width incl. parens)` over candidates `start..end`.
  fn col_dims(cands: &[Candidate]) -> (usize, usize) {
    let name = cands
      .iter()
      .map(|c| calc_str_width(&one_line(c.as_str())))
      .max()
      .unwrap_or(0);
    let desc = cands
      .iter()
      .map(|c| {
        c.desc
          .as_ref()
          .filter(|d| !d.is_empty())
          .map_or(0, |d| calc_str_width(d) + 2)
      })
      .max()
      .unwrap_or(0);
    (name, desc)
  }

  fn col_width(cands: &[Candidate]) -> usize {
    let (name, desc) = Self::col_dims(cands);
    if desc > 0 { name + 2 + desc } else { name }
  }

  fn visible_window(&self, t_cols: usize) -> Vec<usize> {
    pack_columns(
      self.candidates.len(),
      self.scroll_col,
      GridLayout::MAX_VISIBLE_ROWS,
      t_cols,
      |start, end| Self::col_width(&self.candidates[start..end]),
    )
  }

  fn ensure_cursor_visible(&mut self, t_cols: usize) {
    let cursor = self.cursor.get();
    let n = self.candidates.len();
    let candidates = &self.candidates;
    scroll_into_view(
      cursor,
      &mut self.scroll_col,
      n,
      GridLayout::MAX_VISIBLE_ROWS,
      t_cols,
      |start, end| Self::col_width(&candidates[start..end]),
    );
  }

  /// Jump one screenful of columns forward/back, wrapping at the ends.
  pub fn next_page(&mut self) {
    self.has_selection = true;
    if self.candidates.is_empty() {
      return;
    }
    let t_cols = Shed::term(Terminal::t_cols);
    let step = self.visible_window(t_cols).len().max(1) * GridLayout::MAX_VISIBLE_ROWS;
    self.cursor.wrap_add(step);
  }

  pub fn prev_page(&mut self) {
    self.has_selection = true;
    if self.candidates.is_empty() {
      return;
    }
    let t_cols = Shed::term(Terminal::t_cols);
    let step = self.visible_window(t_cols).len().max(1) * GridLayout::MAX_VISIBLE_ROWS;
    self.cursor.wrap_sub(step);
  }

  pub fn draw(&mut self) -> usize {
    if self.candidates.is_empty() {
      return 0;
    }

    let t_cols = Shed::term(Terminal::t_cols);
    let rows = GridLayout::MAX_VISIBLE_ROWS;
    self.ensure_cursor_visible(t_cols);

    let col_widths = self.visible_window(t_cols);
    let num_cols = col_widths.len().max(1);
    let cursor_pos = self.cursor.get();
    let n = self.candidates.len();
    let first = self.scroll_col * rows;
    // The first visible column has the lowest indices, so it's the tallest.
    let grid_rows = rows.min(n - first);
    let visible_end = ((self.scroll_col + num_cols) * rows).min(n);

    // break the line to move under the prompt
    write_term!("\n").ok();

    // Column-major: cell (col c, row r) is candidate `(scroll_col + c) * rows + r`.
    for r in 0..grid_rows {
      for (c, col_w) in col_widths.iter().enumerate() {
        let idx = (self.scroll_col + c) * rows + r;
        if idx >= n {
          break; // later columns at this row are exhausted too
        }

        let col_start = (self.scroll_col + c) * rows;
        let col_end = (col_start + rows).min(n);
        let (col_name_max, _) = Self::col_dims(&self.candidates[col_start..col_end]);

        let cand = &self.candidates[idx];
        let name_plain = one_line(cand.as_str());
        let name_w = calc_str_width(&name_plain);
        // Emphasize the prefix the candidate shares with the typed token.
        let prefix_len = common_prefix_len(&self.prefix, &name_plain);
        let name = emphasize_grid(&name_plain, |i| i < prefix_len);

        let is_selected = self.has_selection && idx == cursor_pos;

        match (&cand.desc, is_selected) {
          (Some(desc), _) if !desc.is_empty() => {
            // Decide how much room the description has. Normally that's
            // col_w - col_name_max - 2 (the aligned position). But if the
            // description doesn't fit there, it can extend leftward into
            // the name-pad, down to a minimum 2-char gap after the name.
            // Beyond that point we truncate with an ellipsis.
            let desc_w_full = calc_str_width(desc) + 2; // includes parens
            let aligned_avail = col_w.saturating_sub(col_name_max + 2);
            let max_extend_avail = col_w.saturating_sub(name_w + 2);
            let (pad_chars, desc_text) = if desc_w_full <= aligned_avail {
              // Fits at the aligned position; keep alignment.
              (col_name_max.saturating_sub(name_w), format!("({desc})"))
            } else if desc_w_full <= max_extend_avail {
              // Doesn't fit aligned, but does fit if we extend into the
              // padding. Reduce the name-pad just enough to fit.
              let need = desc_w_full - aligned_avail;
              let pad = col_name_max.saturating_sub(name_w).saturating_sub(need);
              (pad, format!("({desc})"))
            } else {
              // Even fully extended (no name-pad at all) it doesn't fit.
              // Truncate the description and append an ellipsis.
              let truncated = truncate_to_width(desc, max_extend_avail.saturating_sub(3));
              (0, format!("({truncated}…)"))
            };
            let name_pad_str = " ".repeat(pad_chars);
            let used = name_w + pad_chars + 2 + calc_str_width(&desc_text);
            let trailing = " ".repeat(col_w.saturating_sub(used));
            if is_selected {
              write_term!("\x1b[7m{name}{name_pad_str}  {desc_text}{trailing}\x1b[27m",).ok();
            } else {
              write_term!("{name}{name_pad_str}  \x1b[2m{desc_text}\x1b[22m{trailing}",).ok();
            }
          }
          (_, true) => {
            // Selected without description.
            let trailing = " ".repeat(col_w.saturating_sub(name_w));
            write_term!("\x1b[7m{name}{trailing}\x1b[27m").ok();
          }
          (_, false) => {
            // Unselected without description.
            let trailing = " ".repeat(col_w.saturating_sub(name_w));
            write_term!("{name}{trailing}").ok();
          }
        }

        // Inter-column gap, skipped when the next column has no cell here.
        if c + 1 < num_cols && (self.scroll_col + c + 1) * rows + r < n {
          write_term!("{}", " ".repeat(GridLayout::COL_GAP)).ok();
        }
      }
      if r + 1 < grid_rows {
        write_term!("\n").ok();
      }
    }

    // Show a position counter when not everything is on screen.
    let counter_rows = if first > 0 || visible_end < n {
      write_term!(
        "\n\x1b[2mItems {} to {} of {}\x1b[22m",
        first + 1,
        visible_end,
        n
      )
      .ok();
      1
    } else {
      0
    };
    let rows_drawn = grid_rows + counter_rows;

    // Walk back up to the prompt row. Restore the column with \r +
    // horizontal move.
    queue_term!(
      TermCtl::Cursor(Up(rows_drawn as u16)),
      TermCtl::PrintChar('\r')
    )
    .ok();
    if self.prompt_cursor_col > 0 {
      queue_term!(TermCtl::Cursor(Forward(self.prompt_cursor_col as u16))).ok();
    }

    // Store the visible row count so clear() wipes exactly what we drew.
    self.old_layout = Some(GridLayout { rows: rows_drawn });

    rows_drawn
  }
}

pub(crate) struct GridCompleter {
  completer: SimpleCompleter,
  selector: GridSelector,
}

impl GridCompleter {
  pub fn new() -> Self {
    Self {
      completer: SimpleCompleter::default(),
      selector: GridSelector::new(),
    }
  }

  /// Preview the selected candidate into the buffer, or just consume the key
  /// if nothing is selected yet.
  fn preview_response(&self) -> CompResponse {
    match self.selected_candidate() {
      Some(cand) => CompResponse::Preview(cand),
      None => CompResponse::Consumed,
    }
  }
}

impl Completer for GridCompleter {
  fn set_prompt_line_context(&mut self, line_width: usize, cursor_col: usize) {
    self
      .selector
      .set_prompt_line_context(line_width, cursor_col);
  }

  fn complete(
    &mut self,
    line: String,
    cursor_pos: usize,
    direction: i32,
    source: super::CompSource,
  ) -> ShResult<Option<CompMatch>> {
    let inner = self
      .completer
      .complete(line, cursor_pos, direction, source)?;
    let candidates = self.completer.candidates.clone();
    match candidates.len() {
      0 => {
        self.completer.reset();
        Ok(None)
      }
      1 => {
        // Prime the selector so `selected_candidate()` returns the
        // single candidate. The caller at handle_tab in mod.rs reads
        // it to compute the new cursor position after splicing. We also
        // set `has_selection` here. The single-candidate case is effectively
        // "auto-accept", which is conceptually past the no-selection
        // state.
        let cand_str = candidates[0].as_str().to_string();
        self.selector.activate(candidates);
        self.selector.has_selection = true;
        let completed = self.get_completed_line(&cand_str);
        // Preserve the inner completer's match-kind (Exact vs CommonPrefix);
        // we only rewrite the spliced line.
        Ok(inner.map(|m| m.with_line(completed)))
      }
      _ => {
        self.selector.activate(candidates);
        let (start, end) = self.completer.token_span;
        self
          .selector
          .set_prefix(self.completer.original_input[start..end].to_string());
        Ok(None)
      }
    }
  }

  fn clear(&mut self) {
    self.selector.clear();
  }

  fn reset(&mut self) {
    self.completer.reset();
    self.selector.reset();
  }

  fn reset_stay_active(&mut self) {
    self.selector.cursor.set(0);
  }

  fn is_active(&self) -> bool {
    !self.selector.candidates.is_empty()
  }

  fn selected_candidate(&self) -> Option<Candidate> {
    self.selector.selected_candidate()
  }

  fn token_span(&self) -> (usize, usize) {
    self.completer.token_span()
  }

  fn original_input(&self) -> &str {
    &self.completer.original_input
  }

  fn all_candidates(&self) -> Vec<Candidate> {
    self.completer.all_candidates()
  }

  fn draw(&mut self) -> usize {
    self.selector.draw()
  }

  fn predicted_rows(&self) -> Option<usize> {
    if self.selector.candidates.is_empty() {
      return Some(0);
    }
    let t_cols = Shed::term(Terminal::t_cols);
    let rows = GridLayout::MAX_VISIBLE_ROWS;
    let n = self.selector.candidates.len();
    let first = self.selector.scroll_col * rows;
    let grid_rows = rows.min(n.saturating_sub(first)).max(1);
    let num_cols = self.selector.visible_window(t_cols).len().max(1);
    let visible_end = ((self.selector.scroll_col + num_cols) * rows).min(n);
    let counter = usize::from(first > 0 || visible_end < n);
    Some(grid_rows + counter)
  }

  #[expect(clippy::unnested_or_patterns)]
  fn handle_key(&mut self, key: KeyEvent) -> ShResult<CompResponse> {
    match key {
      // Live preview: splice the now-selected candidate into the buffer so the
      // user sees what they'd accept. The completer stays active.
      key!(Tab) | key!(Down) => {
        self.selector.next_candidate();
        Ok(self.preview_response())
      }
      key!(Shift + Tab) | key!(Up) => {
        self.selector.prev_candidate();
        Ok(self.preview_response())
      }
      key!(Right) => {
        self.selector.move_col(true);
        Ok(self.preview_response())
      }
      key!(Left) => {
        self.selector.move_col(false);
        Ok(self.preview_response())
      }
      key!(Ctrl + 'f') | key!(PageDown) => {
        self.selector.next_page();
        Ok(self.preview_response())
      }
      key!(Ctrl + 'b') | key!(PageUp) => {
        self.selector.prev_page();
        Ok(self.preview_response())
      }
      key!(Enter) => match self.selected_candidate() {
        Some(cand) => Ok(CompResponse::Accept(cand)),
        None => Ok(CompResponse::Dismiss),
      },
      key!(Esc) | key!(Ctrl + 'c') => Ok(CompResponse::Dismiss),

      _ => Ok(CompResponse::DismissPassthrough),
    }
  }

  fn get_completed_line(&self, candidate: &str) -> String {
    let (start, end) = self.completer.token_span;
    format!(
      "{}{}{}",
      &self.completer.original_input[..start],
      candidate,
      &self.completer.original_input[end..],
    )
  }
}
