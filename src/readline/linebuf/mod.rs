use std::{collections::VecDeque, ops::Range};

use unicode_segmentation::UnicodeSegmentation;

use crate::{
  eval::lex::{LexFlags, LexStream, TkFlags, TkRule},
  expand::{self, alias},
  motion,
  readline::linebuf::edit::{EditStack, RecordPolicy},
  shopt,
  state::{
    self, Shed, paths,
    vars::{VarFlags, VarKind, VarStr},
  },
  status_msg,
  util::error::ShResult,
};

use super::{
  context::{self, CtxTkRule},
  editcmd,
  editcmd::{EditCmd, Motion},
  editmode, highlight,
  history::History,
  register, stash,
};

mod char_class;
mod edit;
mod excmd;
mod hint;
mod killring;
mod motion;
mod ops;
mod pos;
mod select;
mod types;
mod verb;

pub(crate) use super::util::pos::{Pos, SignedPos};
pub(crate) use char_class::CharClass;
pub(crate) use hint::Hint;
pub(crate) use killring::KillRing;
pub(crate) use ops::{rot13_char, toggle_case_char};
pub(crate) use pos::{Cursor, MotionKind};
pub(crate) use select::{SelectMode, SelectShape};
pub(crate) use types::{Grapheme, Line, Lines};

pub(crate) const DEFAULT_VIEWPORT_HEIGHT: usize = 40;

/// Holds and manages edits for the current in-progress command.
/// This struct is the beating heart of `shed`'s line editor.
///
/// Consumes `EditCmd`s to perform edits.
///
/// ## Structure
///
/// As opposed to traditional flat-string style approaches for line editors,
/// we instead use a `Lines`, which nests data three layers deep:
///
/// 1. `Lines(Vec<Line>)` - has methods for operating on ranges of lines
/// 2. `Line(Vec<Grapheme>)` - has methods for operating on ranges of graphemes
/// 3. `Grapheme(SmallVec<[char;4]>)` - has methods for closely inspecting UTF-8 grapheme clusters
///
/// This results in a 2D array of graphemes, and a 3D array of UTF-8 codepoints. Linewise operations become very simple;
/// lookup is an O(1) index into a vector, operations on whole lines can just use a range like
/// `self.lines[0..5]`, etc. Cursor columns are also simpler in this case; an emoji with
/// several zero-width-joiners is the exact same size as an ascii character. We can perform
/// operations without needing to tip-toe around `char` boundaries.
///
/// ## Tradeoffs
///
/// The tradeoff is that contiguous operations spanning multiple lines become somewhat complex to handle.
/// With a flat string you just include the newline in the operation. With our model, we don't have newlines.
///
/// Personally I think the tradeoff is worth it, after working with both the flat string model and the 2D grid model.
/// Scanning for newlines has proven to be an exceptionally fragile method of performing linewise operations, which
/// is what necessitated this design in the first place. In order to have robust support for many of `vim`'s more in-depth
/// features such as line-addressed ex mode commands, this design was what I landed on.
#[derive(Debug, Clone)]
pub(crate) struct LineBuf {
  lines: Lines,
  hint: Option<Hint>,
  cursor: Cursor,

  select_mode: Option<SelectMode>,
  last_selection: Option<(SelectMode, Pos)>,

  last_substitute: Option<EditCmd>,
  last_global: Option<EditCmd>,
  last_search: Option<Motion>,
  pending_search: Option<VarStr>,

  /// Set when the last command's `f`/`t`/search-style motion found no target.
  /// Drained by macro playback and the `vice` driver to abort a run.
  search_failed: bool,

  insert_mode_start_pos: Option<Pos>,
  saved_col: Option<usize>,
  viewport_cap: Option<usize>,

  scroll_offset: usize,

  edit_stack: EditStack,

  kill_ring: KillRing,

  concat_points: VecDeque<Pos>,
  highlight_cache: Option<HighlightCache>,
  indent_cache: Option<Vec<(usize, usize)>>,
  byte_positions: Option<Vec<(usize, Pos)>>,

  open_file: Option<VarStr>,
}

#[derive(Clone, Debug)]
pub(super) struct HighlightCache {
  pub joined: String,
  pub hash: u64,
  pub tokens: Vec<context::CtxTk>,
}

