use unicode_segmentation::UnicodeSegmentation;

use super::{
  Grapheme, Line, Lines, Pos,
  eval::{ParseFlags, ParsedSrc, lex::LexFlags},
};
use crate::readline::context::CtxTkRule;
use crate::{readline::context::CtxTk, state::vars::VarStr, util::VarStrDisplay};

/// One undo step. Finalized steps are stored compactly as a positional delta;
/// only an actively-merging entry keeps full buffer snapshots, and there is at
/// most one of those at a time (the undo-stack top while a merge is open).
#[derive(Clone, Debug)]
pub struct Edit {
  pub old_cursor: Pos,
  pub new_cursor: Pos,
  pub merging: bool,
  body: EditBody,
}

#[derive(Clone, Debug)]
enum EditBody {
  Delta {
    at: Pos,
    removed: VarStr,
    inserted: VarStr,
  },
  Snapshot {
    old: Lines,
    new: Lines,
  },
}

impl Edit {
  fn delta(at: Pos, removed: VarStr, inserted: VarStr, old_cursor: Pos, new_cursor: Pos) -> Self {
    Edit {
      old_cursor,
      new_cursor,
      merging: false,
      body: EditBody::Delta {
        at,
        removed,
        inserted,
      },
    }
  }
  fn snapshot(old: Lines, new: Lines, old_cursor: Pos, new_cursor: Pos, merging: bool) -> Self {
    Edit {
      old_cursor,
      new_cursor,
      merging,
      body: EditBody::Snapshot { old, new },
    }
  }
  /// An empty step, used to break a merge chain without recording a change.
  pub(super) fn barrier(cursor: Pos) -> Self {
    Edit::delta(cursor, VarStr::default(), VarStr::default(), cursor, cursor)
  }
  pub fn is_empty(&self) -> bool {
    match &self.body {
      EditBody::Delta {
        removed, inserted, ..
      } => removed.is_empty() && inserted.is_empty(),
      EditBody::Snapshot { old, new } => old == new,
    }
  }
  /// Extend an open merge with the current buffer state.
  fn set_new(&mut self, new_lines: Lines, new_cursor: Pos) {
    if let EditBody::Snapshot { new, .. } = &mut self.body {
      *new = new_lines;
    }
    self.new_cursor = new_cursor;
  }
  /// Collapse a finished merge's snapshots into a compact delta.
  pub(super) fn finalize(&mut self) {
    let data = match &self.body {
      EditBody::Snapshot { old, new } => Some(Diff::new(old, new)),
      EditBody::Delta { .. } => None,
    };
    if let Some(Diff {
      start,
      removed,
      inserted,
    }) = data
    {
      self.body = EditBody::Delta {
        at: start,
        removed,
        inserted,
      };
    }
  }
  /// Re-expand a delta into snapshots so the entry can be merged into. `current`
  /// is the live buffer, which equals this entry's post-edit state.
  fn make_snapshot(&mut self, current: &Lines) {
    let snap = match &self.body {
      EditBody::Delta {
        at,
        removed,
        inserted,
      } => {
        let mut old = current.clone();
        splice_lines(
          &mut old,
          *at,
          &inserted.to_str_lossy(),
          &removed.to_str_lossy(),
        ); // revert this delta
        Some((old, current.clone()))
      }
      EditBody::Snapshot { .. } => None,
    };
    if let Some((old, new)) = snap {
      self.body = EditBody::Snapshot { old, new };
    }
  }
  pub(super) fn undo(&self, lines: &mut Lines) {
    self.apply(lines, true);
  }
  pub(super) fn redo(&self, lines: &mut Lines) {
    self.apply(lines, false);
  }

  /// Apply this step to `lines` in the undo (true) or redo (false) direction.
  fn apply(&self, lines: &mut Lines, is_undo: bool) {
    match &self.body {
      EditBody::Snapshot { old, new } => {
        *lines = if is_undo { old.clone() } else { new.clone() };
      }
      EditBody::Delta {
        at,
        removed,
        inserted,
      } => {
        if is_undo {
          splice_lines(
            lines,
            *at,
            &inserted.to_str_lossy(),
            &removed.to_str_lossy(),
          );
        } else {
          splice_lines(
            lines,
            *at,
            &removed.to_str_lossy(),
            &inserted.to_str_lossy(),
          );
        }
      }
    }
  }
}

