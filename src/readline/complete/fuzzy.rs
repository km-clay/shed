use crate::state::terminal::Terminal;

use super::{
  Candidate, CompMatch, CompResponse, Completer, ShResult, Shed, SimpleCompleter,
  editmode::{EditMode, Emacs},
  key,
  keys::{KeyCode as C, KeyEvent as K},
  linebuf::LineBuf,
  state::terminal::{ColorMode, Cols, Rows, TermGuard, calc_str_width},
  util, write_term,
};

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

#[derive(Debug, Clone)]
pub(crate) struct FuzzyLayout {
  top_left: usize,
  rows: usize,
  cols: usize,
  cursor_col: usize,
  /// Width of the prompt line above the `\n` that starts the fuzzy window.
  /// If PSR was drawn, this is `t_cols`; otherwise the content width.
  preceding_line_width: usize,
  /// Cursor column on the prompt line before the fuzzy window was drawn.
  preceding_cursor_col: usize,
}

#[derive(Default, Debug)]
pub(crate) struct QueryEditor {
  mode: Emacs,
  scroll_offset: usize,
  available_width: usize,
  linebuf: LineBuf,
}

impl QueryEditor {
  pub fn clear(&mut self) {
    self.linebuf = LineBuf::new();
    self.mode = Emacs::default();
    self.scroll_offset = 0;
  }
  pub fn set_available_width(&mut self, width: usize) {
    self.available_width = width;
  }
  pub fn update_scroll_offset(&mut self) {
    let cursor_pos = self.linebuf.cursor_to_flat();
    if cursor_pos < self.scroll_offset + 1 {
      self.scroll_offset = self.linebuf.cursor_to_flat().saturating_sub(1);
    }
    if cursor_pos >= self.scroll_offset + self.available_width.saturating_sub(1) {
      self.scroll_offset = self
        .linebuf
        .cursor_to_flat()
        .saturating_sub(self.available_width.saturating_sub(1));
    }
    let max_offset = self
      .linebuf
      .count_graphemes()
      .saturating_sub(self.available_width);
    self.scroll_offset = self.scroll_offset.min(max_offset);
  }
  pub fn get_window(&mut self) -> String {
    let buf_len = self.linebuf.count_graphemes();
    if buf_len <= self.available_width {
      return self.linebuf.to_string();
    }
    let start = self
      .scroll_offset
      .min(buf_len.saturating_sub(self.available_width));
    let end = (start + self.available_width).min(buf_len);
    self.linebuf.slice(start..end).unwrap_or_default()
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
  Dismiss,
  Consumed,
}

#[derive(Default, Debug)]
pub(crate) struct FuzzySelector {
  query: QueryEditor,
  filtered: Vec<ScoredCandidate>,
  candidates: Vec<Candidate>,
  cursor: ClampedUsize,
  number_candidates: bool,
  old_layout: Option<FuzzyLayout>,
  max_height: usize,
  scroll_offset: usize,
  prompt_line_width: usize,
  prompt_cursor_col: usize,
  row_map: Vec<Option<usize>>,
  hovered: Option<usize>, // index of the currently hovered candidate, if any
  title: String,
  _mouse_guard: Option<TermGuard>,
}

#[derive(Debug)]
pub(crate) struct FuzzyCompleter {
  completer: SimpleCompleter,
  pub selector: FuzzySelector,
}

impl FuzzySelector {
  const SELECTOR_GRAY: &str = "\x1b[90m▌\x1b[0m";
  const PROMPT_ARROW: &str = "\x1b[1;36m>\x1b[0m";

  pub fn new(title: impl Into<String>) -> Self {
    Self {
      max_height: 8,
      query: QueryEditor::default(),
      filtered: vec![],
      candidates: vec![],
      cursor: ClampedUsize::new(0, 0, true),
      number_candidates: false,
      old_layout: None,
      scroll_offset: 0,
      prompt_line_width: 0,
      row_map: vec![],
      prompt_cursor_col: 0,
      hovered: None,
      title: title.into(),
      _mouse_guard: Some(Shed::term_mut(|t| t.mouse_support_guard(true.into()))),
    }
  }