impl Default for LineBuf {
  fn default() -> Self {
    Self {
      lines: Lines::default(),
      hint: None,
      byte_positions: None,
      cursor: Cursor {
        pos: Pos { row: 0, col: 0 },
        exclusive: false,
      },
      select_mode: None,
      last_selection: None,
      last_substitute: None,
      last_global: None,
      last_search: None,
      pending_search: None,
      search_failed: false,
      insert_mode_start_pos: None,
      saved_col: None,
      viewport_cap: None,
      scroll_offset: 0,
      edit_stack: EditStack::new(),
      open_file: None,
      kill_ring: KillRing::new(),
      concat_points: VecDeque::new(),
      indent_cache: None,
      highlight_cache: None,
    }
  }
}

impl LineBuf {
  pub(crate) fn new() -> Self {
    Self::default()
  }
  pub(crate) fn cursor(&self) -> Pos {
    self.cursor.pos
  }
  pub(crate) fn lines(&self) -> &Lines {
    &self.lines
  }
  pub(crate) fn null_caches(&mut self) {
    self.indent_cache = None;
    self.byte_positions = None;
  }
  pub(crate) fn scroll_offset(&self) -> usize {
    self.scroll_offset
  }
  /// Whether the most recently executed command was a search-style motion
  /// (`f`/`t`/`;`/`,`/`n`/`N`) that found no target.
  pub(crate) fn search_failed(&self) -> bool {
    self.search_failed
  }
  pub(super) fn exec_cmd(&mut self, cmd: &EditCmd) -> ShResult<()> {
    let is_char_insert/*-*/= cmd.is_char_insert();
    let is_kill/*--------*/= cmd.is_kill();
    let is_killring_op/*-*/= cmd.is_killring_op();
    let starts_merge/*---*/= cmd.starts_merge();
    let is_vertical/*----*/= cmd.is_vertical();
    let is_undo_op/*-----*/= cmd.is_undo_op();
    let is_separator/*---*/= cmd.is_separator_insert();
    let is_edit/*--------*/= cmd.is_edit();

    if !is_vertical {
      self.saved_col = None;
    }
    self.search_failed = false;

    let res = if is_edit {
      let policy = if is_undo_op {
        RecordPolicy::Skip
      } else if is_char_insert || self.edit_stack.merging_undos() {
        RecordPolicy::Merge
      } else if starts_merge {
        RecordPolicy::Restart
      } else {
        RecordPolicy::Break
      };

      self.edit_with(policy, |this| {
        if is_separator
          && !this.grapheme_before_cursor().is_none_or(|gr| gr.is_ws())
          && shopt!(prompt.expand_aliases)
        {
          this.attempt_alias_expansion();
        }

        this.exec_verb(cmd)
      })
    } else {
      // not an edit, skip the edit handling and execute directly
      self.exec_verb(cmd)
    };

    let num_lines = self.lines().len();
    let cursor_line = self.cursor().row;

    Shed::vars_mut(|v| {
      v.set_var(
        "EDITOR_LINES",
        VarKind::string(num_lines.to_string().into()),
        VarFlags::READONLY,
      )?;
      v.set_var(
        "EDITOR_LINE",
        VarKind::string((cursor_line + 1).to_string().into()),
        VarFlags::READONLY,
      )
    })?;

    if let Some(file) = &self.open_file {
      let display = paths::display_path(file);
      Shed::vars_mut(|v| {
        v.set_var(
          "EDITOR_FILE",
          VarKind::string(display.into()),
          VarFlags::READONLY,
        )
      })?;
    }

    self.fix_cursor();
    if !is_kill {
      self.kill_ring.merging = false;
    }
    if !is_killring_op {
      self.kill_ring.reset();
    }

    if let Some(Hint::Override(hint_lines)) = self.hint.as_ref()
      && !self.lines.is_prefix_lines(hint_lines)
    {
      self.clear_hint();
    }

    res
  }

  pub(crate) fn attempt_inline_expansion(&mut self, history: &History) -> bool {
    let hist_res = self.attempt_history_expansion(history);
    let alias_res = shopt!(prompt.expand_aliases) && self.attempt_alias_expansion();

    hist_res || alias_res
  }