pub struct Diff {
  start: Pos,
  removed: VarStr,
  inserted: VarStr,
}

impl Diff {
  fn new(before: &Lines, after: &Lines) -> Self {
    if before.len() == after.len() {
      let mut changed: Option<usize> = None;
      let mut multiple = false;
      for r in 0..before.len() {
        if before[r] != after[r] {
          if changed.is_some() {
            multiple = true;
            break;
          }
          changed = Some(r);
        }
      }
      match changed {
        Some(r) if !multiple => return Self::diff_single_row(r, &before[r], &after[r]),
        None => {
          return Self {
            start: Pos::default(),
            removed: VarStr::default(),
            inserted: VarStr::default(),
          };
        } // identical
        _ => {} // >1 differing rows: fall through to the flat diff
      }
    }
    Self::diff_flat(before, after)
  }
  /// Flat grapheme diff over the whole newline-joined buffer. Correct for any
  /// change including row insert/delete, at the cost of stringifying both buffers.
  fn diff_flat(before: &Lines, after: &Lines) -> Self {
    let a = before.to_string();
    let b = after.to_string();
    let ag: Vec<&str> = a.graphemes(true).collect();
    let bg: Vec<&str> = b.graphemes(true).collect();
    let pmax = ag.len().min(bg.len());
    let mut p = 0;
    while p < pmax && ag[p] == bg[p] {
      p += 1;
    }
    let smax = (ag.len() - p).min(bg.len() - p);
    let mut s = 0;
    while s < smax && ag[ag.len() - 1 - s] == bg[bg.len() - 1 - s] {
      s += 1;
    }
    let removed: VarStr = ag[p..ag.len() - s].concat().into();
    let inserted: VarStr = bg[p..bg.len() - s].concat().into();
    let mut row = 0;
    let mut col = 0;
    for g in &ag[..p] {
      if *g == "\n" {
        row += 1;
        col = 0;
      } else {
        col += 1;
      }
    }
    Self {
      start: Pos { row, col },
      removed,
      inserted,
    }
  }

  /// Diff two single lines into ((row, col), removed, inserted). No newlines are
  /// involved, so the spans stay within the row.
  fn diff_single_row(row: usize, before: &Line, after: &Line) -> Self {
    let bg = before.graphemes();
    let ag = after.graphemes();
    let pmax = bg.len().min(ag.len());
    let mut p = 0;
    while p < pmax && bg[p] == ag[p] {
      p += 1;
    }
    let smax = (bg.len() - p).min(ag.len() - p);
    let mut s = 0;
    while s < smax && bg[bg.len() - 1 - s] == ag[ag.len() - 1 - s] {
      s += 1;
    }
    let removed: VarStr = Line(bg[p..bg.len() - s].to_vec()).to_var_str();
    let inserted: VarStr = Line(ag[p..ag.len() - s].to_vec()).to_var_str();
    Self {
      start: Pos { row, col: p },
      removed,
      inserted,
    }
  }
}

/// Exclusive end position of `text` laid out starting at `at`.
fn region_end(at: Pos, text: &str) -> Pos {
  let nl = text.matches('\n').count();
  if nl == 0 {
    Pos {
      row: at.row,
      col: at.col + text.graphemes(true).count(),
    }
  } else {
    let last = text.rsplit('\n').next().unwrap_or("");
    Pos {
      row: at.row + nl,
      col: last.graphemes(true).count(),
    }
  }
}

/// Replace the region `remove` occupies (laid out at `at`) with `insert`.
pub(super) fn splice_lines(lines: &mut Lines, at: Pos, remove: &str, insert: &str) {
  let end = region_end(at, remove);
  extract_range_contiguous(lines, at, end);
  insert_lines_at(lines, at, insert);
}

