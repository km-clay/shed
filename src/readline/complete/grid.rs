use crate::{queue_term, state::terminal::Terminal};

use super::{
  Candidate, CompMatch, CompResponse, Completer, K as KeyEvent, ShResult, Shed, SimpleCompleter,
  fuzzy::ClampedUsize, key, state::terminal::calc_str_width, write_term,
};

/// Truncate `s` (as display width) to at most `max_width` columns. Stops
/// before adding a character that would push past the limit. Used when a
/// description doesn't fit even after eating all the available padding —
/// the caller appends an ellipsis after.
fn truncate_to_width(s: &str, max_width: usize) -> String {
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

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct CellMetrics {
  /// Display width of the candidate's name.
  pub name: usize,
  /// Display width of the description portion `(desc)` including the parens,
  /// or 0 if no description.
  pub desc: usize,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct GridLayout {
  top_left: usize,
  rows: usize,
  /// Total width of each column (`name_max` + 2 + `desc_max`, or just `name_max`
  /// if no cell in that column has a description).
  col_widths: Vec<usize>,
  /// Max name width in each column. Used at render time to right-pad each
  /// cell's name so descriptions within a column align vertically.
  name_widths: Vec<usize>,
}

impl GridLayout {
  const COL_GAP: usize = 2;
  /// Cap on how many rows of the grid we render at once. Beyond this, the
  /// selector scrolls vertically to keep the cursor visible.
  pub const MAX_VISIBLE_ROWS: usize = 10;

  pub fn from_metrics(cells: &[CellMetrics], t_cols: usize) -> Self {
    if cells.is_empty() {
      return Self::default();
    }

    let mut num_cols = cells.len();
    while num_cols > 1 {
      let (name_widths, desc_widths) = Self::col_dims_for(cells, num_cols);
      let col_widths = Self::col_widths_from(&name_widths, &desc_widths);
      let total: usize = col_widths.iter().sum::<usize>() + Self::COL_GAP * (num_cols - 1);
      if total <= t_cols {
        break;
      }
      num_cols -= 1;
    }

    // Re-tighten num_cols against the chosen num_rows. The horizontal-fit
    // loop picks a `num_cols` whose rows-per-column is `ceil(N/num_cols)`,
    // but for some inputs that leaves trailing columns completely empty
    // (e.g. N=31 with num_cols=15 → num_rows=3 → only 11 columns get any
    // cells). Recomputing from num_rows gives the minimum num_cols that
    // covers all cells, which avoids empty-column edge cases everywhere.
    let num_rows = cells.len().div_ceil(num_cols);
    let num_cols = cells.len().div_ceil(num_rows);

    let (name_widths, desc_widths) = Self::col_dims_for(cells, num_cols);
    let mut col_widths = Self::col_widths_from(&name_widths, &desc_widths);

    // If the ideal column widths overflow the terminal (because some
    // description is enormous and the loop bottomed out at num_cols=1),
    // cap each column at its fair share of the available width. The
    // render path then truncates oversize descriptions to fit.
    let total_gaps = Self::COL_GAP * num_cols.saturating_sub(1);
    let budget = t_cols.saturating_sub(total_gaps);
    let total: usize = col_widths.iter().sum();
    if total > budget && num_cols > 0 {
      let max_per_col = budget / num_cols;
      for cw in &mut col_widths {
        *cw = (*cw).min(max_per_col);
      }
    }

    Self {
      top_left: 0,
      rows: num_rows,
      col_widths,
      name_widths,
    }
  }

  /// For column-major layout with `n` columns (Tab moves down within a
  /// column, wrapping into the next column at the bottom), compute the max
  /// name-width and max description-width in each column. Column c spans
  /// cells `c * num_rows .. (c+1) * num_rows` clamped to `cells.len()`.
  fn col_dims_for(cells: &[CellMetrics], n: usize) -> (Vec<usize>, Vec<usize>) {
    let num_rows = cells.len().div_ceil(n);
    let col_slice = |c: usize| {
      // Clamp both start and end against `cells.len()`. After the
      // tightening in `from_metrics` no column should be empty, but during
      // the search loop in `from_metrics` we may evaluate a num_cols value
      // that's still loose; clamping start prevents the slice from
      // panicking when start > len.
      let start = (c * num_rows).min(cells.len());
      let end = ((c + 1) * num_rows).min(cells.len());
      &cells[start..end]
    };
    let name_widths = (0..n)
      .map(|c| col_slice(c).iter().map(|m| m.name).max().unwrap_or(0))
      .collect();
    let desc_widths = (0..n)
      .map(|c| col_slice(c).iter().map(|m| m.desc).max().unwrap_or(0))
      .collect();
    (name_widths, desc_widths)
  }

  /// `name_max + 2 + desc_max` when there's at least one description in the
  /// column, otherwise just `name_max`.
  fn col_widths_from(name_widths: &[usize], desc_widths: &[usize]) -> Vec<usize> {
    name_widths
      .iter()
      .zip(desc_widths.iter())
      .map(|(&n, &d)| if d > 0 { n + 2 + d } else { n })
      .collect()
  }

  pub fn cols(&self) -> usize {
    self.col_widths.len()
  }
}

#[derive(Debug, Default, Clone)]
pub(crate) struct GridSelector {
  candidates: Vec<Candidate>,
  cursor: ClampedUsize,
  old_layout: Option<GridLayout>,
  page_size: usize,

  /// Column to return to after drawing
  prompt_cursor_col: usize,
  /// True if the user presses tab again after activating
  has_selection: bool,
}

impl GridSelector {
  pub fn new() -> Self {
    Self::default()
  }
  fn reset(&mut self) {
    *self = Self::new();
  }

  pub fn activate(&mut self, candidates: Vec<Candidate>) {
    self.candidates = candidates;
    self.cursor = ClampedUsize::new(0, self.candidates.len(), true);
    self.old_layout = None;
    self.page_size = 0;
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

  pub fn set_prompt_line_context(&mut self, _line_width: usize, cursor_col: usize) {
    self.prompt_cursor_col = cursor_col;
  }

  pub fn clear(&mut self) {
    if let Some(layout) = self.old_layout.take() {
      // cursor is in the editor b
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

  pub fn get_metrics(&self) -> Vec<CellMetrics> {
    // Per-candidate display metrics: name width + description width (with
    // parens, 0 if absent). The layout uses these to compute per-column
    // name- and description-maxes so descriptions align vertically within
    // each column.
    self
      .candidates
      .iter()
      .map(|c| {
        let name = calc_str_width(c.as_str());
        let desc = c
          .desc
          .as_ref()
          .filter(|d| !d.is_empty())
          .map_or(0, |d| calc_str_width(d) + 2);
        CellMetrics { name, desc }
      })
      .collect()
  }

  pub fn next_page(&mut self) {
    self.has_selection = true;
    let len = self.candidates.len();
    if len == 0 {
      return;
    }
    let next_stop = self.next_page_stop();
    self.cursor.set(next_stop);
  }

  fn next_page_stop(&self) -> usize {
    if self.page_size == 0 || self.candidates.is_empty() {
      return 0;
    }
    let current_page = self.cursor.get() / self.page_size;
    let total_pages = self.candidates.len().div_ceil(self.page_size);
    let next_page = (current_page + 1) % total_pages;
    next_page * self.page_size
  }

  pub fn prev_page(&mut self) {
    self.has_selection = true;
    let len = self.candidates.len();
    if len == 0 {
      return;
    }
    let prev_stop = self.prev_page_stop();
    self.cursor.set(prev_stop);
  }

  fn prev_page_stop(&self) -> usize {
    if self.page_size == 0 || self.candidates.is_empty() {
      return 0;
    }
    let current_page = self.cursor.get() / self.page_size;
    let total_pages = self.candidates.len().div_ceil(self.page_size);
    let prev_page = (current_page + total_pages - 1) % total_pages;
    prev_page * self.page_size
  }

  pub fn draw(&mut self) -> usize {
    if self.candidates.is_empty() {
      return 0;
    }

    let t_cols = Shed::term(Terminal::t_cols);

    let metrics = self.get_metrics();
    let mut layout = GridLayout::from_metrics(&metrics, t_cols);

    let cursor_pos = self.cursor.get();
    let num_cols = layout.cols();

    // the grid is split into pages
    // a page is 'num_cols * MAX_VISIBLE_ROWS' cells.
    // the current page number is 'cursor / page_size'
    self.page_size = num_cols.saturating_mul(GridLayout::MAX_VISIBLE_ROWS).max(1);
    let total_pages = self.candidates.len().div_ceil(self.page_size);
    let current_page = cursor_pos / self.page_size;
    let page_start = current_page * self.page_size;
    let page_end = (page_start + self.page_size).min(self.candidates.len());
    let page_cells = page_end - page_start;
    let page_rows = page_cells.div_ceil(num_cols);

    // break the line to move under the prompt
    write_term!("\n").ok();

    // Column-major within the page: cell at (col c, row r) within the page
    // is at page-relative index `c * page_rows + r`. Absolute candidate
    // index is `page_start + page_rel_idx`.
    for r in 0..page_rows {
      for c in 0..num_cols {
        let page_rel_idx = c * page_rows + r;
        if page_rel_idx >= page_cells {
          break;
        }
        let idx = page_start + page_rel_idx;
        if idx >= self.candidates.len() {
          break;
        }

        let cand = &self.candidates[idx];
        let name = cand.as_str();
        let name_w = metrics[idx].name;
        let col_name_max = layout.name_widths[c];
        let col_w = layout.col_widths[c];

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

        // Inter-column gap (skip after the last column, or when the next
        // column has no cell at this row within the page).
        if c + 1 < num_cols {
          let next_page_rel = (c + 1) * page_rows + r;
          if next_page_rel < page_cells {
            write_term!("{}", " ".repeat(GridLayout::COL_GAP)).ok();
          }
        }
      }
      if r + 1 < page_rows {
        write_term!("\n").ok();
      }
    }

    // When the candidate list spans multiple pages, append a "page n/N"
    // counter underneath the grid so the user knows where they are.
    let counter_rows = if total_pages > 1 {
      write_term!("\n\x1b[4mpage {}/{}\x1b[24m", current_page + 1, total_pages,).ok();
      1
    } else {
      0
    };
    let rows_drawn = page_rows + counter_rows;

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

    layout.top_left = 0;
    // Store the *visible* row count so clear() wipes exactly what we drew.
    layout.rows = rows_drawn;
    self.old_layout = Some(layout);

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
    let metrics = self.selector.get_metrics();
    let layout = GridLayout::from_metrics(&metrics, t_cols);
    // Page-based reporting: the visible portion is at most one page of
    // `num_cols * MAX_VISIBLE_ROWS` cells. Counter row appears when the
    // list spans more than one page.
    let num_cols = layout.cols();
    let page_size = num_cols.saturating_mul(GridLayout::MAX_VISIBLE_ROWS).max(1);
    let total_pages = self.selector.candidates.len().div_ceil(page_size);
    let cursor_pos = self.selector.cursor.get();
    let current_page = cursor_pos / page_size;
    let page_start = current_page * page_size;
    let page_end = (page_start + page_size).min(self.selector.candidates.len());
    let page_cells = page_end - page_start;
    let page_rows = page_cells.div_ceil(num_cols.max(1));
    let counter_rows = usize::from(total_pages > 1);
    Some(page_rows + counter_rows)
  }

  fn handle_key(&mut self, key: KeyEvent) -> ShResult<CompResponse> {
    match key {
      key!(Tab) => {
        self.selector.next_candidate();
        // Live preview: splice the now-selected candidate into the buffer
        // so the user sees what they'd accept. Completer stays active.
        match self.selected_candidate() {
          Some(cand) => Ok(CompResponse::Preview(cand)),
          None => Ok(CompResponse::Consumed),
        }
      }
      key!(Shift + Tab) => {
        self.selector.prev_candidate();
        match self.selected_candidate() {
          Some(cand) => Ok(CompResponse::Preview(cand)),
          None => Ok(CompResponse::Consumed),
        }
      }
      key!(Ctrl + 'f') | key!(PageDown) => {
        self.selector.next_page();
        match self.selected_candidate() {
          Some(cand) => Ok(CompResponse::Preview(cand)),
          None => Ok(CompResponse::Consumed),
        }
      }
      key!(Ctrl + 'b') | key!(PageUp) => {
        self.selector.prev_page();
        match self.selected_candidate() {
          Some(cand) => Ok(CompResponse::Preview(cand)),
          None => Ok(CompResponse::Consumed),
        }
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
