use std::io;

use nix::unistd;
use regex::Regex;

use yansi::Style;

use crate::{
  key,
  keys::{KeyCode, KeyEvent},
  procio, queue_term,
  readline::SimpleEditor,
  state::{Shed, terminal::Terminal},
  util::{Direction, error::ShResult, ui},
  write_term,
};

use super::{
  StyledHelp,
  markup::{MarkedSpan, REF_SEQ},
  render::{self, Overlay},
};

pub(super) enum PagerEvent {
  Continue,
  Back,
  Forward,
  OpenRef(String), // Open a new pager from this cross-reference
  ClosePage,
  ExitPager,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum PagerCmd {
  Scroll(isize), // line offset
  TopOfPage,
  BottomOfPage,
}

#[derive(Default, Debug)]
struct SearchQuery {
  editor: SimpleEditor,
  dir: Direction,
  results: Vec<(usize, usize)>,
  active_result_idx1: usize,
  anchor: usize, // line we started on
  active: bool,
}

impl SearchQuery {
  pub fn reset(&mut self) {
    self.active = false;
    self.editor.buf.clear_buffer();
    self.results.clear();
    self.active_result_idx1 = 0;
  }

  pub fn is_empty(&self) -> bool {
    self.editor.buf.is_empty()
  }
}

struct CrossRef {
  span: MarkedSpan,
  target: Option<String>,
}

impl CrossRef {
  pub fn span(&self) -> &MarkedSpan {
    &self.span
  }
  pub fn resolve_target(&self, content: &str) -> String {
    self
      .target
      .clone()
      .unwrap_or_else(|| self.span.content(content).to_string())
  }
}

impl From<(MarkedSpan, Option<String>)> for CrossRef {
  fn from((span, target): (MarkedSpan, Option<String>)) -> Self {
    Self { span, target }
  }
}

struct ClickableRef {
  row: usize,
  col_start: usize,
  col_end: usize,
  ref_idx: usize,
}

pub(super) struct HelpPager {
  search: SearchQuery,
  ref_keys: Vec<(usize, char)>,
  cross_refs: Vec<CrossRef>,
  click_refs: Vec<ClickableRef>,
  hovered: Option<usize>, // index into cross_refs

  jump_dist: usize,

  scroll_offset: usize,
  filename: Option<String>,
  content: StyledHelp,
}

impl HelpPager {
  pub fn new(content: &str, scroll_offset: usize, filename: Option<String>) -> Option<Self> {
    if !unistd::isatty(procio::stdout_fileno()).unwrap_or(false) {
      // If we're not in a terminal, just print the content and exit
      // Someone could be piping the output, like `help | grep foo`
      unistd::write(procio::stdout_fileno(), content.as_bytes()).ok();
      unistd::write(procio::stdout_fileno(), b"\n").ok();
      return None;
    }
    let mut content = StyledHelp::new(content);
    let cross_refs = content
      .find_markers(REF_SEQ)
      .into_iter()
      .zip(content.take_ref_targets())
      .map(CrossRef::from)
      .collect();

    Some(Self {
      jump_dist: 15,
      ref_keys: vec![],
      click_refs: vec![],
      search: SearchQuery::default(),
      hovered: None,
      scroll_offset,
      filename,
      content,
      cross_refs,
    })
  }
  pub fn content(&self) -> &str {
    self.content.content()
  }

  pub fn cross_refs_in_viewport(&self) -> Vec<usize> {
    let top = self.scroll_offset;
    let t_rows = Shed::term(Terminal::t_rows).saturating_sub(1);
    let bottom = top + t_rows;

    let first = self
      .cross_refs
      .iter()
      .position(|c_ref| c_ref.span().line_no(self.content()) >= top);

    let last = self
      .cross_refs
      .iter()
      .rposition(|c_ref| c_ref.span().line_no(self.content()) < bottom);

    match (first, last) {
      (Some(f), Some(l)) if f <= l => (f..=l).collect(),
      _ => vec![],
    }
  }