fn insert_lines_at(lines: &mut Lines, at: Pos, text: &str) {
  if text.is_empty() {
    return;
  }
  let row = at.row.min(lines.len().saturating_sub(1));
  let col = at.col.min(lines[row].len());
  if !text.contains('\n') {
    lines[row].insert_str(col, text);
    return;
  }
  let segs: Vec<&str> = text.split('\n').collect();
  let mut tail: Line = lines[row].split_off(col);
  lines[row].push_str(segs[0]);
  let mut new: Vec<Line> = segs[1..]
    .iter()
    .map(|seg| {
      let mut l = Line::default();
      l.push_str(seg);
      l
    })
    .collect();
  if let Some(last) = new.last_mut() {
    last.append(&mut tail);
  }
  for (i, l) in new.into_iter().enumerate() {
    lines.insert(row + 1 + i, l);
  }
}

/// A block whose body is indented one level until a matching closer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Opener {
  If,
  Case,
  For,
  Loop, // while / until
  Select,
  Brace,   // command group { … }
  Paren,   // subshell ( … )
  CaseArm, // one pat) … ;; arm inside a case
}

/// Maps a keyword to the block it opens. Middles (`then`/`do`/`else`/…) are
/// absent: depth changes at the opener, so they're neutral for the count.
fn keyword_opener(kw: &str) -> Option<Opener> {
  Some(match kw {
    "if" => Opener::If,
    "case" => Opener::Case,
    "for" => Opener::For,
    "while" | "until" => Opener::Loop,
    "select" => Opener::Select,
    _ => return None,
  })
}

/// The openers a closer keyword is allowed to close.
fn keyword_closes(kw: &str) -> Option<&'static [Opener]> {
  Some(match kw {
    "fi" => &[Opener::If],
    "done" => &[Opener::For, Opener::Loop, Opener::Select],
    _ => return None, // esac is handled specially (it may also close a dangling arm)
  })
}

fn collect_depth_events(tokens: &[CtxTk], stack: &mut Vec<Opener>, events: &mut Vec<(usize, i32)>) {
  for tk in tokens {
    let start = tk.span().range().start;
    match tk.class() {
      CtxTkRule::Keyword => {
        let kw = tk.span().as_str();
        if let Some(opener) = keyword_opener(kw) {
          stack.push(opener);
          events.push((start, 1));
        } else if kw == "esac" {
          // a final arm may omit its ;;, so close a dangling arm first, then the case
          if stack.last() == Some(&Opener::CaseArm) {
            stack.pop();
            events.push((start, -1));
          }
          if stack.last() == Some(&Opener::Case) {
            stack.pop();
            events.push((start, -1));
          }
        } else if let Some(targets) = keyword_closes(kw)
          && stack.last().is_some_and(|top| targets.contains(top))
        {
          stack.pop();
          events.push((start, -1));
        }
        // middles and unrelated keywords are depth-neutral
      }
      // command-level braces/subshells arrive as flat operators carrying the
      // delimiter text; pair them by text against the stack.
      CtxTkRule::Operator => match tk.span().as_str() {
        "{" => {
          stack.push(Opener::Brace);
          events.push((start, 1));
        }
        // a ( directly inside a case is a pattern's leading paren (a|b), not a
        // subshell, so it opens nothing; the ) opens the arm
        "(" if stack.last() == Some(&Opener::Case) => {}
        "(" => {
          stack.push(Opener::Paren);
          events.push((start, 1));
        }
        "}" if stack.last() == Some(&Opener::Brace) => {
          stack.pop();
          events.push((start, -1));
        }
        ")" if stack.last() == Some(&Opener::Paren) => {
          stack.pop();
          events.push((start, -1));
        }
        // a ) with the case itself on top closes a pattern, opening its arm
        ")" if stack.last() == Some(&Opener::Case) => {
          stack.push(Opener::CaseArm);
          events.push((start, 1));
        }
        _ => {}
      },
      // ;; ends the current arm. Its separator token swallows the leading
      // newline + indent, so find the ;; rather than using the span start
      // (which sits at the end of the previous row).
      CtxTkRule::Separator
        if stack.last() == Some(&Opener::CaseArm)
          && let Some(off) = tk.span().as_str().find(";;") =>
      {
        stack.pop();
        events.push((start + off, -1));
      }
      // $(…), backticks, process subs, arithmetic: their own scope. The parser
      // doesn't count the delimiter toward depth, so neither do we, but we still
      // recurse (isolated) to catch nested keyword blocks, unwinding any left open.
      CtxTkRule::Subshell
      | CtxTkRule::CmdSub
      | CtxTkRule::BacktickSub
      | CtxTkRule::ProcSubIn
      | CtxTkRule::ProcSubOut
      | CtxTkRule::Arithmetic => {
        let end = tk.span().range().end;
        let mut inner = Vec::new();
        collect_depth_events(tk.sub_tokens(), &mut inner, events);
        for _ in 0..inner.len() {
          events.push((end, -1));
        }
      }
      // an `arr=( … )` array literal indents its body, so unlike `$(…)` it does
      // count: open at the `(`, close at the `)` once it's there.
      CtxTkRule::ArrayLiteral => {
        events.push((start, 1));
        let end = tk.span().range().end;
        let mut inner = Vec::new();
        collect_depth_events(tk.sub_tokens(), &mut inner, events);
        for _ in 0..inner.len() {
          events.push((end, -1));
        }
        if tk.span().as_str().ends_with(')') {
          events.push((end - 1, -1));
        }
      }
      // args, strings, etc.: descend to catch nested expansions inside them
      _ => collect_depth_events(tk.sub_tokens(), stack, events),
    }
  }
}

