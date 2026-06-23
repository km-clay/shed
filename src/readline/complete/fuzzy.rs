use crate::state::terminal::Terminal;

use super::{
  Candidate, CompMatch, CompResponse, Completer, ShResult, Shed, SimpleCompleter,
  editmode::{EditMode, Emacs},
  grid::{GridLayout, truncate_to_width},
  key,
  keys::{KeyCode as C, KeyEvent as K},
  linebuf::LineBuf,
  state::terminal::{TermGuard, calc_str_width},
  write_term,
};

/// Collapse a candidate to a single display row: newlines become the visible
/// `␤` glyph, tabs expand to spaces, and other control bytes are dropped. This
/// lets multi-line commands occupy exactly one cell and reuse grid truncation.
pub(crate) fn one_line(s: &str) -> String {
  let mut out = String::with_capacity(s.len());
  for ch in s.chars() {
    match ch {
      '\n' | '\r' => out.push('\u{2424}'),
      '\t' => out.push_str("    "),
      c if c.is_control() => {}
      c => out.push(c),
    }
  }
  out
}

/// Resets the emphasis attributes (intensity, underline, color) without
/// disturbing surrounding reverse-video or dim state.
const EMPH_OFF: &str = "\x1b[22;24;39m";

pub(crate) fn emphasize_fuzzy(text: &str, hl: impl Fn(usize) -> bool) -> String {
  emphasize(text, hl, "\x1b[1;4;33m")
}

pub(crate) fn emphasize_grid(text: &str, hl: impl Fn(usize) -> bool) -> String {
  emphasize(text, hl, "\x1b[1;4m")
}

/// Wrap each contiguous run of characters where `hl(i)` is true in the emphasis
/// SGR. Inserts only zero-width escapes, so callers must measure widths on the
/// plain text before calling this.
fn emphasize(text: &str, hl: impl Fn(usize) -> bool, emph_on: &str) -> String {
  let mut out = String::with_capacity(text.len());
  let mut active = false;
  for (i, ch) in text.chars().enumerate() {
    if hl(i) && !active {
      out.push_str(emph_on);
      active = true;
    } else if !hl(i) && active {
      out.push_str(EMPH_OFF);
      active = false;
    }
    out.push(ch);
  }
  if active {
    out.push_str(EMPH_OFF);
  }
  out
}

/// Char indices in `text` that a left-to-right fuzzy match of `query` lands on
/// (the same greedy walk the scorer uses). Empty if `query` isn't a full match.
fn match_positions(text: &str, query: &str) -> Vec<usize> {
  let q: Vec<char> = query.chars().collect();
  if q.is_empty() {
    return vec![];
  }
  let mut qi = 0;
  let mut out = Vec::with_capacity(q.len());
  for (i, ch) in text.chars().enumerate() {
    if qi < q.len() && ch.eq_ignore_ascii_case(&q[qi]) {
      out.push(i);
      qi += 1;
    }
  }
  if qi == q.len() { out } else { vec![] }
}

#[derive(Clone, Default, Debug)]
pub(crate) struct ClampedUsize {
  val: usize,
  max: usize,
  wrap: bool,
}

impl ClampedUsize {
  pub fn new(val: usize, max: usize, wrap: bool) -> Self {
    Self { val, max, wrap }
  }
  pub fn get(&self) -> usize {
    self.val
  }
  pub fn set(&mut self, val: usize) {
    self.val = val.min(self.max.saturating_sub(1));
  }
  pub fn set_max(&mut self, max: usize) {
    self.max = max;
    if self.val >= self.max && self.max > 0 {
      self.val = self.max - 1;
    }
  }
  pub fn wrap_add(&mut self, n: usize) {
    if self.max == 0 {
      return;
    }
    if self.wrap {
      self.val = (self.val + n) % self.max;
    } else {
      self.val = (self.val + n).min(self.max.saturating_sub(1));
    }
  }
  pub fn wrap_sub(&mut self, n: usize) {
    if self.max == 0 {
      return;
    }
    if self.wrap {
      self.val = (self.val + self.max - (n % self.max)) % self.max;
    } else {
      self.val = self.val.saturating_sub(n);
    }
  }

  pub fn sub(&mut self, n: usize) {
    self.val = self.val.saturating_sub(n);
  }
  pub fn add(&mut self, n: usize) {
    self.val = self.val.saturating_add(n).min(self.max.saturating_sub(1));
  }
}

