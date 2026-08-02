use std::convert::Into;

use nix::{
  errno::Errno,
  poll::{PollFd, PollFlags, PollTimeout, poll},
};

use crate::{
  flush_term, sherr,
  state::terminal::{TermCap, Terminal},
};

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

/// Render control bytes as caret notation (`^M`, `^I`, `^[`, `^?`) for the
/// single-line query field. Unlike the highlighter's `is_visualized_control`,
/// this also visualizes `\n` and `\t`: neither belongs in a one-line field, and
/// a stray newline (e.g. from Shift+Enter) would otherwise corrupt the display.
fn caret_notation(s: &str) -> String {
  let mut out = String::with_capacity(s.len());
  for ch in s.chars() {
    match ch {
      '\x7f' => out.push_str("^?"),
      c if (c as u32) < 0x20 => {
        out.push('^');
        out.push((c as u8 ^ 0x40) as char);
      }
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

/// Char indices in `text` that the best-scoring fuzzy match of `query` lands
/// on (the same optimal alignment the scorer picks, so highlights and score
/// always agree). Empty if `query` isn't a subsequence of `text`.
pub(crate) fn match_positions(text: &str, query: &str) -> Vec<usize> {
  let q: Vec<char> = query.chars().collect();
  if q.is_empty() {
    return vec![];
  }
  let text_chars: Vec<char> = text.chars().collect();
  fuzzy_align(&text_chars, &q, true)
    .map(|(_, positions)| positions)
    .unwrap_or_default()
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
    self.fuzzy_score_with(other, fuzzy_match_score)
  }
  pub fn fuzzy_score_with(&mut self, other: &str, cb: ScoreCallback) -> i32 {
    let query_chars: Vec<char> = other.chars().collect();
    let score = cb(&self.candidate, &query_chars, self.penalize_len_diff);
    self.score = Some(score);
    score
  }
}

pub(crate) fn fuzzy_match_score(
  candidate: &str,
  query_chars: &[char],
  penalize_len_diff: bool,
) -> i32 {
  if query_chars.is_empty() {
    return 0;
  }

  let candidate_chars: Vec<char> = candidate.chars().collect();
  let Some((mut score, _)) = fuzzy_align(&candidate_chars, query_chars, false) else {
    return i32::MIN;
  };

  if penalize_len_diff {
    let len_diff = (candidate_chars.len() as isize - query_chars.len() as isize).unsigned_abs();
    score -= (len_diff as i32) * 2;
  }

  score
}

/// Maximum-scoring alignment of `query` within `candidate`.
///
/// Runs an O(n·m) dynamic program (Smith–Waterman with affine gaps) using the
/// same boundary / consecutive / gap constants the greedy scorer uses. The only
/// change is that it considers *every* alignment and keeps the best one,
/// so a contiguous run like `spin` inside `... spin` beats a scattered
/// `s`…`p`…`in` walk.
///
/// Returns `(best_score, positions)`, or `None` if `query` is not a
/// subsequence of `candidate`. `positions` is filled only when `track` is set
/// (backtracking is skipped on the score-only hot path).
fn fuzzy_align(candidate: &[char], query: &[char], track: bool) -> Option<(i32, Vec<usize>)> {
  use ScoredCandidate as Sc;

  let n = candidate.len();
  let m = query.len();
  if m == 0 {
    return Some((0, vec![]));
  }
  if m > n {
    return None;
  }

  // Well below any real score, but not `i32::MIN` (leaves headroom for adds).
  const NEG: i32 = i32::MIN / 2;

  let char_bonus = |i: usize, qch: char| -> i32 {
    let mut b = 0;
    if i == 0 {
      b += Sc::BONUS_FIRST_CHAR;
    }
    if i == 0 || Sc::is_word_bound(candidate[i - 1], candidate[i], qch) {
      b += Sc::BONUS_BOUNDARY;
    }
    b
  };

  // `prev`/`curr` are the DP row for the previous / current query char:
  // `prev[i]` = best score aligning query[..=j] with query[j] landing on
  // candidate[i]. The first query char can start anywhere it matches.
  let mut prev = vec![NEG; n];
  for (i, &c) in candidate.iter().enumerate() {
    if c.eq_ignore_ascii_case(&query[0]) {
      prev[i] = char_bonus(i, query[0]);
    }
  }

  // parent[j][i] = candidate index used for query[j-1] when query[j] lands on
  // i; only needed to backtrack `positions`.
  let mut parent = if track {
    vec![vec![usize::MAX; n]; m]
  } else {
    vec![]
  };

  let mut curr = vec![NEG; n];
  for j in 1..m {
    let qch = query[j];
    curr.iter_mut().for_each(|c| *c = NEG);

    // running_gap = max over k <= i-2 of `prev[k] + k*GAP_EXTEND` (plus its
    // argmax). Adding the i-dependent term below recovers the affine gap
    // penalty for the best gapped predecessor in O(1).
    let mut running_gap = NEG;
    let mut running_gap_k = usize::MAX;

    for i in 0..n {
      let mut best = NEG;
      let mut best_k = usize::MAX;

      // Consecutive predecessor (k = i-1, no gap).
      if i >= 1 && prev[i - 1] > NEG {
        let consec = prev[i - 1] + Sc::BONUS_CONSECUTIVE;
        if consec > best {
          best = consec;
          best_k = i - 1;
        }
      }
      // Best gapped predecessor (k <= i-2), recovered from the running max.
      if running_gap > NEG {
        let gapped = running_gap - Sc::PENALTY_GAP_START - (i as i32 - 2) * Sc::PENALTY_GAP_EXTEND;
        if gapped > best {
          best = gapped;
          best_k = running_gap_k;
        }
      }

      if best > NEG && candidate[i].eq_ignore_ascii_case(&qch) {
        curr[i] = char_bonus(i, qch) + best;
        if track {
          parent[j][i] = best_k;
        }
      }

      // Fold k = i-1 into the running max so it's available for i+1.
      if i >= 1 && prev[i - 1] > NEG {
        let val = prev[i - 1] + (i as i32 - 1) * Sc::PENALTY_GAP_EXTEND;
        if val > running_gap {
          running_gap = val;
          running_gap_k = i - 1;
        }
      }
    }

    std::mem::swap(&mut prev, &mut curr);
  }

  // Best end position for the last query char.
  let mut best_i = None;
  let mut best_score = NEG;
  for (i, &s) in prev.iter().enumerate() {
    if s > best_score {
      best_score = s;
      best_i = Some(i);
    }
  }
  let best_i = best_i?;

  let positions = if track {
    let mut positions = vec![0usize; m];
    let mut i = best_i;
    for j in (1..m).rev() {
      positions[j] = i;
      i = parent[j][i];
    }
    positions[0] = i;
    positions
  } else {
    vec![]
  };

  Some((best_score, positions))
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

#[derive(Debug, Default)]
pub(crate) struct FuzzyBuilder {
  entries: Vec<(String, i32)>,
  placeholder: Option<String>,
  score_cb: Option<ScoreCallback>,
  highlight_cb: Option<HighlightCallback>,
  inline: bool,
}

impl FuzzyBuilder {
  pub fn new() -> Self {
    Self {
      inline: true,
      ..Default::default()
    }
  }

  pub fn with_inline(mut self, enable: bool) -> Self {
    self.inline = enable;
    self
  }

  pub fn with_entries(mut self, entries: Vec<(String, i32)>) -> Self {
    self.entries = entries;
    self
  }
  pub fn with_placeholder(mut self, placeholder: impl Into<String>) -> Self {
    self.placeholder = Some(placeholder.into());
    self
  }
  pub fn with_score_cb(mut self, cb: ScoreCallback) -> Self {
    self.score_cb = Some(cb);
    self
  }
  pub fn with_highlight_cb(mut self, cb: HighlightCallback) -> Self {
    self.highlight_cb = Some(cb);
    self
  }
  pub fn pick(self) -> ShResult<Option<String>> {
    if self.entries.is_empty() || Shed::term(Terminal::test_mode) {
      return Ok(None);
    }
    let Some(tty) = Shed::term(Terminal::tty) else {
      return Ok(None); // not attached to a terminal
    };

    let _raw = Shed::term_mut(Terminal::raw_mode_guard)?;

    let candidates = self
      .entries
      .into_iter()
      .map(|(text, weight)| Candidate::from(text).with_weight(weight))
      .collect();

    let inline = self.inline;
    let mut selector = FuzzySelector::new("");
    selector.set_placeholder(self.placeholder);
    selector.set_inline(inline);
    selector.set_score_cb(self.score_cb);
    selector.set_highlight_cb(self.highlight_cb);
    selector.activate(candidates);
    selector.set_prompt_line_context(0, 0);

    // The beam is set with a raw escape that bypasses `execute_control`, so the
    // terminal's tracked style stays at the pre-picker value; restore to it on exit.
    let restore_style = Shed::term(Terminal::cursor_style);
    scopeguard::defer! {
      flush_term!("{restore_style}").ok();
    };

    let tty_fd = PollFd::new(tty, PollFlags::POLLIN);
    let chosen = loop {
      selector.draw();
      let col = selector.query_cursor_col();
      let down = if inline { "" } else { "\x1b[1B" };
      flush_term!("{down}\r\x1b[{col}C\x1b[5 q").ok();

      let mut decided = None;
      match poll(&mut [tty_fd.clone()], PollTimeout::NONE) {
        Ok(0) => decided = Some(None), // eof, treat as cancel
        Ok(_) => {
          Shed::term_mut(Terminal::read)?;
          for key in Shed::term_mut(Terminal::drain_keys) {
            match selector.handle_key(key)? {
              SelectorResponse::Accept(c) => decided = Some(Some(c.as_str().to_string())),
              SelectorResponse::Dismiss => decided = Some(None),
              SelectorResponse::Preview(_) | SelectorResponse::Consumed => {}
            }
            if decided.is_some() {
              break;
            }
          }
        }
        Err(Errno::EINTR) => {}
        Err(e) => {
          selector.clear();
          return Err(sherr!(InternalErr, "fuzzy picker poll failed: {e}"));
        }
      }

      if !inline {
        flush_term!("\x1b[1A").ok();
      }
      selector.clear();
      if let Some(result) = decided {
        break result;
      }
    };

    if inline {
      flush_term!("\r\x1b[2K").ok();
    }
    Ok(chosen)
  }
}

/// Pick the best fuzzy match for `query` with no UI, breaking ties by weight.
/// Used for the non-interactive `zd <query>` jump.
pub(crate) fn fuzzy_best_match(
  query: &str,
  entries: Vec<(String, i32)>,
  cb: Option<ScoreCallback>,
  transform: Option<QueryTransform>,
) -> Option<String> {
  let score_cb = cb.unwrap_or(fuzzy_match_score);
  let query = transform.map_or_else(|| query.to_string(), |f| f(query));
  entries
    .into_iter()
    .filter_map(|(text, weight)| {
      let score =
        ScoredCandidate::new(Candidate::from(text.clone())).fuzzy_score_with(&query, score_cb);
      (score > i32::MIN).then_some((score, weight, text))
    })
    .max_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)))
    .map(|(.., text)| text)
}