/// Per-row `(start, end)` block depth from an already-built context-token slice.
/// `input` is only used for its newline positions (the token spans are absolute
/// offsets into it).
pub fn depth_levels_from_tokens(tokens: &[CtxTk], input: &str) -> Vec<(usize, usize)> {
  let mut events = Vec::new();
  let mut stack = Vec::new();
  collect_depth_events(tokens, &mut stack, &mut events);
  events.sort_by_key(|(pos, _)| *pos);

  let mut boundaries = vec![0usize];
  for (i, ch) in input.char_indices() {
    if ch == '\n' {
      boundaries.push(i + 1);
    }
  }
  let n_rows = boundaries.len();
  boundaries.push(input.len());

  let mut depth: i32 = 0;
  let mut ei = 0;
  let mut depths = Vec::with_capacity(boundaries.len());
  for &b in &boundaries {
    while ei < events.len() && events[ei].0 < b {
      depth += events[ei].1;
      ei += 1;
    }
    depths.push(depth.max(0) as usize);
  }

  (0..n_rows).map(|i| (depths[i], depths[i + 1])).collect()
}

/// Tokenize `input` and compute its per-row depth. Used where no token cache is
/// available (a cursor prefix) and by the parity test.
pub fn depth_levels_via_ctx(input: &str) -> Vec<(usize, usize)> {
  let tokens = crate::readline::context::get_context_tokens(input);
  depth_levels_from_tokens(&tokens, input)
}

/// Strict parse that flags unterminated structures (open quotes, subshells, …).
pub fn parse_failed_strict(input: &str) -> bool {
  ParsedSrc::new(input.into())
    .with_lex_flags(LexFlags::LEX_UNFINISHED_STRUCTURES)
    .with_parse_flags(ParseFlags::ERR_RETURN)
    .parse_src()
    .is_err()
}