#[derive(Default, Debug, Clone)]
pub(crate) struct ScoredCandidate {
  pub candidate: Candidate,
  pub score: Option<i32>,
  pub penalize_len_diff: bool,
}

impl ScoredCandidate {
  const BONUS_BOUNDARY: i32 = 10;
  const BONUS_CONSECUTIVE: i32 = 8;
  const BONUS_FIRST_CHAR: i32 = 5;
  const PENALTY_GAP_START: i32 = 3;
  const PENALTY_GAP_EXTEND: i32 = 1;

  pub fn new(candidate: Candidate) -> Self {
    Self {
      candidate,
      score: None,
      penalize_len_diff: false,
    }
  }
  pub fn with_len_penalty(mut self, enable: bool) -> Self {
    self.penalize_len_diff = enable;
    self
  }
  fn is_word_bound(prev: char, curr: char, query_ch: char) -> bool {
    match prev {
      '/' | '_' | '-' | '.' | ' ' => true,
      c if c.is_lowercase() && curr.is_uppercase() => query_ch.is_uppercase(),
      _ => false,
    }
  }
  pub fn fuzzy_score(&mut self, other: &str) -> i32 {
    if other.is_empty() {
      self.score = Some(0);
      return 0;
    }

    let query_chars: Vec<char> = other.chars().collect();
    let candidate_chars: Vec<char> = self.candidate.chars().collect();
    let mut indices = vec![];
    let mut qi = 0;
    for (ci, c_ch) in self.candidate.chars().enumerate() {
      if qi < query_chars.len() && c_ch.eq_ignore_ascii_case(&query_chars[qi]) {
        indices.push(ci);
        qi += 1;
      }
    }

    if indices.len() != query_chars.len() {
      self.score = Some(i32::MIN);
      return i32::MIN;
    }

    let mut score: i32 = 0;

    for (i, &idx) in indices.iter().enumerate() {
      if idx == 0 {
        score += Self::BONUS_FIRST_CHAR;
      }

      if idx == 0
        || Self::is_word_bound(
          candidate_chars[idx - 1],
          candidate_chars[idx],
          query_chars[i],
        )
      {
        score += Self::BONUS_BOUNDARY;
      }

      if i > 0 {
        let gap = idx - indices[i - 1] - 1;
        if gap == 0 {
          score += Self::BONUS_CONSECUTIVE;
        } else {
          score -= Self::PENALTY_GAP_START + (gap as i32 - 1) * Self::PENALTY_GAP_EXTEND;
        }
      }
    }

    if self.penalize_len_diff {
      let len_diff = (candidate_chars.len() as isize - query_chars.len() as isize).unsigned_abs();
      let len_penalty = (len_diff as i32) * 2;
      score -= len_penalty;
    }

    self.score = Some(score);
    score
  }
}

impl From<String> for ScoredCandidate {
  fn from(content: String) -> Self {
    Self {
      candidate: content.into(),
      score: None,
      penalize_len_diff: false,
    }
  }
}

impl From<Candidate> for ScoredCandidate {
  fn from(candidate: Candidate) -> Self {
    Self {
      candidate,
      score: None,
      penalize_len_diff: false,
    }
  }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct FuzzyLayout {
  /// Total rows drawn below the prompt line (query line + grid rows + counter).
  rows: usize,
}

#[derive(Default, Debug)]
pub(crate) struct QueryEditor {
  mode: Emacs,
  linebuf: LineBuf,
}

impl QueryEditor {
  pub fn clear(&mut self) {
    self.linebuf = LineBuf::new();
    self.mode = Emacs::default();
  }
  pub fn handle_key(&mut self, key: K) -> ShResult<()> {
    let Some(cmd) = self.mode.handle_key(key) else {
      return Ok(());
    };
    self.linebuf.exec_cmd(&cmd)
  }
}

pub(crate) enum SelectorResponse {
  Accept(Candidate),
  /// Selection changed; the caller may preview it without committing.
  Preview(Candidate),
  Dismiss,
  Consumed,
}

#[derive(Default, Debug)]
pub(crate) struct FuzzySelector {
  query: QueryEditor,
  filtered: Vec<ScoredCandidate>,
  candidates: Vec<Candidate>,
  cursor: ClampedUsize,
  old_layout: Option<FuzzyLayout>,
  /// Index of the leftmost visible column (each column is `MAX_VISIBLE_ROWS`
  /// tall). Layout only ever touches the columns visible from here.
  scroll_col: usize,
  prompt_cursor_col: usize,
  _mouse_guard: Option<TermGuard>,
}

#[derive(Debug)]
pub(crate) struct FuzzyCompleter {
  completer: SimpleCompleter,
  pub selector: FuzzySelector,
}

impl FuzzySelector {
  /// Bright arrow used for both the query prompt and the selected cell.
  const PROMPT: &str = "\x1b[1;36m►\x1b[0m";
  const SELECTED: &str = "►";
  /// Every cell is prefixed with a 2-column leader (`► ` or dim `· `).
  const LEADER_W: usize = 2;