  pub(crate) fn attempt_alias_expansion_all(&mut self) -> bool {
    let raw = self.to_string();
    let (result, first_pos) = alias::expand_alias_with_pos(raw);
    if first_pos.is_some() {
      self.lines = Lines::to_lines(&result);
      true
    } else {
      false
    }
  }

  pub(crate) fn attempt_alias_expansion(&mut self) -> bool {
    self.edit_with(RecordPolicy::Skip, |this| {
      let (to_cursor, mut after_cursor) = this.lines.clone().split_lines(this.cursor.pos);
      let raw = to_cursor.join();
      let handle = state::register_source(raw.as_bytes());
      let mut tokens = LexStream::new(&handle, LexFlags::empty())
        .filter_map(Result::ok)
        .filter(|tk| !matches!(tk.class, TkRule::Null))
        .collect::<Vec<_>>();
      while tokens
        .last()
        .is_some_and(|tk| !tk.flags.contains(TkFlags::IS_CMD))
      {
        tokens.pop();
      }

      let Some(last) = tokens.pop() else {
        return false;
      };
      if !last.flags.contains(TkFlags::IS_CMD) {
        return false;
      }
      let tk_start = last.span.start();
      let word = last.slice();
      let word = &word.to_string();

      if let Some(alias) = Shed::logic(|l| l.aliases().get(word).cloned())
        && let alias = alias.to_string()
        && !raw[tk_start..].starts_with(&alias)
      {
        let delta = alias.graphemes(true).count() as isize - word.graphemes(true).count() as isize;
        let expanded = last.replaced(&alias);

        this.lines = Lines::to_lines(&expanded);
        this.lines.attach_lines(&mut after_cursor);
        this.cursor.pos = this.cursor.pos.col_add_signed(delta);

        true
      } else {
        false
      }
    })
  }

  pub(crate) fn attempt_history_expansion(&mut self, history: &History) -> bool {
    self.edit_with(RecordPolicy::Skip, |this| {
      let buf = this.to_string();
      let tks = context::get_context_tokens(&buf);
      let mut hist_expansions = vec![];
      for tk in &tks {
        hist_expansions.extend(tk.find_nodes(|n| *n.class() == CtxTkRule::HistExp));
      }
      hist_expansions.sort_by_key(|n| n.start());

      let mut any_changes = false;
      let mut changes: Vec<((Pos, Pos), String)> = vec![];
      for exp in hist_expansions {
        let range = exp.range();
        let Some(start) = this.byte_to_pos(range.start) else {
          continue;
        };
        let Some(mut end) = this.byte_to_pos(range.end) else {
          continue;
        };
        end = end.col_sub(1); // exclusive range
        let change = if let Some(s) = history.resolve_hist_token(&exp.slice().to_str_lossy()) {
          any_changes = true;
          s.clone()
        } else {
          any_changes = true;
          let raw = exp.slice();
          let raw = raw.to_str_lossy();
          raw
            .strip_prefix('!')
            .map_or_else(|| raw.to_string(), ToString::to_string)
        };

        changes.push(((start, end), change));
      }

      for (range, change) in changes.into_iter().rev() {
        let old_len = this.count_graphemes();
        this.replace_range(range, &change);
        let new_len = this.count_graphemes();
        let delta = new_len as isize - old_len as isize;
        let (nr, nc) = this.offset_col_wrapping(this.row(), delta);
        this.cursor.pos.set(nr, nc);
      }

      any_changes
    })
  }

  pub(crate) fn search_match_spans(&self) -> Vec<Range<usize>> {
    if let Some(pat) = self.pending_search.as_ref()
      && !pat.is_empty()
      && let Ok(re) = Shed::meta_mut(|m| m.get_regex(&pat.to_str_lossy()))
    {
      let buf = self.to_string();
      let positions = self.byte_positions();
      let lookup = |b: usize| -> Option<usize> {
        positions
          .iter()
          .find_map(|(off, _p)| (*off >= b).then_some(*off))
      };
      re.find_iter(&buf)
        .filter_map(|m| Some(lookup(m.start())?..lookup(m.end())?))
        .collect()
    } else {
      vec![]
    }
  }

  pub(crate) fn open_file(&self) -> Option<VarStr> {
    self.open_file.clone()
  }
}