pub(super) fn extract_range_contiguous(buf: &mut Lines, start: Pos, end: Pos) -> Lines {
  let start_col = start.col.min(buf[start.row].len());
  let end_col = end.col.min(buf[end.row].len());

  if start.row == end.row {
    // single line case
    let line = &mut buf[start.row];
    let removed: Vec<Grapheme> = line.0.drain(start_col..end_col).collect();
    return Lines(vec![Line(removed)]);
  }

  // multi line case
  // tail of first line
  let first_tail: Line = buf[start.row].split_off(start_col);

  // all inbetween lines. extracts nothing if only two rows
  let middle: Lines = buf.drain(start.row + 1..end.row).collect();

  // head of last line
  let last_col = end_col.min(buf[start.row + 1].len());
  let last_head: Line = Line::from(buf[start.row + 1].0.drain(..last_col).collect::<Vec<_>>());

  // tail of last line
  let mut last_remainder = buf.remove(start.row + 1);

  // attach tail of last line to head of first line
  buf[start.row].append(&mut last_remainder);

  // construct vector of extracted content
  let mut extracts = vec![first_tail];
  extracts.extend(middle.0);
  extracts.push(last_head);
  Lines(extracts)
}

impl super::LineBuf {
  /// Provides a public interface for editing the buffer in a way that is recognized by the undo system.
  /// Any change made by the provided function will be tracked in the undo stack.
  pub fn edit<T, F: FnMut(&mut Self) -> T>(&mut self, mut f: F) -> T {
    let before = self.lines.clone();
    let old_cursor = self.cursor.pos;

    let res = f(self);

    if self.is_empty() {
      self.set_hint(None);
    }

    let new_cursor = self.cursor.pos;
    self.handle_edit(before, new_cursor, old_cursor);

    res
  }
  pub fn handle_edit(&mut self, old: Lines, new_cursor: Pos, old_cursor: Pos) {
    self.record_edit(old, old_cursor, new_cursor, self.merging_undos);
  }
  /// Record an edit from `old` (the pre-edit buffer) to the current buffer. An
  /// open merge at the top of the stack absorbs the change; otherwise a new
  /// entry is pushed, kept as snapshots when `want_merge` so later edits fold in.
  pub(super) fn record_edit(
    &mut self,
    old: Lines,
    old_cursor: Pos,
    new_cursor: Pos,
    want_merge: bool,
  ) {
    if self.undo_stack.last().is_some_and(|e| e.merging) {
      let new = self.lines.clone();
      self.undo_stack.last_mut().unwrap().set_new(new, new_cursor);
    } else if want_merge {
      let new = self.lines.clone();
      self
        .undo_stack
        .push(Edit::snapshot(old, new, old_cursor, new_cursor, true));
    } else {
      let Diff {
        start,
        removed,
        inserted,
      } = Diff::new(&old, &self.lines);
      self.undo_stack.push(Edit::delta(
        start, removed, inserted, old_cursor, new_cursor,
      ));
    }
  }
  /// Open or close the top entry as a merge target, converting its
  /// representation so that "merging ⟺ snapshot" always holds.
  pub(super) fn set_top_merging(&mut self, merging: bool) {
    if merging {
      if self.undo_stack.last().is_some_and(|e| !e.merging) {
        let current = self.lines.clone();
        let top = self.undo_stack.last_mut().unwrap();
        top.make_snapshot(&current);
        top.merging = true;
      }
    } else if let Some(top) = self.undo_stack.last_mut() {
      top.finalize();
      top.merging = false;
    }
  }
}

#[cfg(test)]
mod depth_levels_tests {
  use super::depth_levels_via_ctx;