  pub fn display(&mut self) -> ShResult<()> {
    queue_term!(TermCtl::Cursor(Home))?;
    let height = Shed::term(Terminal::t_rows).saturating_sub(1);

    // Build click map for cross-references in viewport
    self.click_refs.clear();
    let scroll = self.scroll_offset;
    let content_str = self.content.content();
    for (idx, c_ref) in self.cross_refs.iter().enumerate() {
      let line_no = c_ref.span().line_no(content_str);
      if line_no < scroll || line_no >= scroll + height {
        continue;
      }
      let screen_row = line_no - scroll; // 0-based offset from top of viewport
      let line_start = c_ref.span().line_start(content_str);

      let (prefix_range, _, postfix_range) = c_ref.span().rel_to_line(content_str);
      let line_text = &content_str[line_start..];

      let col_start = ui::calc_str_width(&line_text[..prefix_range.start]);
      let col_end = ui::calc_str_width(&line_text[..postfix_range.end]);

      self.click_refs.push(ClickableRef {
        row: screen_row,
        col_start,
        col_end,
        ref_idx: idx,
      });
    }

    // Build the overlay list for this frame. Order doesn't matter, the
    // renderer sorts events by position internally.
    let mut overlays: Vec<Overlay> = Vec::new();

    if let Some(idx) = self.hovered
      && let Some(c_ref) = self.cross_refs.get(idx)
    {
      // insert overlay for hovered cross references
      overlays.push(Overlay::Span {
        range: c_ref.span().content_range(),
        style: hover_style(),
      });
    }

    for (i, (s, e)) in self.search.results.iter().enumerate() {
      let is_focused = i + 1 == self.search.active_result_idx1;
      // insert search result overlay
      overlays.push(Overlay::Span {
        range: *s..*e,
        style: if is_focused {
          search_focus_style()
        } else {
          search_hit_style()
        },
      });
    }

    for (ref_idx, ch) in &self.ref_keys {
      if let Some(c_ref) = self.cross_refs.get(*ref_idx) {
        // insert hint key text
        overlays.push(Overlay::Insert {
          pos: c_ref.span().content_range().end,
          text: format!("[{ch}]"),
          style: hint_key_style(),
        });
      }
    }

    // apply overlays (search, hover, hint keys)
    let rendered = render::render(self.content(), overlays);

    // final rendered content
    let content_lines: Vec<_> = rendered
      .lines()
      .skip(self.scroll_offset)
      .take(height)
      .collect();

    for line in &content_lines {
      write_term!("{line}").ok();
      queue_term!(TermCtl::Clear(LineToEnd), TermCtl::PrintChar('\n')).ok();
    }

    for _ in content_lines.len()..height {
      write_term!("\x1b[1;34m~\x1b[0m").ok(); // draw tildes on empty lines
      queue_term!(TermCtl::Clear(LineToEnd), TermCtl::PrintChar('\n')).ok();
    }

    queue_term!(TermCtl::PrintChar('\r')).ok();

    if let Some(name) = &self.filename {
      write_term!("\x1b[1;7;4m {name} \x1b[0m ",).ok();
    }

    if self.search.active {
      let query = self.search.editor.buf.to_string();
      let prefix = match self.search.dir {
        Direction::Forward => '/',
        Direction::Backward => '?',
      };
      write_term!("\x1b[1;7;4m {prefix}{query} \x1b[0m",).ok();
    } else {
      queue_term!(TermCtl::Clear(LineToEnd),).ok();
    }

    Shed::term_mut(io::Write::flush)?;
    Ok(())
  }

  pub fn handle_input(&mut self) -> ShResult<PagerEvent> {
    Shed::term_mut(Terminal::read)?;
    let keys = Shed::term_mut(Terminal::drain_keys);

    let mut res = PagerEvent::Continue;
    for key in keys {
      res = self.handle_key(key)?;
    }

    Ok(res)
  }