  pub fn new(_title: impl Into<String>) -> Self {
    Self {
      query: QueryEditor::default(),
      filtered: vec![],
      candidates: vec![],
      cursor: ClampedUsize::new(0, 0, true),
      old_layout: None,
      scroll_col: 0,
      prompt_cursor_col: 0,
      _mouse_guard: Some(Shed::term_mut(|t| t.mouse_support_guard(true.into()))),
    }
  }

  /// Retained for API compatibility; the grid layout doesn't number rows.
  pub fn number_candidates(self, _enable: bool) -> Self {
    self
  }

  pub fn candidates(&self) -> &[Candidate] {
    &self.candidates
  }

  pub fn filtered(&self) -> &[ScoredCandidate] {
    &self.filtered
  }

  pub fn activate(&mut self, candidates: Vec<Candidate>) {
    self.candidates = candidates;
    self.score_candidates();
  }

  pub fn set_query(&mut self, query: &str) {
    self.query.linebuf = LineBuf::new().with_initial(query, query.len());
    self.score_candidates();
  }

  pub fn reset_query(&mut self) {
    self.query.clear();
    self.score_candidates();
  }

  pub fn selected_candidate(&self) -> Option<Candidate> {
    self
      .filtered
      .get(self.cursor.get())
      .map(|c| c.candidate.clone())
  }

  pub fn set_prompt_line_context(&mut self, _line_width: usize, cursor_col: usize) {
    self.prompt_cursor_col = cursor_col;
  }

  pub fn score_candidates(&mut self) {
    let mut scored: Vec<_> = self
      .candidates
      .clone()
      .into_iter()
      .filter_map(|c| {
        let mut sc = ScoredCandidate::new(c);
        let score = sc.fuzzy_score(&self.query.linebuf.to_string());
        if score > i32::MIN { Some(sc) } else { None }
      })
      .collect();
    scored.sort_by_key(|sc| sc.score.unwrap_or(i32::MIN));
    scored.reverse();
    self.cursor.set_max(scored.len());
    // Highlight the top match and scroll home after every (re)score.
    self.cursor.set(0);
    self.scroll_col = 0;
    self.filtered = scored;
  }

  /// `(max name width, max desc width incl. parens)` over a column's
  /// candidates. Names and descriptions are maxed independently so the
  /// aligned layout reserves room for the widest of each, not the widest
  /// single name+desc pair.
  fn col_dims(cands: &[ScoredCandidate]) -> (usize, usize) {
    let name = cands
      .iter()
      .map(|sc| calc_str_width(&one_line(&sc.candidate.display())))
      .max()
      .unwrap_or(0);
    let desc = cands
      .iter()
      .filter_map(|sc| sc.candidate.desc().filter(|d| !d.is_empty()))
      .map(|d| calc_str_width(d) + 2) // includes parens
      .max()
      .unwrap_or(0);
    (name, desc)
  }

  /// Display width of a column box: leader + name + (2-col gap + parenthesized
  /// desc) when any desc is present. Used for both layout and scrolling, which
  /// must agree on column widths.
  fn col_box_width(cands: &[ScoredCandidate]) -> usize {
    let (name, desc) = Self::col_dims(cands);
    Self::LEADER_W + name + if desc > 0 { 2 + desc } else { 0 }
  }

  /// Per-visible-column widths (each column a fixed `MAX_VISIBLE_ROWS` tall).
  fn visible_window(&self, t_cols: usize) -> Vec<usize> {
    super::grid::pack_columns(
      self.filtered.len(),
      self.scroll_col,
      GridLayout::MAX_VISIBLE_ROWS,
      t_cols,
      |start, end| Self::col_box_width(&self.filtered[start..end]),
    )
  }