  // Expected per-row (start, end) depths across the shell's block constructs.
  // These were validated against the parser's `block_depth` before that field
  // was retired; this pins the token-based derivation against regressions.
  #[rustfmt::skip]
  #[expect(clippy::type_complexity)]
  const BATTERY: &[(&str, &str, &[(usize, usize)])] = &[
    ("if 1-line", "if true; then\n  echo hi\nfi", &[(0,1),(1,1),(1,0)]),
    ("if multiline", "if true\nthen\n  echo hi\nfi", &[(0,1),(1,1),(1,1),(1,0)]),
    ("if/elif/else", "if a; then\n  b\nelif c; then\n  d\nelse\n  e\nfi", &[(0,1),(1,1),(1,1),(1,1),(1,1),(1,1),(1,0)]),
    ("nested if", "if a; then\n  if b; then\n    c\n  fi\nfi", &[(0,1),(1,2),(2,2),(2,1),(1,0)]),
    ("for/do/done", "for x in a b; do\n  echo $x\ndone", &[(0,1),(1,1),(1,0)]),
    ("while", "while true; do\n  echo hi\ndone", &[(0,1),(1,1),(1,0)]),
    ("func braces", "foo() {\n  bar() {\n    echo hi\n  }\n}", &[(0,1),(1,2),(2,2),(2,1),(1,0)]),
    ("brace group", "{\n  echo a\n  echo b\n}", &[(0,1),(1,1),(1,1),(1,0)]),
    ("subshell", "(\n  echo a\n  echo b\n)", &[(0,1),(1,1),(1,1),(1,0)]),
    ("cmdsub multiline", "x=$(\n  echo a\n)\necho $x", &[(0,0),(0,0),(0,0),(0,0)]),
    ("for inside if", "if a; then\n  for x in y; do\n    z\n  done\nfi", &[(0,1),(1,2),(2,2),(2,1),(1,0)]),
    ("unclosed if", "if true; then\n  echo hi", &[(0,1),(1,1)]),
    ("case", "case $x in\n  a)\n    echo a\n    ;;\nesac", &[(0,1),(1,2),(2,2),(2,1),(1,0)]),
    ("case multi-arm", "case $x in\n  a)\n    echo a\n    ;;\n  b)\n    echo b\n    ;;\nesac", &[(0,1),(1,2),(2,2),(2,1),(1,2),(2,2),(2,1),(1,0)]),
    ("case no trailing ;;", "case $x in\n  a)\n    echo a\nesac", &[(0,1),(1,2),(2,2),(2,0)]),
    ("case in if", "if t; then\n  case $x in\n    a)\n      b\n      ;;\n  esac\nfi", &[(0,1),(1,2),(2,3),(3,3),(3,2),(2,1),(1,0)]),
    ("case leading paren", "case $x in\n  (a|b)\n    echo ab\n    ;;\nesac", &[(0,1),(1,2),(2,2),(2,1),(1,0)]),
    ("subshell in arm", "case $x in\n  a)\n    (\n      echo a\n    )\n    ;;\nesac", &[(0,1),(1,2),(2,3),(3,3),(3,2),(2,1),(1,0)]),
    ("plain lines", "echo a\necho b\necho c", &[(0,0),(0,0),(0,0)]),
    ("array literal", "arr=(\n  a\n  b\n)", &[(0,1),(1,1),(1,1),(1,0)]),
    ("array one-line", "arr=(a b c)\necho hi", &[(0,0),(0,0)]),
    ("array in func", "f() {\n  x=(\n    a\n  )\n}", &[(0,1),(1,2),(2,2),(2,1),(1,0)]),
    ("function kw", "function foo {\n  echo hi\n}", &[(0,1),(1,1),(1,0)]),
    ("dbl bracket test", "if [[ -f x ]]; then\n  echo y\nfi", &[(0,1),(1,1),(1,0)]),
    ("arith command", "if (( 1 + 1 )); then\n  echo y\nfi", &[(0,1),(1,1),(1,0)]),
    ("nested everything", "for x in a; do\n  if [[ $x ]]; then\n    case $x in\n      a)\n        (\n          echo deep\n        )\n        ;;\n    esac\n  fi\ndone", &[(0,1),(1,2),(2,3),(3,4),(4,5),(5,5),(5,4),(4,3),(3,2),(2,1),(1,0)]),
    ("pipe across lines", "echo a |\n  grep b |\n  wc -l", &[(0,0),(0,0),(0,0)]),
    ("heredoc", "cat <<EOF\nbody\nEOF\necho done", &[(0,0),(0,0),(0,0),(0,0)]),
  ];

  #[test]
  fn depth_battery() {
    for (label, input, expected) in BATTERY {
      assert_eq!(depth_levels_via_ctx(input), *expected, "{label}");
    }
  }
}