  #[expect(clippy::unnested_or_patterns)]
  pub fn handle_key(&mut self, key: KeyEvent) -> ShResult<PagerEvent> {
    let cmd = match key {
      key!(Tab) => {
        if self.ref_keys.is_empty() {
          self.enter_hint_mode();
        } else {
          self.ref_keys.clear();
        }
        return Ok(PagerEvent::Continue);
      }

      key!(Esc) => {
        if !self.ref_keys.is_empty() {
          self.ref_keys.clear();
          return Ok(PagerEvent::Continue);
        } else if self.search.active {
          self.search.reset();
          return Ok(PagerEvent::Continue);
        }
        return Ok(PagerEvent::ClosePage);
      }

      key!(Backspace) if self.search.active && self.search.is_empty() => {
        self.search.reset();
        return Ok(PagerEvent::Continue);
      }

      key!(Enter) if self.search.active => {
        self.search(true);
        self.search.active = false; // keep results for highlighting

        return Ok(PagerEvent::Continue);
      }

      _ if self.search.active => {
        self.search.editor.handle_key(key)?;
        if self.search.editor.buf.is_empty() {
          self.search.results.clear();
        } else {
          self.search(false);
        }

        return Ok(PagerEvent::Continue);
      }

      KeyEvent(KeyCode::Char(ch @ ('/' | '?')), _) => {
        if !self.ref_keys.is_empty() {
          self.ref_keys.clear();
        }
        self.search.reset();
        let dir = match ch {
          '?' => Direction::Backward,
          '/' => Direction::Forward,
          _ => unreachable!(),
        };

        self.search.active = true;
        self.search.dir = dir;
        self.search.anchor = self.scroll_offset;
        self.search.active_result_idx1 = 0;
        self.search.results.clear();

        return Ok(PagerEvent::Continue);
      }

      KeyEvent(KeyCode::Char(ch), _) if !self.ref_keys.is_empty() => {
        if let Some(index) = self
          .ref_keys
          .iter()
          .find(|(_, c)| *c == ch)
          .map(|(i, _)| *i)
        {
          self.ref_keys.clear();
          let c_ref = &self.cross_refs[index];
          let target = c_ref.resolve_target(self.content());

          return Ok(PagerEvent::OpenRef(target));
        }
        self.ref_keys.clear();
        return self.handle_key(key); // re-process the key without hint mode
      }

      KeyEvent(KeyCode::Char(dir @ ('n' | 'N')), _) => {
        match dir {
          'n' => self.jump_to_match(Direction::Forward),
          'N' => self.jump_to_match(Direction::Backward),
          _ => unreachable!(),
        }
        return Ok(PagerEvent::Continue);
      }

      key!('q') => return Ok(PagerEvent::ExitPager),

      key!('g') => PagerCmd::TopOfPage,
      key!('G') => PagerCmd::BottomOfPage,

      key!('d') | key!(PageDown) => PagerCmd::Scroll(self.jump_dist.cast_signed()),
      key!('u') | key!(PageUp) => PagerCmd::Scroll(-self.jump_dist.cast_signed()),

      key!(ScrollDown) | key!(Down) | key!('j') | key!(Enter) if !self.search.active => {
        PagerCmd::Scroll(1)
      }
      key!(ScrollUp) | key!(Up) | key!('k') => PagerCmd::Scroll(-1),
      key!(Back) | key!(Left) | key!('h') => return Ok(PagerEvent::Back),
      key!(Forward) | key!(Right) | key!('l') => return Ok(PagerEvent::Forward),

      KeyEvent(KeyCode::MousePos(row, col), _) => {
        return self.handle_hover(row, col);
      }
      KeyEvent(KeyCode::LeftClick(row, col), _) => {
        return Ok(self.handle_click(row, col));
      }

      _ => return Ok(PagerEvent::Continue),
    };

    self.exec_cmd(cmd);

    Ok(PagerEvent::Continue)
  }

  pub fn max_scroll(&self) -> usize {
    let content_rows = Shed::term(Terminal::t_rows).saturating_sub(1);
    self.content().lines().count().saturating_sub(content_rows)
  }

  pub fn search(&mut self, jump: bool) {
    if self.search.editor.buf.to_string().is_empty() || !self.search.active {
      return;
    }
    let pat = self.search.editor.buf.to_string();
    let re = Regex::new(&regex::escape(&pat)).unwrap();

    let visible = self.content.visible();
    let map = self.content.visible_to_baked();

    // search the visible string, and map the visible bytes
    // back to the styled content byte positions
    self.search.results = re
      .find_iter(visible)
      .map(|m| (map[m.start()], map[m.end()]))
      .collect();

    // update active idx
    let lf_positions: Vec<_> = self
      .content
      .content()
      .bytes()
      .enumerate()
      .filter(|(_, c)| *c == b'\n')
      .map(|(i, _)| i)
      .collect();

    let line_for = |start: &usize| {
      lf_positions
        .iter()
        .position(|pos| *pos > *start)
        .unwrap_or(lf_positions.len())
    };
    if !self.search.results.is_empty() {
      let anchor = self.search.anchor;
      // compare line to anchor
      let pos = match self.search.dir {
        Direction::Forward => self
          .search
          .results
          .iter()
          .position(|(start, _)| line_for(start) >= anchor),
        Direction::Backward => self
          .search
          .results
          .iter()
          .rposition(|(start, _)| line_for(start) <= anchor),
      };
      // wrap around if none found
      self.search.active_result_idx1 = pos.map_or_else(
        || {
          if matches!(self.search.dir, Direction::Backward) {
            self.search.results.len()
          } else {
            1
          }
        },
        |p| p + 1,
      );
    }

    if jump {
      self.jump_to_match(self.search.dir);
    }
  }