  /// Scroll horizontally so the selected cell's column is on screen.
  fn ensure_cursor_visible(&mut self, t_cols: usize) {
    let cursor = self.cursor.get();
    let n = self.filtered.len();
    let filtered = &self.filtered;
    super::grid::scroll_into_view(
      cursor,
      &mut self.scroll_col,
      n,
      GridLayout::MAX_VISIBLE_ROWS,
      t_cols,
      |start, end| Self::col_box_width(&filtered[start..end]),
    );
  }

  /// Render the query input with a reverse-video block standing in for the
  /// hardware cursor, which stays parked on the prompt line.
  fn query_display(&self) -> String {
    let chars: Vec<char> = self.query.linebuf.to_string().chars().collect();
    let cur = self.query.linebuf.cursor_to_flat().min(chars.len());
    let before: String = chars[..cur].iter().collect();
    let at = chars.get(cur).copied().unwrap_or(' ');
    let after: String = chars
      .get(cur + 1..)
      .map(|s| s.iter().collect())
      .unwrap_or_default();
    format!("{before}\x1b[7m{at}\x1b[27m{after}")
  }

  pub fn predicted_rows(&self) -> usize {
    if self.candidates.is_empty() && self.filtered.is_empty() {
      return 0;
    }
    // Query line plus a "(no matches)" row.
    if self.filtered.is_empty() {
      return 2;
    }
    let rows = GridLayout::MAX_VISIBLE_ROWS;
    let n = self.filtered.len();
    let first = self.scroll_col * rows;
    let grid_rows = rows.min(n.saturating_sub(first)).max(1);
    // Query line + grid rows + the "Items x to y of z" counter.
    1 + grid_rows + 1
  }

  /// Response for a key that moved/refiltered the selection: preview the newly
  /// selected candidate, or just consume the key if nothing matches.
  fn nav_response(&self) -> SelectorResponse {
    match self.selected_candidate() {
      Some(cand) => SelectorResponse::Preview(cand),
      None => SelectorResponse::Consumed,
    }
  }

  pub fn handle_key(&mut self, key: K) -> ShResult<SelectorResponse> {
    match key {
      // Pointer events are consumed but unhandled for now; hit-testing a
      // column-major paged grid needs a cell map we haven't built yet.
      K(C::MousePos(..), _) | K(C::LeftClick(..), _) => Ok(SelectorResponse::Consumed),
      key!(Ctrl + 'd') | key!(Esc) => {
        self.filtered.clear();
        Ok(SelectorResponse::Dismiss)
      }
      key!(Enter) => match self.filtered.get(self.cursor.get()) {
        Some(selected) => Ok(SelectorResponse::Accept(selected.candidate.clone())),
        None => Ok(SelectorResponse::Dismiss),
      },
      key!(Tab) | key!(Down) => {
        self.cursor.wrap_add(1);
        Ok(self.nav_response())
      }
      // Up clamps at the top rather than wrapping to the far end of a long
      // history (a ~12k-index jump is jarring); Down still cycles forward.
      key!(Shift + Tab) | key!(Up) => {
        self.cursor.sub(1);
        Ok(self.nav_response())
      }
      // One column over on the same row. Right wraps at the last column; Left
      // clamps at the first so it can't leap to the far end of the list.
      key!(Right) => {
        let next = super::grid::step_column(
          self.cursor.get(),
          self.filtered.len(),
          GridLayout::MAX_VISIBLE_ROWS,
          true,
          true,
        );
        self.cursor.set(next);
        Ok(self.nav_response())
      }
      key!(Left) => {
        let next = super::grid::step_column(
          self.cursor.get(),
          self.filtered.len(),
          GridLayout::MAX_VISIBLE_ROWS,
          false,
          false,
        );
        self.cursor.set(next);
        Ok(self.nav_response())
      }
      key!(ScrollDown) => {
        self.cursor.add(1);
        Ok(self.nav_response())
      }
      key!(ScrollUp) => {
        self.cursor.sub(1);
        Ok(self.nav_response())
      }
      key!(PageDown) | key!(Ctrl + 'f') => {
        let step = self
          .visible_window(Shed::term(Terminal::t_cols))
          .len()
          .max(1)
          * GridLayout::MAX_VISIBLE_ROWS;
        self.cursor.add(step);
        Ok(self.nav_response())
      }
      key!(PageUp) | key!(Ctrl + 'b') => {
        let step = self
          .visible_window(Shed::term(Terminal::t_cols))
          .len()
          .max(1)
          * GridLayout::MAX_VISIBLE_ROWS;
        self.cursor.sub(step);
        Ok(self.nav_response())
      }
      key!(Ctrl + 'c') => {
        self.query.clear();
        self.score_candidates();
        Ok(self.nav_response())
      }
      _ => {
        self.query.handle_key(key)?;
        self.score_candidates();
        Ok(self.nav_response())
      }
    }
  }