  pub fn number_candidates(self, enable: bool) -> Self {
    Self {
      number_candidates: enable,
      ..self
    }
  }
  fn selector_hl() -> String {
    match Shed::term(Terminal::color_mode) {
      Some(ColorMode::Truecolor) => "\x1b[38;2;200;0;120m▌\x1b[1;39;48;5;237m",
      Some(ColorMode::Palette256) => "\x1b[38;5;162m▌\x1b[1;39;48;5;237m",
      Some(ColorMode::Palette16) => "\x1b[35m▌\x1b[1;39;100m",
      None => "▌\x1b[1m",
    }
    .to_string()
  }

  fn selector_hover_hl() -> String {
    match Shed::term(Terminal::color_mode) {
      Some(ColorMode::Truecolor | ColorMode::Palette256) => "\x1b[90m▌\x1b[1;39;48;5;237m",
      Some(ColorMode::Palette16) => "\x1b[90m▌\x1b[1;39;100m",
      None => "▌\x1b[1m",
    }
    .to_string()
  }

  /// Calculate how many rows we need in order to draw this thing
  pub fn predicted_rows(&self) -> usize {
    const CHROME_ROWS: usize = 4;
    if self.candidates.is_empty() && self.filtered.is_empty() {
      return 0;
    }

    let mut cand_rows = 0usize;
    for c in self.filtered.iter().skip(self.scroll_offset) {
      let h = c.candidate.content().trim_end().lines().count().max(1);
      if cand_rows + h > self.max_height {
        cand_rows = self.max_height;
        break;
      }
      cand_rows += h;
    }

    CHROME_ROWS + cand_rows
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
    self.query.update_scroll_offset();
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

  pub fn set_prompt_line_context(&mut self, line_width: usize, cursor_col: usize) {
    self.prompt_line_width = line_width;
    self.prompt_cursor_col = cursor_col;
  }

  fn candidate_height(&self, idx: usize) -> usize {
    self.filtered.get(idx).map_or(1, |c| {
      c.candidate.content().trim_end().lines().count().max(1)
    })
  }

  fn get_window(&mut self) -> &[ScoredCandidate] {
    self.update_scroll_offset();

    let mut lines = 0;
    let mut end = self.scroll_offset;
    while end < self.filtered.len() {
      if lines >= self.max_height {
        break;
      }
      lines += self.candidate_height(end);
      end += 1;
    }

    &self.filtered[self.scroll_offset..end]
  }

  pub fn update_scroll_offset(&mut self) {
    let cursor = self.cursor.get();

    // Scroll up: cursor above window
    if cursor < self.scroll_offset {
      self.scroll_offset = cursor;
      return;
    }

    // Scroll down: work backwards from cursor to find the
    // earliest offset that fits within max_height lines.
    let mut lines = 0;
    let mut new_offset = cursor;
    loop {
      let h = self.candidate_height(new_offset);
      if lines + h > self.max_height && new_offset < cursor {
        new_offset += 1;
        break;
      }
      lines += h;
      if new_offset == 0 {
        break;
      }
      new_offset -= 1;
    }

    if new_offset > self.scroll_offset {
      self.scroll_offset = new_offset;
    }
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
    self.filtered = scored;
  }

  pub fn handle_click(&mut self, row: usize, _col: usize) -> SelectorResponse {
    let top_left = self.old_layout.as_ref().map_or(0, |l| l.top_left);
    let relative_row = row.saturating_sub(top_left);
    if let Some(idx) = self.row_map.get(relative_row).copied().flatten() {
      if self.cursor.val == idx {
        SelectorResponse::Accept(self.filtered[idx].candidate.clone())
      } else {
        self.cursor = ClampedUsize::new(idx, self.filtered.len(), true);
        SelectorResponse::Consumed
      }
    } else {
      SelectorResponse::Consumed
    }
  }

  pub fn handle_hover(&mut self, row: usize) -> SelectorResponse {
    let top_left = self.old_layout.as_ref().map_or(0, |l| l.top_left);
    let relative_row = row.saturating_sub(top_left);
    let idx = self.row_map.get(relative_row).copied().flatten();

    if self.hovered != idx {
      self.hovered = idx;
    }

    SelectorResponse::Consumed
  }

  #[expect(clippy::unnested_or_patterns)]
  pub fn handle_key(&mut self, key: K) -> ShResult<SelectorResponse> {
    match key {
      K(C::MousePos(row, _), _) => Ok(self.handle_hover(row)),
      K(C::LeftClick(row, col), _) => Ok(self.handle_click(row, col)),
      key!(Ctrl + 'd') | key!(Esc) => {
        self.filtered.clear();
        Ok(SelectorResponse::Dismiss)
      }
      key!(Enter) => {
        if let Some(selected) = self.filtered.get(self.cursor.get()) {
          Ok(SelectorResponse::Accept(selected.candidate.clone()))
        } else {
          Ok(SelectorResponse::Dismiss)
        }
      }
      key @ (key!(ScrollUp) | key!(Shift + Tab) | key!(Up)) => {
        match key {
          key!(ScrollUp) => self.cursor.sub(1), // no wrap
          key!(Up) | key!(Shift + Tab) => self.cursor.wrap_sub(1), // wrap
          _ => unreachable!(),
        }
        Ok(SelectorResponse::Consumed)
      }
      key @ (key!(ScrollDown) | key!(Tab) | key!(Down)) => {
        match key {
          key!(ScrollDown) => self.cursor.add(1),            // no wrap
          key!(Down) | key!(Tab) => self.cursor.wrap_add(1), // wrap
          _ => unreachable!(),
        }
        self.update_scroll_offset();
        Ok(SelectorResponse::Consumed)
      }
      key!(Ctrl + 'c') => {
        self.query.clear();
        self.score_candidates();
        Ok(SelectorResponse::Consumed)
      }
      _ => {
        self.query.handle_key(key)?;
        self.score_candidates();
        Ok(SelectorResponse::Consumed)
      }
    }
  }

  #[expect(clippy::too_many_lines)]
  pub fn draw(&mut self) -> usize {
    const MAX_DESC_COL: usize = 32;
    self.row_map.clear();
    let (cols, top_left) = Shed::term_mut(|t| {
      (
        t.t_cols(),
        t.get_cursor_pos()
          .ok()
          .flatten()
          .unwrap_or((Rows(0), Cols(0)))
          .0
          .0
          + 1,
      )
    });

    let pad = |content: &str, fill: &str, right_border: &str| {
      util::pad_line(content, fill, right_border, cols);
    };

    let mut row_map = vec![];
    let cursor_pos = self.cursor.get();
    let offset = self.scroll_offset;
    let number_candidates = self.number_candidates;
    let max_height = self.max_height;
    let num_filtered = self.filtered.len();
    let num_candidates = self.candidates.len();
    let min_pad = num_candidates.to_string().len().saturating_add(1).max(6);
    let hovered = self.hovered;

    self.query.set_available_width(cols.saturating_sub(6));
    self.query.update_scroll_offset();
    let query = self.query.get_window();
    let title = self.title.clone();
    let visible = self.get_window();
    let mut rows: usize = 0;

    // ╭─ Title ──────────────────╮
    let title_content = format!(
      "\n{}{} \x1b[1m{}\x1b[0m ",
      util::TOP_LEFT,
      util::HOR_LINE,
      title
    );
    pad(&title_content, util::HOR_LINE, util::TOP_RIGHT);
    rows += 1;
    row_map.push(None);

    // │ > query                  │
    let prompt_content = format!("{} {} {}", util::VERT_LINE, Self::PROMPT_ARROW, query);
    pad(&prompt_content, " ", util::VERT_LINE);
    rows += 1;

    // ├──filtered/total──────────┤
    let sep_content = format!(
      "{}{}\x1b[33m{}\x1b[0m/\x1b[33m{}\x1b[0m",
      util::TREE_LEFT,
      util::HOR_LINE.repeat(2),
      num_filtered,
      num_candidates
    );
    pad(&sep_content, util::HOR_LINE, util::TREE_RIGHT);
    rows += 1;

    // Candidate lines
    let mut lines_drawn = 0;
    let col_lim = if number_candidates {
      cols.saturating_sub(3 + min_pad)
    } else {
      cols.saturating_sub(3)
    };

    let desc_col_width = visible
      .iter()
      .filter(|sc| sc.candidate.desc.is_some())
      .map(|sc| sc.candidate.display())
      .filter_map(|s| s.trim_end().lines().next().map(calc_str_width))
      .max()
      .unwrap_or(0)
      .min(MAX_DESC_COL);

    for (i, s_cand) in visible.iter().enumerate() {
      if lines_drawn >= max_height {
        break;
      }

      let selected = i + offset == cursor_pos;
      let hovered = hovered == Some(i + offset);
      let selector = if selected {
        &Self::selector_hl()
      } else if hovered {
        &Self::selector_hover_hl()
      } else {
        Self::SELECTOR_GRAY
      };
      let mut drew_number = false;

      let mut first = true;
      let display = s_cand.candidate.display();
      for line in display.trim_end().lines() {
        if lines_drawn >= max_height {
          break;
        }

        let mut line = line.trim_end().replace('\t', "    ");
        if first {
          first = false;
          if let Some(desc) = &s_cand.candidate.desc {
            let cand_width = calc_str_width(&line);
            let pad = desc_col_width.saturating_sub(cand_width);
            line = format!("{line}{}\x1b[90m  {desc}\x1b[0m", " ".repeat(pad));
          }
        }
        if calc_str_width(&line) >= col_lim {
          line.truncate(col_lim.saturating_sub(6));
          line.push_str("...");
        }

        let left = if number_candidates && !drew_number {
          let num = i + offset + 1;
          format!(
            "{} {}\x1b[33m{num:<min_pad$}\x1b[39m{line}\x1b[0m",
            util::VERT_LINE,
            selector
          )
        } else if number_candidates {
          format!(
            "{} {}{:>min_pad$}{line}\x1b[0m",
            util::VERT_LINE,
            selector,
            ""
          )
        } else {
          format!("{} {}{line}\x1b[0m", util::VERT_LINE, selector)
        };

        pad(&left, " ", util::VERT_LINE);
        rows += 1;
        row_map.push(Some(i + offset));
        drew_number = true;
        lines_drawn += 1;
      }
    }

    // ╰──────────────────────────╯
    write_term!(
      "{}{}{}",
      util::BOT_LEFT,
      util::HOR_LINE.repeat(cols.saturating_sub(2)),
      util::BOT_RIGHT
    )
    .unwrap();
    rows += 1;
    row_map.push(None);

    // Move cursor back up to the query input line
    let lines_below_prompt = rows.saturating_sub(2);
    let cursor_in_window = self
      .query
      .linebuf
      .cursor_to_flat()
      .saturating_sub(self.query.scroll_offset);
    let cursor_col = cursor_in_window + 4;
    write_term!("\x1b[{lines_below_prompt}A\r\x1b[{cursor_col}C").unwrap();

    let new_layout = FuzzyLayout {
      top_left,
      rows,
      cols,
      cursor_col,
      preceding_line_width: self.prompt_line_width,
      preceding_cursor_col: self.prompt_cursor_col,
    };
    self.old_layout = Some(new_layout);
    self.row_map = row_map;

    rows
  }

  pub fn clear(&mut self) {
    if let Some(layout) = self.old_layout.take() {
      let new_cols = Shed::term(Terminal::t_cols);
      let total_cells = layout.rows * layout.cols;
      let physical_rows = if new_cols > 0 {
        total_cells.div_ceil(new_cols)
      } else {
        layout.rows
      };
      let cursor_offset = layout.cols + layout.cursor_col;
      let cursor_phys_row = cursor_offset.checked_div(new_cols).unwrap_or(1);
      let lines_below = physical_rows.saturating_sub(cursor_phys_row + 1);

      let gap_extra = if new_cols > 0 && layout.preceding_line_width > new_cols {
        let wrap_rows = (layout.preceding_line_width).div_ceil(new_cols);
        let cursor_wrap_row = layout.preceding_cursor_col / new_cols;
        wrap_rows.saturating_sub(cursor_wrap_row + 1)
      } else {
        0
      };

      if lines_below > 0 {
        write_term!("\x1b[{lines_below}B").unwrap();
      }
      for _ in 0..physical_rows {
        write_term!("\x1b[2K\x1b[A").unwrap();
      }
      write_term!("\x1b[2K").unwrap();
      for _ in 0..gap_extra {
        write_term!("\x1b[2K\x1b[A").unwrap();
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