type ScoreCallback = fn(&str, &[char], bool) -> i32;
/// Transforms the raw typed query into the text actually matched against (e.g.
/// zd's read-only `~`/`$VAR` expansion). The query box still shows the raw text.
type QueryTransform = fn(&str) -> String;
/// Returns the char positions in the display text to highlight for a match (e.g.
/// zd highlighting the basename), or `None` to fall back to `match_positions`.
type HighlightCallback = fn(&str, &str) -> Option<Vec<usize>>;

#[derive(Default, Debug)]
pub(crate) struct FuzzySelector {
  query: QueryEditor,
  filtered: Vec<ScoredCandidate>,
  last_query: String,
  candidates: Vec<Candidate>,
  cursor: ClampedUsize,
  old_layout: Option<FuzzyLayout>,
  /// Index of the leftmost visible column (each column is `MAX_VISIBLE_ROWS`
  /// tall). Layout only ever touches the columns visible from here.
  scroll_col: usize,
  prompt_cursor_col: usize,
  /// Hint shown in bright black inside the query box while it's empty.
  placeholder: Option<String>,
  /// Standalone mode (no prompt line above): the query line is the top row, so
  /// `draw` skips the leading newline and `clear` erases its own row too.
  inline: bool,

  score_cb: Option<ScoreCallback>,
  query_transform: Option<QueryTransform>,
  highlight_cb: Option<HighlightCallback>,
  /// The raw query run through `query_transform` (or the raw query when none).
  /// Scoring and match highlighting both use this so they stay consistent.
  effective_query: String,

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
      last_query: String::new(),
      candidates: vec![],
      cursor: ClampedUsize::new(0, 0, true),
      old_layout: None,
      scroll_col: 0,
      prompt_cursor_col: 0,
      placeholder: None,
      score_cb: None,
      query_transform: None,
      highlight_cb: None,
      effective_query: String::new(),
      inline: false,
      _mouse_guard: Some(Shed::term_mut(|t| t.mouse_support_guard(true.into()))),
    }
  }

  /// Hint text shown in the empty query box (e.g. for standalone pickers).
  pub fn set_placeholder(&mut self, text: Option<impl Into<String>>) {
    self.placeholder = text.map(Into::into);
  }

  pub fn set_score_cb(&mut self, cb: Option<ScoreCallback>) {
    self.score_cb = cb;
  }

  pub fn set_highlight_cb(&mut self, cb: Option<HighlightCallback>) {
    self.highlight_cb = cb;
  }

  /// Standalone mode: the query line is the top row, with no prompt line above.
  pub fn set_inline(&mut self, enable: bool) {
    self.inline = enable;
  }

  /// Column of the query cursor on the query line, past the `► ` leader.
  pub fn query_cursor_col(&self) -> usize {
    let raw = self.query.linebuf.to_string();
    let flat = self.query.linebuf.cursor_to_flat();
    let before: String = raw.chars().take(flat).collect();
    Self::LEADER_W + calc_str_width(&caret_notation(&before))
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
    // New candidate set: `filtered` is stale, so force a full rescan.
    self.last_query.clear();
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
    let raw = self.query.linebuf.to_string();
    // Match against the transformed query (e.g. expanded `~`/`$VAR`), while the
    // box still shows the raw text. `extends` stays on the raw text: the transform
    // is deterministic, so a raw prefix implies an effective prefix.
    let query = self
      .query_transform
      .map_or_else(|| raw.clone(), |f| f(&raw));
    let query_chars: Vec<char> = query.chars().collect();
    let score_fn = self.score_cb.unwrap_or(fuzzy_match_score);

    // The `extends` fast path retains a subset of the previous results, which is
    // only valid when more typed chars mean fewer matches. A query transform
    // breaks that (e.g. "$HOM" -> "" matches all, "$HOME" -> "/home/me"), so a
    // full rescan is required whenever one is set.
    let extends = self.query_transform.is_none()
      && !self.last_query.is_empty()
      && raw.starts_with(self.last_query.as_str());

    let mut scored: Vec<ScoredCandidate> = if extends {
      let mut prev = std::mem::take(&mut self.filtered);
      prev.retain_mut(|sc| {
        let score = score_fn(&sc.candidate, &query_chars, sc.penalize_len_diff);
        sc.score = Some(score);
        score > i32::MIN
      });
      prev
    } else {
      self
        .candidates
        .iter()
        .filter_map(|c| {
          let score = score_fn(c, &query_chars, false);

          (score > i32::MIN).then(|| {
            let mut sc = ScoredCandidate::new(c.clone());
            sc.score = Some(score);
            sc
          })
        })
        .collect()
    };
    // Sort ascending by (score, weight) then reverse, rather than sorting
    // descending directly: the reverse also flips full ties into reverse-insert
    // order, which keeps history (loaded oldest-first) showing newest at top.
    scored.sort_by(|a, b| {
      a.score
        .unwrap_or(i32::MIN)
        .cmp(&b.score.unwrap_or(i32::MIN))
        .then(a.candidate.weight().cmp(&b.candidate.weight()))
    });
    scored.reverse();
    self.cursor.set_max(scored.len());
    // Highlight the top match and scroll home after every (re)score.
    self.cursor.set(0);
    self.scroll_col = 0;
    self.filtered = scored;
    self.last_query = raw;
    self.effective_query = query;
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
  // `t_cols` is passed in rather than read here: this runs inside the
  // `write_term!` (a `Shed::term_mut`) call in `draw`, so re-borrowing the
  // terminal would panic.
  fn query_display(&self, t_cols: usize, underline_color: bool) -> String {
    // The query is a distinct input field: an underlined strip spanning the
    // line, with empty cells as underlined spaces. The real hardware cursor
    // (a blinking beam) marks the position, so there's no fake cursor here.
    let field_width = t_cols.saturating_sub(Self::LEADER_W + 1);
    let query = caret_notation(&self.query.linebuf.to_string());
    let (body, used) = match self.placeholder.as_ref() {
      Some(placeholder) if query.is_empty() => {
        let hint = truncate_to_width(placeholder, field_width);
        let width = calc_str_width(&hint);
        (format!("\x1b[2m{hint}\x1b[22m"), width)
      }
      _ => {
        let width = calc_str_width(&query);
        (query, width)
      }
    };
    let fill = " ".repeat(field_width.saturating_sub(used));
    if underline_color {
      format!("\x1b[4;58:5:250m{body}{fill}\x1b[59;24m")
    } else {
      format!("\x1b[4m{body}{fill}\x1b[24m")
    }
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

  #[expect(clippy::unnested_or_patterns)]
  pub fn handle_key(&mut self, key: K) -> ShResult<SelectorResponse> {
    match key {
      // Pointer events are consumed but unhandled for now; hit-testing a
      // column-major paged grid needs a cell map we haven't built yet.
      K(C::MousePos(..) | C::LeftClick(..), _) => Ok(SelectorResponse::Consumed),
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
      key!(Shift + Tab) | key!(Up) | key!(ScrollUp) => {
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
    let underline_color =
      Shed::term(|t| t.term_caps().contains(TermCap::UNDERLINE_STYLES) && t.color_mode().is_some());
    let rows = GridLayout::MAX_VISIBLE_ROWS;
    self.ensure_cursor_visible(t_cols);

    // Query line. In embedded mode it sits one row below the prompt; inline
    // (standalone) it's the top row, so there's no leading newline.
    if !self.inline {
      write_term!("\n").ok();
    }
    write_term!(
      "{} {}",
      Self::PROMPT,
      self.query_display(t_cols, underline_color)
    )
    .ok();
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
      // Highlight against the effective (transformed) query so matches line up
      // with what was actually scored.
      let query = self.effective_query.clone();
      let highlight_cb = self.highlight_cb;

      // Column-major: cell (col c, row r) is candidate `(scroll_col + c) * rows + r`.
      for r in 0..grid_rows {
        write_term!("\n").ok();
        rows_drawn += 1;
        for (c, width) in col_widths.iter().enumerate() {
          let idx = (self.scroll_col + c) * rows + r;
          if idx >= n {
            break; // later columns at this row are exhausted too
          }

          let avail = width.saturating_sub(Self::LEADER_W);

          let mut name_plain = one_line(&self.filtered[idx].candidate.display());
          if calc_str_width(&name_plain) > avail {
            name_plain = format!(
              "{}…",
              truncate_to_width(&name_plain, avail.saturating_sub(1))
            );
          }
          let name_w = calc_str_width(&name_plain);
          let positions = highlight_cb
            .and_then(|cb| cb(&name_plain, &query))
            .unwrap_or_else(|| match_positions(&name_plain, &query));
          let name = emphasize_fuzzy(&name_plain, |i| positions.binary_search(&i).is_ok());

          // Per-column max name width, so descriptions align (like the grid).
          let col_start = (self.scroll_col + c) * rows;
          let col_end = (col_start + rows).min(n);
          let (col_name_max, _) = Self::col_dims(&self.filtered[col_start..col_end]);

          let is_selected = idx == cursor_pos;

          let cell = if let Some(desc) = self.filtered[idx]
            .candidate
            .desc()
            .filter(|d| !d.is_empty())
          {
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
          } else {
            let trailing = " ".repeat(avail.saturating_sub(name_w));
            format!("{name}{trailing}")
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

    // Park the cursor back on the first overlay row: the prompt line in embedded
    // mode (one above the query line), or the query line itself when inline.
    let up = rows_drawn - usize::from(self.inline);
    write_term!("\x1b[{up}A\r").ok();
    if self.prompt_cursor_col > 0 {
      write_term!("\x1b[{}C", self.prompt_cursor_col).ok();
    }

    self.old_layout = Some(FuzzyLayout { rows: rows_drawn });
    rows_drawn
  }

  pub fn clear(&mut self) {
    if let Some(layout) = self.old_layout.take() {
      if self.inline {
        // Cursor rests on the first overlay row (the query line); erase it and
        // each row below, then return to it.
        write_term!("\x1b[2K").ok();
        for _ in 1..layout.rows {
          write_term!("\x1b[1B\x1b[2K").ok();
        }
        if layout.rows > 1 {
          write_term!("\x1b[{}A", layout.rows - 1).ok();
        }
        write_term!("\r").ok();
        return;
      }
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
  fn query_cursor_col(&self) -> Option<usize> {
    Some(self.selector.query_cursor_col())
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

#[cfg(test)]
mod caret_tests {
  use super::caret_notation;

  #[test]
  fn visualizes_newline_tab_cr_and_esc() {
    assert_eq!(caret_notation("a\nb"), "a^Jb");
    assert_eq!(caret_notation("a\rb"), "a^Mb");
    assert_eq!(caret_notation("a\tb"), "a^Ib");
    assert_eq!(caret_notation("a\x1bb"), "a^[b");
    assert_eq!(caret_notation("a\x7fb"), "a^?b");
  }

  #[test]
  fn leaves_plain_text_untouched() {
    assert_eq!(caret_notation("~/projects/fern"), "~/projects/fern");
  }
}