  pub fn draw(&mut self) -> usize {
    let t_cols = Shed::term(Terminal::t_cols);
    let rows = GridLayout::MAX_VISIBLE_ROWS;
    self.ensure_cursor_visible(t_cols);

    // Query line, one row below the prompt.
    write_term!("\n").ok();
    write_term!("{} {}", Self::PROMPT, self.query_display()).ok();
    let mut rows_drawn = 1usize;

    if self.filtered.is_empty() {
      write_term!("\n").ok();
      write_term!("\x1b[2m(no matches)\x1b[22m").ok();
      rows_drawn += 1;
    } else {
      let col_widths = self.visible_window(t_cols);
      let num_cols = col_widths.len().max(1);
      let cursor_pos = self.cursor.get();
      let n = self.filtered.len();
      let first = self.scroll_col * rows;
      // The first visible column has the lowest indices, so it's the tallest.
      let grid_rows = rows.min(n - first);
      let visible_end = ((self.scroll_col + num_cols) * rows).min(n);
      let query = self.query.linebuf.to_string();

      // Column-major: cell (col c, row r) is candidate `(scroll_col + c) * rows + r`.
      for r in 0..grid_rows {
        write_term!("\n").ok();
        rows_drawn += 1;
        for (c, width) in col_widths.iter().enumerate() {
          let idx = (self.scroll_col + c) * rows + r;
          if idx >= n {
            break; // later columns at this row are exhausted too
          }

          let name_plain = one_line(&self.filtered[idx].candidate.display());
          let name_w = calc_str_width(&name_plain);
          // Matched positions index the name (the cell's prefix), so they stay
          // valid; the name is emphasized and the desc is laid out after it.
          let positions = match_positions(&name_plain, &query);
          let name = emphasize_fuzzy(&name_plain, |i| positions.binary_search(&i).is_ok());

          // Per-column max name width, so descriptions align (like the grid).
          let col_start = (self.scroll_col + c) * rows;
          let col_end = (col_start + rows).min(n);
          let (col_name_max, _) = Self::col_dims(&self.filtered[col_start..col_end]);

          let avail = width.saturating_sub(Self::LEADER_W);
          let is_selected = idx == cursor_pos;

          let cell = match self.filtered[idx]
            .candidate
            .desc()
            .filter(|d| !d.is_empty())
          {
            Some(desc) => {
              // Aligned position is after col_name_max + a 2-col gap; if the
              // desc doesn't fit there it may extend left into the name-pad,
              // and past that it truncates with an ellipsis.
              let desc_w_full = calc_str_width(desc) + 2; // includes parens
              let aligned_avail = avail.saturating_sub(col_name_max + 2);
              let max_extend_avail = avail.saturating_sub(name_w + 2);
              let (pad_chars, desc_text) = if desc_w_full <= aligned_avail {
                (col_name_max.saturating_sub(name_w), format!("({desc})"))
              } else if desc_w_full <= max_extend_avail {
                let need = desc_w_full - aligned_avail;
                let pad = col_name_max.saturating_sub(name_w).saturating_sub(need);
                (pad, format!("({desc})"))
              } else {
                let truncated = truncate_to_width(desc, max_extend_avail.saturating_sub(3));
                (0, format!("({truncated}…)"))
              };
              let name_pad = " ".repeat(pad_chars);
              let used = name_w + pad_chars + 2 + calc_str_width(&desc_text);
              let trailing = " ".repeat(avail.saturating_sub(used));
              if is_selected {
                format!("{name}{name_pad}  {desc_text}{trailing}")
              } else {
                format!("{name}{name_pad}  \x1b[2m{desc_text}\x1b[22m{trailing}")
              }
            }
            None => {
              let trailing = " ".repeat(avail.saturating_sub(name_w));
              format!("{name}{trailing}")
            }
          };

          if is_selected {
            write_term!("{} \x1b[7m{cell}\x1b[27m", Self::SELECTED).ok();
          } else {
            write_term!("\x1b[2m·\x1b[22m {cell}").ok();
          }

          // Inter-column gap, skipped when the next column has no cell here.
          if c + 1 < num_cols && (self.scroll_col + c + 1) * rows + r < n {
            write_term!("{}", " ".repeat(GridLayout::COL_GAP)).ok();
          }
        }
      }

      write_term!("\n").ok();
      write_term!(
        "\x1b[2mItems {} to {} of {}\x1b[22m",
        first + 1,
        visible_end,
        n
      )
      .ok();
      rows_drawn += 1;
    }

    // Park the hardware cursor back on the prompt line (the grid contract). The
    // query's visible cursor is faked with reverse video in `query_display`.
    write_term!("\x1b[{rows_drawn}A\r").ok();
    if self.prompt_cursor_col > 0 {
      write_term!("\x1b[{}C", self.prompt_cursor_col).ok();
    }

    self.old_layout = Some(FuzzyLayout { rows: rows_drawn });
    rows_drawn
  }