  pub fn jump_to_match(&mut self, dir: Direction) {
    if self.search.results.is_empty() {
      return;
    }

    let content = self.content();

    // I'd like to personally thank the borrow checker for forcing this thing into existence
    let lf_positions: Vec<_> = content
      .bytes()
      .enumerate()
      .filter(|(_, c)| *c == b'\n')
      .map(|(i, _)| i)
      .collect();

    let line_for = |start: &usize| {
      lf_positions
        .iter()
        .position(|pos| *pos > *start)
        .unwrap_or(lf_positions.len())
    };

    // Try to find a match past the anchor in the given direction
    let after_anchor = self.search.results.iter().filter(|(start, _)| {
      if self.search.active_result_idx1 > 0 {
        let current_range = self.search.results[self.search.active_result_idx1 - 1];
        match dir {
          Direction::Forward => *start > current_range.1,
          Direction::Backward => *start < current_range.0,
        }
      } else {
        true
      }
    });

    let found = match dir {
      Direction::Forward => after_anchor.min_by_key(|(start, _)| *start),
      Direction::Backward => after_anchor.max_by_key(|(start, _)| *start),
    };

    // If nothing found past anchor, wrap around
    let found = found.or_else(|| match dir {
      Direction::Forward => self.search.results.iter().min_by_key(|(start, _)| *start),
      Direction::Backward => self.search.results.iter().max_by_key(|(start, _)| *start),
    });

    let height = Shed::term(Terminal::t_rows).saturating_sub(1); // Get current terminal height
    if let Some((start, _)) = found {
      let line_no = line_for(start);

      // Check if the target line is already in the viewport
      let is_visible = line_no >= self.scroll_offset && line_no < (self.scroll_offset + height);

      if !is_visible {
        // Only jump if not visible
        self.scroll_offset = line_no.saturating_sub(2);
      }
      self.search.anchor = line_no;
    }

    // update the focus index
    match dir {
      Direction::Forward => {
        self.search.active_result_idx1 += 1;
        if self.search.active_result_idx1 > self.search.results.len() {
          self.search.active_result_idx1 = 1;
        }
      }
      Direction::Backward => {
        if self.search.active_result_idx1 <= 1 {
          self.search.active_result_idx1 = self.search.results.len();
        } else {
          self.search.active_result_idx1 -= 1;
        }
        if self.search.active_result_idx1 == 0 {
          self.search.active_result_idx1 = self.search.results.len();
        }
      }
    }
  }

  pub fn enter_hint_mode(&mut self) {
    if self.search.active {
      self.search.reset();
    }

    let mut chars = HintChars::new();
    let c_refs = self.cross_refs_in_viewport();

    for i in c_refs {
      if let Some(ch) = chars.next() {
        self.ref_keys.push((i, ch));
      } else {
        break; // no more hint chars available
      }
    }
  }

  fn click_ref_from_pos(&self, row: usize, col: usize) -> Option<&ClickableRef> {
    self
      .click_refs
      .iter()
      .find(|cr| cr.row == row && col >= cr.col_start && col < cr.col_end)
  }

  fn handle_hover(&mut self, row: usize, col: usize) -> ShResult<PagerEvent> {
    let (row, col) = (row.saturating_sub(1), col.saturating_sub(1));
    let new_hover = self.click_ref_from_pos(row, col).map(|cr| cr.ref_idx);

    if new_hover != self.hovered {
      self.hovered = new_hover;
      self.display()?;
    }

    Ok(PagerEvent::Continue)
  }