  pub fn clear(&mut self) {
    if let Some(layout) = self.old_layout.take() {
      for _ in 0..layout.rows {
        write_term!("\x1b[1B\x1b[2K").ok();
      }
      if layout.rows > 0 {
        write_term!("\x1b[{}A", layout.rows).ok();
      }
      write_term!("\r").ok();
      if self.prompt_cursor_col > 0 {
        write_term!("\x1b[{}C", self.prompt_cursor_col).ok();
      }
    }
  }
}

impl Default for FuzzyCompleter {
  fn default() -> Self {
    Self {
      completer: SimpleCompleter::default(),
      selector: FuzzySelector::new("Complete"),
    }
  }
}

impl Completer for FuzzyCompleter {
  fn all_candidates(&self) -> Vec<Candidate> {
    self.selector.candidates.clone()
  }
  fn set_prompt_line_context(&mut self, line_width: usize, cursor_col: usize) {
    self
      .selector
      .set_prompt_line_context(line_width, cursor_col);
  }
  fn reset_stay_active(&mut self) {
    self.selector.reset_query();
  }
  fn get_completed_line(&self, candidate: &str) -> String {
    log::debug!("Getting completed line for candidate: {candidate}");

    let selected = self.selector.selected_candidate().unwrap_or_default();
    let (start, end) = self.completer.token_span;
    // Wholesale replace `token_span` with the candidate. See
    // `SimpleCompleter::get_completed_line` for the rationale.
    let ret = format!(
      "{}{}{}",
      &self.completer.original_input[..start],
      selected.as_str(),
      &self.completer.original_input[end..],
    );
    log::debug!("Completed line: {ret}");
    ret
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
    let candidates: Vec<_> = self.completer.candidates.clone();
    if candidates.is_empty() {
      self.completer.reset();
      return Ok(None);
    } else if candidates.len() == 1 {
      self.selector.filtered = candidates.into_iter().map(ScoredCandidate::from).collect();
      let selected = self.selector.filtered[0].candidate.content().to_string();
      let completed = self.get_completed_line(&selected);
      // Preserve the inner completer's match-kind; only the spliced line changes.
      return Ok(inner.map(|m| m.with_line(completed)));
    }
    self.selector.activate(candidates);
    Ok(None)
  }

  fn predicted_rows(&self) -> Option<usize> {
    Some(self.selector.predicted_rows())
  }

  fn handle_key(&mut self, key: K) -> ShResult<CompResponse> {
    match self.selector.handle_key(key)? {
      SelectorResponse::Accept(s) => Ok(CompResponse::Accept(s)),
      SelectorResponse::Preview(s) => Ok(CompResponse::Preview(s)),
      SelectorResponse::Dismiss => Ok(CompResponse::Dismiss),
      SelectorResponse::Consumed => Ok(CompResponse::Consumed),
    }
  }
  fn clear(&mut self) {
    self.selector.clear();
  }
  fn draw(&mut self) -> usize {
    self.selector.draw()
  }
  fn reset(&mut self) {
    self.completer.reset();
    self.selector.reset_query();
  }
  fn token_span(&self) -> (usize, usize) {
    self.completer.token_span()
  }
  fn is_active(&self) -> bool {
    !self.selector.candidates.is_empty()
  }
  fn selected_candidate(&self) -> Option<Candidate> {
    self.selector.selected_candidate()
  }
  fn original_input(&self) -> &str {
    &self.completer.original_input
  }
}