  fn handle_click(&mut self, row: usize, col: usize) -> PagerEvent {
    let (row, col) = (row.saturating_sub(1), col.saturating_sub(1));
    if let Some(cr) = self
      .click_refs
      .iter()
      .find(|cr| cr.row == row && col >= cr.col_start && col < cr.col_end)
    {
      let c_ref = &self.cross_refs[cr.ref_idx];
      let target = c_ref.resolve_target(self.content());

      return PagerEvent::OpenRef(target);
    }
    PagerEvent::Continue
  }

  pub fn exec_cmd(&mut self, cmd: PagerCmd) {
    match cmd {
      PagerCmd::Scroll(n) => {
        self.scroll_offset = self
          .scroll_offset
          .saturating_add_signed(n)
          .min(self.max_scroll());
      }
      PagerCmd::TopOfPage => {
        self.scroll_offset = 0;
      }
      PagerCmd::BottomOfPage => {
        let content_rows = Shed::term(Terminal::t_rows).saturating_sub(1);
        let n_lines = self.content().lines().count();
        self.scroll_offset = n_lines.saturating_sub(content_rows);
      }
    }
  }
}

fn hover_style() -> Style {
  Style::new().invert().cyan()
}

fn search_hit_style() -> Style {
  Style::new().bold().invert()
}

fn search_focus_style() -> Style {
  Style::new().bold().invert().cyan()
}

fn hint_key_style() -> Style {
  Style::new().bold().yellow()
}

struct HintChars {
  seq: String,
}

impl HintChars {
  pub fn new() -> Self {
    Self {
      seq: "MNBVCXZPOIUYTREWQLKJHGFDSAmnbvcxzpoiuytrewqlkjhgfdsa".into(),
    }
  }
}

impl Iterator for HintChars {
  type Item = char;
  fn next(&mut self) -> Option<Self::Item> {
    self.seq.pop()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::keys::{KeyCode as K, KeyEvent, ModKeys};
  use crate::tests::testutil::TestGuard;

  /// Standard sample with multiple lines so scrolling actually has somewhere
  /// to go. Each line is short — the layout machinery just needs *something*
  /// to scroll over.
  const SAMPLE: &str =
    "line 1\nline 2\nline 3\nline 4\nline 5\nfoo bar baz\nline 7\nfoo again\nline 9";

  fn pager_with(content: &str) -> (TestGuard, HelpPager) {
    let g = TestGuard::new();
    let p = HelpPager::new(content, 0, None)
      .expect("HelpPager::new should succeed under TestGuard's pty");
    (g, p)
  }

  fn key(code: K) -> KeyEvent {
    KeyEvent(code, ModKeys::empty())
  }

  // ─── Exit / quit ─────────────────────────────────────────────────────

  #[test]
  fn handle_key_q_exits() {
    let (_g, mut p) = pager_with(SAMPLE);
    let ev = p.handle_key(key(K::Char('q'))).unwrap();
    assert!(matches!(ev, PagerEvent::ExitPager));
  }

  #[test]
  fn handle_key_esc_with_no_state_exits() {
    let (_g, mut p) = pager_with(SAMPLE);
    let ev = p.handle_key(key(K::Esc)).unwrap();
    assert!(matches!(ev, PagerEvent::ClosePage));
  }

  // ─── Scroll motions ──────────────────────────────────────────────────

  #[test]
  fn handle_key_j_scrolls_down_one_line() {
    let (_g, mut p) = pager_with(SAMPLE);
    let before = p.scroll_offset;
    p.handle_key(key(K::Char('j'))).unwrap();
    assert_eq!(p.scroll_offset, (before + 1).min(p.max_scroll()));
  }

  #[test]
  fn handle_key_k_scrolls_up_one_line() {
    let (_g, mut p) = pager_with(SAMPLE);
    // Move down first so there's somewhere to come back from.
    p.handle_key(key(K::Char('j'))).unwrap();
    p.handle_key(key(K::Char('j'))).unwrap();
    let before = p.scroll_offset;
    p.handle_key(key(K::Char('k'))).unwrap();
    assert_eq!(p.scroll_offset, before.saturating_sub(1));
  }

  #[test]
  fn handle_key_g_jumps_to_top() {
    let (_g, mut p) = pager_with(SAMPLE);
    // Get away from the top, then bounce back.
    p.handle_key(key(K::Char('G'))).unwrap();
    assert!(p.scroll_offset > 0 || p.max_scroll() == 0);
    p.handle_key(key(K::Char('g'))).unwrap();
    assert_eq!(p.scroll_offset, 0);
  }

  #[test]
  fn handle_key_capital_g_jumps_to_bottom() {
    let (_g, mut p) = pager_with(SAMPLE);
    p.handle_key(key(K::Char('G'))).unwrap();
    assert_eq!(p.scroll_offset, p.max_scroll());
  }

  #[test]
  fn handle_key_arrow_down_and_scroll_down_match_j() {
    for code in [K::Down, K::ScrollDown] {
      let (_g, mut p) = pager_with(SAMPLE);
      p.handle_key(key(code)).unwrap();
      assert_eq!(p.scroll_offset, 1.min(p.max_scroll()));
    }
  }

  #[test]
  fn handle_key_arrow_up_and_scroll_up_match_k() {
    for code in [K::Up, K::ScrollUp] {
      let (_g, mut p) = pager_with(SAMPLE);
      p.handle_key(key(K::Char('j'))).unwrap();
      p.handle_key(key(K::Char('j'))).unwrap();
      let before = p.scroll_offset;
      p.handle_key(key(code)).unwrap();
      assert_eq!(p.scroll_offset, before.saturating_sub(1));
    }
  }

  #[test]
  fn handle_key_d_half_page_down() {
    let (_g, mut p) = pager_with(SAMPLE);
    let jump = p.jump_dist;
    p.handle_key(key(K::Char('d'))).unwrap();
    assert_eq!(p.scroll_offset, jump.min(p.max_scroll()));
  }

  #[test]
  fn handle_key_u_half_page_up() {
    let (_g, mut p) = pager_with(SAMPLE);
    p.handle_key(key(K::Char('G'))).unwrap();
    let before = p.scroll_offset;
    let jump = p.jump_dist;
    p.handle_key(key(K::Char('u'))).unwrap();
    assert_eq!(p.scroll_offset, before.saturating_sub(jump));
  }

  // ─── History navigation ──────────────────────────────────────────────

  #[test]
  fn handle_key_h_returns_back() {
    let (_g, mut p) = pager_with(SAMPLE);
    let ev = p.handle_key(key(K::Char('h'))).unwrap();
    assert!(matches!(ev, PagerEvent::Back));
  }

  #[test]
  fn handle_key_l_returns_forward() {
    let (_g, mut p) = pager_with(SAMPLE);
    let ev = p.handle_key(key(K::Char('l'))).unwrap();
    assert!(matches!(ev, PagerEvent::Forward));
  }

  #[test]
  fn handle_key_left_and_back_match_h() {
    let (_g, mut p) = pager_with(SAMPLE);
    let ev = p.handle_key(key(K::Left)).unwrap();
    assert!(matches!(ev, PagerEvent::Back));
    let ev = p.handle_key(key(K::Back)).unwrap();
    assert!(matches!(ev, PagerEvent::Back));
  }

  #[test]
  fn handle_key_right_and_forward_match_l() {
    let (_g, mut p) = pager_with(SAMPLE);
    let ev = p.handle_key(key(K::Right)).unwrap();
    assert!(matches!(ev, PagerEvent::Forward));
    let ev = p.handle_key(key(K::Forward)).unwrap();
    assert!(matches!(ev, PagerEvent::Forward));
  }

  // ─── Search mode ─────────────────────────────────────────────────────

  #[test]
  fn handle_key_slash_starts_forward_search() {
    let (_g, mut p) = pager_with(SAMPLE);
    let ev = p.handle_key(key(K::Char('/'))).unwrap();
    assert!(matches!(ev, PagerEvent::Continue));
    assert!(p.search.active);
    assert!(matches!(p.search.dir, Direction::Forward));
  }

  #[test]
  fn handle_key_question_starts_backward_search() {
    let (_g, mut p) = pager_with(SAMPLE);
    let ev = p.handle_key(key(K::Char('?'))).unwrap();
    assert!(matches!(ev, PagerEvent::Continue));
    assert!(p.search.active);
    assert!(matches!(p.search.dir, Direction::Backward));
  }

  #[test]
  fn handle_key_chars_in_search_mode_go_to_query_editor() {
    let (_g, mut p) = pager_with(SAMPLE);
    p.handle_key(key(K::Char('/'))).unwrap();
    p.handle_key(key(K::Char('f'))).unwrap();
    p.handle_key(key(K::Char('o'))).unwrap();
    p.handle_key(key(K::Char('o'))).unwrap();
    assert!(p.search.active);
    // The query editor's buffer should reflect what we typed.
    assert_eq!(p.search.editor.buf.to_string(), "foo");
  }

  #[test]
  fn handle_key_enter_in_search_mode_executes_and_deactivates() {
    let (_g, mut p) = pager_with(SAMPLE);
    p.handle_key(key(K::Char('/'))).unwrap();
    p.handle_key(key(K::Char('f'))).unwrap();
    p.handle_key(key(K::Char('o'))).unwrap();
    p.handle_key(key(K::Char('o'))).unwrap();
    p.handle_key(key(K::Enter)).unwrap();
    // Search no longer active, but results remain so they highlight.
    assert!(!p.search.active);
  }

  #[test]
  fn handle_key_esc_in_search_mode_cancels_search() {
    let (_g, mut p) = pager_with(SAMPLE);
    p.handle_key(key(K::Char('/'))).unwrap();
    p.handle_key(key(K::Char('x'))).unwrap();
    let ev = p.handle_key(key(K::Esc)).unwrap();
    assert!(matches!(ev, PagerEvent::Continue));
    assert!(!p.search.active);
  }

  #[test]
  fn handle_key_backspace_on_empty_search_query_cancels_search() {
    let (_g, mut p) = pager_with(SAMPLE);
    p.handle_key(key(K::Char('/'))).unwrap();
    // Buffer is empty, no chars typed yet — backspace exits search mode.
    let ev = p.handle_key(key(K::Backspace)).unwrap();
    assert!(matches!(ev, PagerEvent::Continue));
    assert!(!p.search.active);
  }

  // ─── Hint mode (Tab) ─────────────────────────────────────────────────

  #[test]
  fn handle_key_tab_with_no_refs_in_view_still_continues() {
    // Our SAMPLE has no cross-refs, so enter_hint_mode produces an empty
    // ref_keys list. Either way the function should not error.
    let (_g, mut p) = pager_with(SAMPLE);
    let ev = p.handle_key(key(K::Tab)).unwrap();
    assert!(matches!(ev, PagerEvent::Continue));
  }

  #[test]
  fn handle_key_tab_again_clears_existing_hint_keys() {
    let (_g, mut p) = pager_with(SAMPLE);
    // Manually populate ref_keys so the "non-empty branch" is exercised.
    p.ref_keys = vec![(0, 'a'), (1, 'b')];
    p.handle_key(key(K::Tab)).unwrap();
    assert!(p.ref_keys.is_empty(), "second Tab should clear ref_keys");
  }

  #[test]
  fn handle_key_esc_in_hint_mode_clears_refs_but_doesnt_exit() {
    let (_g, mut p) = pager_with(SAMPLE);
    p.ref_keys = vec![(0, 'a')];
    let ev = p.handle_key(key(K::Esc)).unwrap();
    assert!(matches!(ev, PagerEvent::Continue));
    assert!(p.ref_keys.is_empty());
  }

  #[test]
  fn handle_key_unmatched_char_in_hint_mode_clears_and_reprocesses() {
    // Provide a fake ref_keys list, then send a char that DOESN'T match;
    // the function clears refs and recursively re-handles the key. Since
    // 'q' is "exit", we should get Exit back.
    let (_g, mut p) = pager_with(SAMPLE);
    p.ref_keys = vec![(0, 'a')];
    let ev = p.handle_key(key(K::Char('q'))).unwrap();
    assert!(matches!(ev, PagerEvent::ExitPager));
    assert!(p.ref_keys.is_empty());
  }

  // ─── Match navigation (n / N) ────────────────────────────────────────

  #[test]
  fn handle_key_n_capital_n_dont_error_with_no_search() {
    let (_g, mut p) = pager_with(SAMPLE);
    let ev = p.handle_key(key(K::Char('n'))).unwrap();
    assert!(matches!(ev, PagerEvent::Continue));
    let ev = p.handle_key(key(K::Char('N'))).unwrap();
    assert!(matches!(ev, PagerEvent::Continue));
  }

  // ─── Unhandled keys ──────────────────────────────────────────────────

  #[test]
  fn handle_key_unhandled_char_is_continue() {
    let (_g, mut p) = pager_with(SAMPLE);
    let ev = p.handle_key(key(K::Char('z'))).unwrap();
    assert!(matches!(ev, PagerEvent::Continue));
  }
}
