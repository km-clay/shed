use crate::libsh::strops::QuoteState;
use crate::motion;
use crate::readline::linebuf::ordered;
use editcmd::{CmdFlags, EditCmd, Motion, MotionCmd, RegisterName, Verb, VerbCmd};
use editmode::{CmdReplay, EditMode, ModeReport, ViInsert, ViNormal, ViReplace, ViVisual};
use history::History;
use itertools::Either;
use keys::{KeyCode, KeyEvent, ModKeys};
use linebuf::LineBuf;
use std::collections::VecDeque;
use std::fmt::Write;
use std::rc::Rc;
use term::{KeyReader, Layout, LineWriter, PollReader, TermWriter, get_win_size};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::builtin::keymap::{KeyMapFlags, KeyMapMatch};
use crate::expand::{expand_keymap, expand_prompt};
use crate::libsh::utils::AutoCmdVecUtils;
use crate::readline::complete::{FuzzyCompleter, SelectorResponse};
use crate::readline::editcmd::Direction;
use crate::readline::editmode::emacs::Emacs;
use crate::readline::editmode::{ViEx, ViVerbatim};
use crate::readline::history::HistEntry;
use crate::readline::term::{Pos, TermReader, calc_str_width};
use crate::state::{
  AutoCmdKind, ShellParam, Var, VarFlags, VarKind, read_logic, read_shopts, with_vars, write_meta,
  write_vars,
};
use crate::{
  libsh::error::ShResult,
  match_loop,
  parse::lex::{self, LexFlags, Tk, TkFlags, TkRule},
  readline::{
    complete::{CompResponse, Completer},
    highlight::Highlighter,
  },
};
use crate::{prelude::*, state};

pub mod complete;
pub mod editcmd;
pub mod editmode;
pub mod highlight;
pub mod history;
pub mod keys;
pub mod layout;
pub mod linebuf;
pub mod register;
pub mod term;

#[cfg(test)]
pub mod tests;

pub mod markers {
  use super::Marker;

  /*
   * These are invisible Unicode characters used to annotate
   * strings with various contextual metadata.
   */

  /* Highlight Markers */

  // token-level (derived from token class)
  pub const COMMAND: Marker = '\u{e100}';
  pub const BUILTIN: Marker = '\u{e101}';
  pub const ARG: Marker = '\u{e102}';
  pub const KEYWORD: Marker = '\u{e103}';
  pub const OPERATOR: Marker = '\u{e104}';
  pub const REDIRECT: Marker = '\u{e105}';
  pub const COMMENT: Marker = '\u{e106}';
  pub const ASSIGNMENT: Marker = '\u{e107}';
  pub const CMD_SEP: Marker = '\u{e108}';
  pub const CASE_PAT: Marker = '\u{e109}';
  pub const SUBSH: Marker = '\u{e10a}';
  pub const SUBSH_END: Marker = '\u{e10b}';

  // sub-token (needs scanning)
  pub const VAR_SUB: Marker = '\u{e10c}';
  pub const VAR_SUB_END: Marker = '\u{e10d}';
  pub const CMD_SUB: Marker = '\u{e10e}';
  pub const CMD_SUB_END: Marker = '\u{e10f}';
  pub const PROC_SUB: Marker = '\u{e110}';
  pub const PROC_SUB_END: Marker = '\u{e111}';
  pub const STRING_DQ: Marker = '\u{e112}';
  pub const STRING_DQ_END: Marker = '\u{e113}';
  pub const STRING_SQ: Marker = '\u{e114}';
  pub const STRING_SQ_END: Marker = '\u{e115}';
  pub const ESCAPE: Marker = '\u{e116}';
  pub const GLOB: Marker = '\u{e117}';
  pub const HIST_EXP: Marker = '\u{e11c}';
  pub const HIST_EXP_END: Marker = '\u{e11d}';
  pub const BACKTICK_SUB: Marker = '\u{e11e}';
  pub const BACKTICK_SUB_END: Marker = '\u{e11f}';

  // other
  pub const VISUAL_MODE_START: Marker = '\u{e118}';
  pub const VISUAL_MODE_END: Marker = '\u{e119}';

  pub const RESET: Marker = '\u{e11a}';

  pub const NULL: Marker = '\u{e11b}';

  /* Expansion Markers */
  /// Double quote '"' marker
  pub const DUB_QUOTE: Marker = '\u{e001}';
  /// Single quote '\\'' marker
  pub const SNG_QUOTE: Marker = '\u{e002}';
  /// Tilde sub marker
  pub const TILDE_SUB: Marker = '\u{e003}';
  /// Input process sub marker
  pub const PROC_SUB_IN: Marker = '\u{e005}';
  /// Output process sub marker
  pub const PROC_SUB_OUT: Marker = '\u{e006}';
  /// Marker for null expansion
  /// This is used for when "$@" or "$*" are used in quotes and there are no
  /// arguments Without this marker, it would be handled like an empty string,
  /// which breaks some commands
  pub const NULL_EXPAND: Marker = '\u{e007}';
  /// Explicit marker for argument separation
  /// This is used to join the arguments given by "$@", and preserves exact
  /// formatting of the original arguments, including quoting
  pub const ARG_SEP: Marker = '\u{e008}';

  pub const VI_SEQ_EXP: Marker = '\u{e009}';

  // Ex mode highlighting markers
  pub const EX_LINE_ADDR: Marker = '\u{e00a}';
  pub const EX_CMD: Marker = '\u{e00b}';
  pub const EX_DELIM: Marker = '\u{e00c}';
  pub const EX_PAT: Marker = '\u{e00d}';
  pub const EX_SHELL_CMD: Marker = '\u{e00e}';

  pub const END_MARKERS: [Marker; 7] = [
    VAR_SUB_END,
    CMD_SUB_END,
    PROC_SUB_END,
    STRING_DQ_END,
    STRING_SQ_END,
    SUBSH_END,
    RESET,
  ];
  pub const TOKEN_LEVEL: [Marker; 10] = [
    SUBSH, COMMAND, BUILTIN, ARG, KEYWORD, OPERATOR, REDIRECT, CMD_SEP, CASE_PAT, ASSIGNMENT,
  ];
  pub const SUB_TOKEN: [Marker; 6] = [VAR_SUB, CMD_SUB, PROC_SUB, STRING_DQ, STRING_SQ, GLOB];

  pub const MISC: [Marker; 3] = [ESCAPE, VISUAL_MODE_START, VISUAL_MODE_END];

  pub fn is_marker(c: Marker) -> bool {
    ('\u{e000}'..'\u{efff}').contains(&c)
  }

  // Help command formatting markers
  pub const TAG: Marker = '\u{e180}';
  pub const REFERENCE: Marker = '\u{e181}';
  pub const HEADER: Marker = '\u{e182}';
  pub const CODE: Marker = '\u{e183}';
  /// angle brackets
  pub const KEYWORD_1: Marker = '\u{e184}';
  /// curly brackets
  pub const KEYWORD_2: Marker = '\u{e185}';
  /// square brackets
  pub const KEYWORD_3: Marker = '\u{e186}';
}
type Marker = char;

/// Non-blocking readline result
pub enum ReadlineEvent {
  /// A complete line was entered
  Line(String),
  /// Ctrl+D on empty line - request to exit
  Eof,
  /// No complete input yet, need more bytes
  Pending,
}

pub struct LineData {
	pub buffer: String,
	pub cursor: usize,
	pub anchor: Option<usize>,
	pub hint: Option<String>,
	pub mode: String
}

pub struct Prompt {
  ps1_expanded: String,
  ps1_raw: String,
  psr_expanded: Option<String>,
  psr_raw: Option<String>,
  dirty: bool,
}

impl Prompt {
  const DEFAULT_PS1: &str =
    "\\e[0m\\n\\e[1;0m\\u\\e[1;36m@\\e[1;31m\\h\\n\\e[1;36m\\W\\e[1;32m/\\n\\e[1;32m\\$\\e[0m ";
  pub fn new() -> Self {
    let pre_prompt = read_logic(|l| l.get_autocmds(AutoCmdKind::PrePrompt));
    pre_prompt.exec();

    let Ok(ps1_raw) = env::var("PS1") else {
      return Self::default();
    };
    // PS1 expansion may involve running commands (e.g., for \h or \W), which can modify shell state
    let saved_status = state::get_status();

    let Ok(ps1_expanded) = expand_prompt(&ps1_raw) else {
      return Self::default();
    };
    let psr_raw = env::var("PSR").ok();
    let psr_expanded = psr_raw
      .clone()
      .map(|r| expand_prompt(&r))
      .transpose()
      .ok()
      .flatten();

    // Restore shell state after prompt expansion, since it may have been modified by command substitutions in the prompt
    state::set_status(saved_status);

    let post_prompt = read_logic(|l| l.get_autocmds(AutoCmdKind::PostPrompt));
    post_prompt.exec();

    Self {
      ps1_expanded,
      ps1_raw,
      psr_expanded,
      psr_raw,
      dirty: false,
    }
  }

  pub fn get_ps1(&mut self) -> &str {
    if self.dirty {
      self.refresh_now();
    }
    &self.ps1_expanded
  }
  pub fn set_ps1(&mut self, ps1_raw: String) -> ShResult<()> {
    self.ps1_raw = ps1_raw;
    self.dirty = true;
    Ok(())
  }
  pub fn set_psr(&mut self, psr_raw: String) -> ShResult<()> {
    self.psr_raw = Some(psr_raw);
    self.dirty = true;
    Ok(())
  }
  pub fn get_psr(&mut self) -> Option<&str> {
    if self.dirty {
      self.refresh_now();
    }
    self.psr_expanded.as_deref()
  }

  /// Mark the prompt as needing re-expansion on next access.
  pub fn invalidate(&mut self) {
    self.dirty = true;
  }

  fn refresh_now(&mut self) {
    let saved_status = state::get_status();
    *self = Self::new();
    state::set_status(saved_status);
    self.dirty = false;
  }

  pub fn refresh(&mut self) {
    self.invalidate();
  }
}

impl Default for Prompt {
  fn default() -> Self {
    Self {
      ps1_expanded: expand_prompt(Self::DEFAULT_PS1)
        .unwrap_or_else(|_| Self::DEFAULT_PS1.to_string()),
      ps1_raw: Self::DEFAULT_PS1.to_string(),
      psr_expanded: None,
      psr_raw: None,
      dirty: false,
    }
  }
}

pub struct ShedLine {
  pub reader: PollReader,
  pub writer: TermWriter,
  pub tty: RawFd,

  pub prompt: Prompt,
  pub highlighter: Highlighter,
  pub completer: Box<dyn Completer>,

  pub mode: Box<dyn EditMode>,
  pub saved_mode: Option<Box<dyn EditMode>>,
  pub pending_keymap: Vec<KeyEvent>,
  pub repeat_action: Option<CmdReplay>,
  pub repeat_motion: Option<MotionCmd>,
  pub editor: LineBuf,

  pub old_layout: Option<Layout>,
  pub history: History,
  pub ex_history: History,

  pub needs_redraw: bool,
  pub ctrl_d_warning_counter: usize,
  pub status_msgs: VecDeque<(String, Instant)>,
}

impl ShedLine {
  pub fn new(prompt: Prompt, tty: RawFd) -> ShResult<Self> {
    Self::new_private(prompt, tty, true)
  }

  pub fn new_no_hist(prompt: Prompt, tty: RawFd) -> ShResult<Self> {
    Self::new_private(prompt, tty, false)
  }

  fn new_private(prompt: Prompt, tty: RawFd, with_hist: bool) -> ShResult<Self> {
    let history = if with_hist {
      History::new("shed_history").unwrap()
    } else {
      History::empty("shed_history")
    };
    let mode = if read_shopts(|o| o.set.vi) {
      Box::new(ViInsert::new()) as Box<dyn EditMode>
    } else {
      Box::new(Emacs::new()) as Box<dyn EditMode>
    };
    let mut new = Self {
      reader: PollReader::new(),
      writer: TermWriter::new(tty),
      prompt,
      tty,
      completer: Box::new(FuzzyCompleter::default()),
      highlighter: Highlighter::new(),
      mode,
      saved_mode: None,
      pending_keymap: Vec::new(),
      old_layout: None,
      repeat_action: None,
      repeat_motion: None,
      editor: LineBuf::new(),
      history,
      ex_history: History::new("ex_history")?,
      needs_redraw: true,
      ctrl_d_warning_counter: 0,
      status_msgs: VecDeque::new(),
    };
    write_vars(|v| {
      v.set_var(
        "SHED_VI_MODE",
        VarKind::Str(new.mode.report_mode().to_string()),
        VarFlags::NONE,
      )
    })?;
    new.prompt.refresh();
    new.writer.flush_write("\n")?; // ensure we start on a new line, in case the previous command didn't end with a newline
    new.print_line(false)?;
    Ok(new)
  }

  pub fn with_initial(mut self, initial: &str) -> Self {
    self.editor = LineBuf::new().with_initial(initial, 0);
    {
      let s = self.editor.joined();
      let c = self.editor.cursor_to_flat();
      self.focused_history().update_pending_cmd((&s, c));
    }
    self
  }

	pub fn get_line_data(&self) -> LineData {
		LineData {
			buffer: self.editor.joined().replace('\n', "\\n"),
			cursor: self.editor.cursor_to_flat(),
			anchor: self.editor.anchor_to_flat(),
			hint: self.editor.try_join_hint().map(|s| s.replace('\n', "\\n")),
			mode: self.mode.report_mode().to_string(),
		}
	}

  /// A mutable reference to the currently focused editor
  /// This includes the main LineBuf, and sub-editors for modes like Ex mode.
  pub fn focused_editor(&mut self) -> &mut LineBuf {
    self.mode.editor().unwrap_or(&mut self.editor)
  }

  /// A mutable reference to the currently focused history, if any.
  /// This includes the main history struct, and history for sub-editors like Ex mode.
  pub fn focused_history(&mut self) -> &mut History {
    self.mode.history().unwrap_or(&mut self.history)
  }

  /// Feed raw bytes from stdin into the reader's buffer
  pub fn feed_bytes(&mut self, bytes: &[u8]) {
    self.reader.feed_bytes(bytes);
  }

  /// Mark that the display needs to be redrawn (e.g., after SIGWINCH)
  pub fn mark_dirty(&mut self) {
    self.needs_redraw = true;
  }

  pub fn fix_column(&mut self) -> ShResult<()> {
    self
      .writer
      .fix_cursor_column(&mut TermReader::new(self.tty))
  }

  pub fn reset_active_widget(&mut self, full_redraw: bool) -> ShResult<()> {
    if self.completer.is_active() {
      self.completer.reset_stay_active();
      self.needs_redraw = true;
      Ok(())
    } else if self.focused_history().fuzzy_finder.is_active() {
      self.focused_history().fuzzy_finder.reset_stay_active();
      self.needs_redraw = true;
      Ok(())
    } else {
      self.reset(full_redraw)
    }
  }

  /// Reset readline state for a new prompt
  pub fn reset(&mut self, full_redraw: bool) -> ShResult<()> {
    // Clear old display before resetting state - old_layout must survive
    // so print_line can call clear_rows with the full multi-line layout
    self.prompt.refresh();
    self.editor = Default::default();
    let mut mode = if read_shopts(|o| o.set.vi) {
      Box::new(ViInsert::new()) as Box<dyn EditMode>
    } else {
      Box::new(Emacs::new()) as Box<dyn EditMode>
    };

    self.swap_mode(&mut mode);
    self.needs_redraw = true;
    if full_redraw {
      self.old_layout = None;
    }
    self.focused_history().pending = None;
    self.focused_history().reset();
    self.print_line(false)
  }

  pub fn prompt(&self) -> &Prompt {
    &self.prompt
  }

  pub fn prompt_mut(&mut self) -> &mut Prompt {
    &mut self.prompt
  }

  pub fn curr_keymap_flags(&self) -> KeyMapFlags {
    let mut flags = KeyMapFlags::empty();
    match self.mode.report_mode() {
      ModeReport::Insert => flags |= KeyMapFlags::INSERT,
      ModeReport::Normal => flags |= KeyMapFlags::NORMAL,
      ModeReport::Ex => flags |= KeyMapFlags::EX,
      ModeReport::Visual => flags |= KeyMapFlags::VISUAL,
      ModeReport::Replace => flags |= KeyMapFlags::REPLACE,
      ModeReport::Verbatim => flags |= KeyMapFlags::VERBATIM,
      ModeReport::Emacs => flags |= KeyMapFlags::EMACS,
      ModeReport::Unknown => panic!("Unknown mode report"),
    }

    if self.mode.pending_seq().is_some_and(|seq| !seq.is_empty()) {
      flags |= KeyMapFlags::OP_PENDING;
    }

    flags
  }

  /// This method ensures that the editing mode (Vi or Emacs) matches the 'vi' option, and switches modes if necessary.
  pub fn fix_editing_mode(&mut self) {
    if read_shopts(|o| o.set.vi) && self.mode.report_mode() == ModeReport::Emacs {
      self.swap_mode(&mut (Box::new(ViInsert::new()) as Box<dyn EditMode>));
    } else if !read_shopts(|o| o.set.vi) && self.mode.report_mode() != ModeReport::Emacs {
      self.swap_mode(&mut (Box::new(Emacs::new()) as Box<dyn EditMode>));
    }
  }

  fn should_submit(&mut self) -> ShResult<bool> {
    if self.mode.report_mode() == ModeReport::Normal {
      return Ok(true);
    }
    if self.editor.cursor_is_escaped()
      && matches!(
        self.mode.report_mode(),
        ModeReport::Emacs | ModeReport::Insert
      )
    {
      return Ok(false);
    }
    let (depth, failed) = self.editor.checked_calc_indent_level();
    Ok(depth == 0 && !failed)
  }

  fn handle_hist_search_key(&mut self, key: KeyEvent) -> ShResult<()> {
    self.print_line(false)?;
    match self.focused_history().fuzzy_finder.handle_key(key)? {
      SelectorResponse::Accept(cmd) => {
        let post_cmds = read_logic(|l| l.get_autocmds(AutoCmdKind::OnHistorySelect));

        let entry_idx = cmd.id().unwrap(); // history entries having an id to unwrap is an invariant.
        self.scroll_history_to(entry_idx);
        {
          let mut writer = std::mem::take(&mut self.writer);
          self.focused_history().fuzzy_finder.clear(&mut writer)?;
          self.writer = writer;
        }
        self.focused_history().fuzzy_finder.reset();

        with_vars([("_HIST_ENTRY".into(), cmd.content().to_string())], || {
          post_cmds.exec();
        });

        write_vars(|v| {
          v.set_var(
            "SHED_VI_MODE",
            VarKind::Str(self.mode.report_mode().to_string()),
            VarFlags::NONE,
          )
        })
        .ok();
        self.prompt.refresh();
        self.needs_redraw = true;
      }
      SelectorResponse::Dismiss => {
        let post_cmds = read_logic(|l| l.get_autocmds(AutoCmdKind::OnHistoryClose));
        post_cmds.exec();

        self.editor.clear_hint();
        {
          let mut writer = std::mem::take(&mut self.writer);
          self.focused_history().fuzzy_finder.clear(&mut writer)?;
          self.writer = writer;
        }
        write_vars(|v| {
          v.set_var(
            "SHED_VI_MODE",
            VarKind::Str(self.mode.report_mode().to_string()),
            VarFlags::NONE,
          )
        })
        .ok();
        self.prompt.refresh();
        self.needs_redraw = true;
      }
      SelectorResponse::Consumed => {
        self.needs_redraw = true;
      }
    }
    Ok(())
  }

  fn handle_completion_key(&mut self, key: &KeyEvent) -> ShResult<bool> {
    self.print_line(false)?;
    match self.completer.handle_key(key.clone())? {
      CompResponse::Accept(candidate) => {
        let post_cmds = read_logic(|l| l.get_autocmds(AutoCmdKind::OnCompletionSelect));

        let span_start = self.completer.token_span().0;
        let new_cursor = span_start + candidate.len();
        let line = self.completer.get_completed_line(&candidate);
        self.focused_editor().set_buffer(line);
        self.focused_editor().set_cursor_from_flat(new_cursor);
        // Don't reset yet - clear() needs old_layout to erase the selector.

        if !self.focused_history().at_pending() {
          self.focused_history().reset_to_pending();
        }
        self
          .history
          .update_pending_cmd((&self.editor.joined(), self.editor.cursor_to_flat()));
        let hint = self.focused_history().get_hint();
				self.editor.set_hint(hint);
        self.completer.clear(&mut self.writer)?;
        self.needs_redraw = true;
        self.completer.reset();

        write_vars(|v| {
          v.set_var(
            "SHED_VI_MODE",
            VarKind::Str(self.mode.report_mode().to_string()),
            VarFlags::NONE,
          )
        })
        .ok();
        self.prompt.refresh();

        with_vars(
          [("_COMP_CANDIDATE".into(), candidate.content().to_string())],
          || {
            post_cmds.exec();
          },
        );

        Ok(true)
      }
      CompResponse::Dismiss => {
        let post_cmds = read_logic(|l| l.get_autocmds(AutoCmdKind::OnCompletionCancel));
        post_cmds.exec();

        let hint = self.focused_history().get_hint();
				self.editor.set_hint(hint);
        self.completer.clear(&mut self.writer)?;
        write_vars(|v| {
          v.set_var(
            "SHED_VI_MODE",
            VarKind::Str(self.mode.report_mode().to_string()),
            VarFlags::NONE,
          )
        })
        .ok();
        self.prompt.refresh();
        self.completer.reset();
        Ok(true)
      }
      CompResponse::Consumed => {
        /* just redraw */
        self.needs_redraw = true;
        Ok(true)
      }
      CompResponse::Passthrough => Ok(false),
    }
  }

  fn handle_keymap(&mut self, key: KeyEvent) -> ShResult<Option<ReadlineEvent>> {
    let keymap_flags = self.curr_keymap_flags();
    self.pending_keymap.push(key.clone());

    let mut matches = read_logic(|l| l.keymaps_filtered(keymap_flags, &self.pending_keymap));
		let is_exact = matches.len() == 1 && matches[0].compare(&self.pending_keymap) == KeyMapMatch::IsExact;

    if matches.is_empty() {
      // No matches. Drain the buffered keys and execute them.
      for key in std::mem::take(&mut self.pending_keymap) {
        if let Some(event) = self.handle_key(key)? {
          return Ok(Some(event));
        }
      }
      self.needs_redraw = true;
    } else if is_exact {
      // We have a single exact match. Execute it.
      let keymap = matches.remove(0);
      self.pending_keymap.clear();
      let action = keymap.action_expanded();
      for key in action {
        if let Some(event) = self.handle_key(key)? {
          return Ok(Some(event));
        }
      }
      self.needs_redraw = true;
    }

    // There is ambiguity. Allow the timeout in the main loop to handle this.
    Ok(None)
  }

  /// Process any available input and return readline event
  /// This is non-blocking - returns Pending if no complete line yet
  pub fn process_input(&mut self) -> ShResult<ReadlineEvent> {
    // Redraw if needed
    if self.needs_redraw {
      self.print_line(false)?;
      self.needs_redraw = false;
    }

    // Process all available keys
    while let Some(key) = self.reader.readkey()? {
      // If completer or history search are active, delegate input to it
      if self.focused_history().fuzzy_finder.is_active() {
        self.handle_hist_search_key(key)?;
        continue;
      } else if self.completer.is_active() && self.handle_completion_key(&key)? {
        // self.handle_completion_key() returns true if we need to continue the loop
        continue;
      } else if self.mode.pending_seq().is_some_and(|seq| !seq.is_empty()) {
        // Vi mode is waiting for more input (e.g. after 'f', 'd', etc.)
        // Bypass keymap matching and send directly to the mode handler
        if let Some(event) = self.handle_key(key)? {
          return Ok(event);
        }
        self.needs_redraw = true;
        continue;
      } else if let Some(event) = self.handle_keymap(key)? {
        return Ok(event);
      }
    }
    if !self.completer.is_active() && !self.focused_history().fuzzy_finder.is_active() {
      write_vars(|v| {
        v.set_var(
          "SHED_VI_MODE",
          VarKind::Str(self.mode.report_mode().to_string()),
          VarFlags::NONE,
        )
      })
      .ok();
    }

    // Redraw if we processed any input
    if self.needs_redraw {
      self.print_line(false)?;
      self.needs_redraw = false;
    }
		let line_data = self.get_line_data();
		write_meta(|m| m.notify_line_edit(line_data)).ok();

    Ok(ReadlineEvent::Pending)
  }

  fn accept_hint(&mut self) -> ShResult<Option<ReadlineEvent>> {
		self.editor.edit(|e| {
			e.accept_hint();
		});
    if !self.focused_history().at_pending() {
      self.focused_history().reset_to_pending();
    }
    self
      .history
      .update_pending_cmd((&self.editor.joined(), self.editor.cursor_to_flat()));
    self.needs_redraw = true;

    Ok(None)
  }

  fn handle_tab(&mut self, key: KeyEvent) -> ShResult<Option<ReadlineEvent>> {
    let KeyEvent(KeyCode::Tab, mod_keys) = key else {
      return Ok(None);
    };

    if self.mode.report_mode() != ModeReport::Ex
      && self.editor.edit(|e| e.attempt_history_expansion(&self.history))
    {
      // If history expansion occurred, don't attempt completion yet
      // allow the user to see the expanded command and accept or edit it before completing
      return Ok(None);
    }

    let direction = match mod_keys {
      ModKeys::SHIFT => -1,
      _ => 1,
    };
    let line = self.focused_editor().joined();
    let cursor_pos = self.focused_editor().cursor_byte_pos();

    match self.completer.complete(line, cursor_pos, direction) {
      Err(e) => {
        e.print_error();
        // Printing the error invalidates the layout
        self.old_layout = None;
      }
      Ok(Some(line)) => {
        let post_cmds = read_logic(|l| l.get_autocmds(AutoCmdKind::OnCompletionSelect));
        let cand = self.completer.selected_candidate().unwrap_or_default();
        with_vars(
          [("_COMP_CANDIDATE".into(), cand.content().to_string())],
          || {
            post_cmds.exec();
          },
        );

        let span_start = self.completer.token_span().0;

        let new_cursor = span_start
          + self
            .completer
            .selected_candidate()
            .map(|c| c.len())
            .unwrap_or_default();

        self.focused_editor().set_buffer(line.clone());
        self.focused_editor().set_cursor_from_flat(new_cursor);

        if !self.focused_history().at_pending() {
          self.focused_history().reset_to_pending();
        }
        self
          .history
          .update_pending_cmd((&self.editor.joined(), self.editor.cursor_to_flat()));
        let hint = self.focused_history().get_hint();
				self.editor.set_hint(hint);
        write_vars(|v| {
          v.set_var(
            "SHED_VI_MODE",
            VarKind::Str(self.mode.report_mode().to_string()),
            VarFlags::NONE,
          )
        })
        .ok();

        // If we are here, we hit a case where pressing tab returned a single candidate
        // So we can just go ahead and reset the completer after this
        self.completer.reset();
      }
      Ok(None) => {
        let post_cmds = read_logic(|l| l.get_autocmds(AutoCmdKind::OnCompletionStart));
        let candidates = self.completer.all_candidates();
        let num_candidates = candidates.len();
        with_vars(
          [
            ("NUM_MATCHES".into(), Into::<Var>::into(num_candidates)),
            ("MATCHES".into(), Into::<Var>::into(candidates)),
            (
              "SEARCH_STR".into(),
              Into::<Var>::into(self.completer.token()),
            ),
          ],
          || {
            post_cmds.exec();
          },
        );

        if self.completer.is_active() {
          write_vars(|v| {
            v.set_var(
              "SHED_VI_MODE",
              VarKind::Str("COMPLETE".to_string()),
              VarFlags::NONE,
            )
          })
          .ok();
          self.prompt.refresh();
          self.needs_redraw = true;
          self.editor.clear_hint();
        } else {
          self.writer.send_bell().ok();
        }
      }
    }

    self.needs_redraw = true;
    Ok(None)
  }

  fn start_hist_search(&mut self) {
    let initial = self.focused_editor().joined();
    match self.focused_history().start_search(&initial) {
      Some(entry) => {
        let post_cmds = read_logic(|l| l.get_autocmds(AutoCmdKind::OnHistorySelect));
        with_vars([("_HIST_ENTRY".into(), entry.clone())], || {
          post_cmds.exec();
        });

        self.focused_editor().set_buffer(entry);
        self.focused_editor().move_cursor_to_end();
        self
          .history
          .update_pending_cmd((&self.editor.joined(), self.editor.cursor_to_flat()));
        self.editor.clear_hint();
      }
      None => {
        let post_cmds = read_logic(|l| l.get_autocmds(AutoCmdKind::OnHistoryOpen));
        let entries = self.focused_history().fuzzy_finder.candidates().to_vec();
        let matches = self
          .focused_history()
          .fuzzy_finder
          .filtered()
          .iter()
          .map(|sc| sc.candidate.content().to_string())
          .collect::<Vec<_>>();

        let num_entries = entries.len();
        let num_matches = matches.len();
        with_vars(
          [
            ("ENTRIES".into(), Into::<Var>::into(entries)),
            ("NUM_ENTRIES".into(), Into::<Var>::into(num_entries)),
            ("MATCHES".into(), Into::<Var>::into(matches)),
            ("NUM_MATCHES".into(), Into::<Var>::into(num_matches)),
            ("SEARCH_STR".into(), Into::<Var>::into(initial)),
          ],
          || {
            post_cmds.exec();
          },
        );

        if self.focused_history().fuzzy_finder.is_active() {
          write_vars(|v| {
            v.set_var(
              "SHED_VI_MODE",
              VarKind::Str("SEARCH".to_string()),
              VarFlags::NONE,
            )
          })
          .ok();
          self.prompt.refresh();
          self.needs_redraw = true;
          self.editor.clear_hint();
        } else {
          self.writer.send_bell().ok();
        }
      }
    }
  }

  fn submit(&mut self) -> ShResult<Option<ReadlineEvent>> {
    self.editor.clear_hint();
    self.editor.set_cursor_from_flat(self.editor.cursor_max());
    self.print_line(true)?;
    if let Some(layout) = &self.old_layout {
      log::debug!("Moving cursor to end of layout");
      self.writer.move_cursor_to_end(layout)?;
    }
    self.writer.flush_write("\n")?;
    let buf = self.editor.take_buf();
    self.focused_history().reset();
    Ok(Some(ReadlineEvent::Line(buf)))
  }

  pub fn handle_key(&mut self, key: KeyEvent) -> ShResult<Option<ReadlineEvent>> {
    if self.should_accept_hint(&key) {
      return self.accept_hint();
    }

    if let KeyEvent(KeyCode::Tab, _) = key {
      return self.handle_tab(key);
    } else if let KeyEvent(KeyCode::Char('R'), ModKeys::CTRL) = key
      && matches!(self.mode.report_mode(), ModeReport::Insert | ModeReport::Ex)
    {
      self.start_hist_search();
    }

    let Ok(cmd) = self.mode.handle_key_fallible(key) else {
      // it's an ex mode error
      self.swap_mode(&mut (Box::new(ViNormal::new()) as Box<dyn EditMode>));

      return Ok(None);
    };

    let Some(mut cmd) = cmd else {
      return Ok(None);
    };

    if let Some(VerbCmd(_, Verb::Normal(seq))) = cmd.verb() {
      let line_nums: Either<_, _> = match cmd.motion() {
        Some(MotionCmd(_, Motion::LineRange(s, e))) => {
          let s = self
            .editor
            .resolve_line_addr(s)?
            .unwrap_or(self.editor.row());
          let e = self
            .editor
            .resolve_line_addr(e)?
            .unwrap_or(self.editor.row());
          let (s, e) = ordered(s, e);
          Either::Left(s..=e)
        }
        Some(MotionCmd(_, Motion::Line(addr))) => {
          let addr = self
            .editor
            .resolve_line_addr(addr)?
            .unwrap_or(self.editor.row());
          Either::Left(addr..=addr)
        }
        Some(MotionCmd(_, m @ (Motion::Global(con, re) | Motion::NotGlobal(con, re)))) => {
          let polarity = matches!(m, Motion::Global(_, _));
          let lines = self.editor.get_matching_lines(con, re, polarity)?;
          Either::Right(lines.into_iter())
        }
        _ => {
          let row = self.editor.row();
          Either::Left(row..=row)
        }
      };

      let keys = expand_keymap(seq);

      self.editor.start_undo_merge();
      for line in line_nums {
        self.editor.set_cursor(linebuf::Pos { row: line, col: 0 });
        self.swap_mode(&mut (Box::new(ViNormal::new()) as Box<dyn EditMode>));

        for key in keys.clone() {
          if let Err(e) = self.handle_key(key) {
            self.editor.stop_undo_merge();
            return Err(e);
          }
        }
      }
      self.editor.stop_undo_merge();

      // just in case
      self.swap_mode(&mut (Box::new(ViNormal::new()) as Box<dyn EditMode>));

      return Ok(None);
    }

    if !cmd.is_virtual_scroll() {
      self.focused_history().stop_virtual_scroll();
      self.editor.clear_concats();
    }

    if self.should_grab_history(&cmd) {
      if read_shopts(|o| o.prompt.hist_cat)
        && cmd
          .flags
          .intersects(CmdFlags::HAS_SHIFT | CmdFlags::HAS_CTRL)
      {
        self.scroll_history_virtual(cmd);
      } else {
        self.scroll_history(cmd);
      }
      self.needs_redraw = true;
      return Ok(None);
    }

    if cmd.is_submit_action() {
      if self.editor.attempt_history_expansion(&self.history) {
        // If history expansion occurred, don't submit yet
        // allow the user to see the expanded command and accept or edit it before submitting
        return Ok(None);
      } else if self.should_submit()? || !read_shopts(|o| o.line.linebreak_on_incomplete) {
        return self.submit();
      }
    }

    if let Some(VerbCmd(_, v @ Verb::DeleteOrEof)) = cmd.verb_mut() {
      // user pressed Ctrl+D in emacs mode
      // we've gotta resolve this into either Delete or EndOfFile here
      if self.focused_editor().is_empty() {
        *v = Verb::EndOfFile;
      } else {
        *v = Verb::Delete;
      }
    } else if let Some(VerbCmd(_, Verb::ClearScreen)) = cmd.verb() {
      self.writer.clear_screen()?;
      self.needs_redraw = true;
      return Ok(None);
    }

    if (cmd.verb().is_some_and(|v| v.1 == Verb::EndOfFile)
      && self.focused_editor().joined().is_empty())
      || cmd.verb().is_some_and(|v| v.1 == Verb::Quit)
    {
      return Ok(Some(ReadlineEvent::Eof));
    }

    // check if it's an edit
    // we don't count Verb::Change since its possible for it to be called and not actually change anything
    // e.g. 'cc' on an empty line, 'C' at the end of a line, etc.
    // this is only used for ringing the bell
    let has_edit_verb = cmd
      .verb()
      .is_some_and(|v| v.1.is_edit() && v.1 != Verb::Change);

    let is_ctrl_d_motion = cmd.motion().is_some_and(|m| m.1 == Motion::HalfScreenDown);

    let is_ex_cmd = cmd.flags.contains(CmdFlags::IS_EX_CMD);
    if is_ex_cmd {
      self.ex_history.push(cmd.raw_seq.clone()).ok();
      self.ex_history.reset();
    }

    let before = self.editor.joined();
    let before_cursor = self.editor.cursor;

    self.exec_cmd(cmd, false)?;

    if let Some(keys) = write_meta(|m| m.take_pending_widget_keys()) {
      for key in keys {
        self.handle_key(key)?;
      }
    }
    let after = self.editor.joined();
    let after_cursor = self.editor.cursor;

    if before != after {
      self
        .history
        .constrain_entries(Some(&self.editor.joined()));
    } else if before == after && has_edit_verb {
      self.writer.send_bell().ok();
    } else if before_cursor == after_cursor && is_ctrl_d_motion {
      if self.ctrl_d_warning_counter == 3 || self.editor.is_empty() {
        // our silly user is spamming ctrl+d for some reason
        // maybe they want to exit the shell?
        write_meta(|m| {
          m.post_status_message(
            "Ctrl+D only quits in insert mode. try ':q' or entering insert mode with 'i'".into(),
          )
        });
        self.ctrl_d_warning_counter = 0;
      } else {
        self.ctrl_d_warning_counter += 1;
      }
    }

    let hint = self.focused_history().get_hint();

		self.editor.set_hint(hint);
    self.needs_redraw = true;
    Ok(None)
  }

  pub fn get_layout(&mut self, line: &str) -> Layout {
    let to_cursor = self.editor.window_slice_to_cursor().unwrap_or_default();
    let (cols, _) = get_win_size(self.tty);
    Layout::from_parts(cols, self.prompt.get_ps1(), &to_cursor, line)
  }
  pub fn scroll_history_virtual(&mut self, cmd: EditCmd) {
    // This function is used for the Shift/Ctrl+Up/Down history concatenation.
    // Instead of replacing the buffer with a scrolled-to history entry
    // This function appends it to the end of the current buffer with '&&' or ';'
    // depending on if the user is holding shift or ctrl.

    let MotionCmd(count, motion) = &cmd.motion.unwrap();
    let sep = if cmd.flags.contains(CmdFlags::HAS_SHIFT) {
      " && "
    } else {
      "; "
    };
    match motion {
      Motion::LineUp => {
        self
          .editor
          .edit(|e| match self.history.virtual_scroll_direction() {
            Some(Direction::Forward) => {
              for _ in 0..*count {
                if !e.pop_right() {
                  e.clear_buffer();
                  self.history.stop_virtual_scroll();
                  break;
                };
                self.history.virt_scroll(-1);
              }
            }
            None | Some(Direction::Backward) => {
              for _ in 0..*count {
                let Some(entry) = self.history.virt_scroll(-1) else {
                  continue;
                };
                let command = entry.command().to_string();
                e.concat_left(sep, &command);
                e.move_cursor_to_end();
              }
            }
          });
      }
      Motion::LineDown => {
        self
          .editor
          .edit(|e| match self.history.virtual_scroll_direction() {
            Some(Direction::Backward) => {
              for _ in 0..*count {
                if !e.pop_left() {
                  e.clear_buffer();
                  self.history.stop_virtual_scroll();
                  break;
                };
                self.history.virt_scroll(1);
              }
            }
            None | Some(Direction::Forward) => {
              for _ in 0..*count {
                let Some(entry) = self.history.virt_scroll(1) else {
                  continue;
                };
                let command = entry.command().to_string();
                e.concat_right(sep, &command);
                e.move_cursor_to_end();
              }
            }
          });
      }
      _ => unreachable!(),
    }
  }
  pub fn scroll_history_to(&mut self, hist_idx: usize) {
    let entry = self.focused_history().scroll_to(hist_idx).cloned();
    if entry.is_some() {
      write_meta(|m| {
        let total = self.focused_history().search_mask_count();
        m.post_status_message(format!("jumped to hist entry: {}/{}", hist_idx + 1, total));
      })
    }
    self.swap_history_editor(entry);
  }
  pub fn scroll_history(&mut self, cmd: EditCmd) {
    let count = if cmd.motion().is_some() {
			log::debug!("Motion for history scroll: {:?}", cmd.motion().unwrap().1);
      &cmd.motion().unwrap().0
    } else {
			log::debug!("Verb for history scroll: {:?}", cmd.verb().unwrap().1);
      match cmd.verb() {
        Some(VerbCmd(c, _)) => c,
        _ => unreachable!(),
      }
    };
    let motion = if cmd.motion().is_some() {
      cmd.motion().unwrap().1.clone()
    } else {
      match cmd.verb() {
        Some(VerbCmd(_, Verb::HistoryUp)) => Motion::LineUp,
        Some(VerbCmd(_, Verb::HistoryDown)) => Motion::LineDown,
        _ => unreachable!(),
      }
    };
    let count = match motion {
      Motion::LineUp => -(*count as isize),
      Motion::LineDown => *count as isize,
      _ => unreachable!(),
    };
    if self.focused_history().pending.is_none() {
			if count >= 0 {
				// if count >= 0, we are scrolling down
				// but if we are here, it means we are already at the pending command,
				// so return and bell
				self.writer.send_bell().ok();
				return;
			}
      // We are scrolling up from a pending command
      // Let's refresh the search mask to make sure
      // our history is up to date
      let joined = self.editor.joined();
      self.focused_history().update_search_mask(Some(&joined));
    }
    let entry = self.focused_history().scroll(count).cloned();
    self.swap_history_editor(entry);
  }
  pub fn swap_history_editor(&mut self, entry: Option<HistEntry>) {
    if let Some(entry) = entry {
      let editor = std::mem::take(self.focused_editor());
      self
        .focused_editor()
        .set_buffer(entry.command().to_string());
      if self.focused_history().pending.is_none() {
        self.focused_history().pending = Some(editor);
      }
      self.focused_editor().clear_hint();
      self.focused_editor().move_cursor_to_end();
    } else if let Some(pending) = self.focused_history().pending.take() {
      *self.focused_editor() = pending;
    } else {
      // If we are here it should mean we are on our pending command
      // And the user tried to scroll history down
      // Since there is no "future" history, we should just bell and do nothing
      self.writer.send_bell().ok();
      return;
    }
    let clamp = self.mode.clamp_cursor();
    self.focused_editor().set_cursor_clamp(clamp);
    self.focused_editor().fix_cursor();
  }
  pub fn should_accept_hint(&self, event: &KeyEvent) -> bool {
    if self.editor.cursor_at_max() && self.editor.has_hint() {
      match self.mode.report_mode() {
        ModeReport::Replace | ModeReport::Insert | ModeReport::Emacs => {
          matches!(event, KeyEvent(KeyCode::Right, ModKeys::NONE))
        }
        ModeReport::Visual | ModeReport::Normal => {
          matches!(event, KeyEvent(KeyCode::Right, ModKeys::NONE))
            || (self.mode.pending_seq().unwrap(/* always Some on normal mode */).is_empty()
              && matches!(event, KeyEvent(KeyCode::Char('l'), ModKeys::NONE)))
        }
        ModeReport::Ex | ModeReport::Verbatim | ModeReport::Unknown => false,
      }
    } else {
      false
    }
  }

  pub fn should_grab_history(&mut self, cmd: &EditCmd) -> bool {
    cmd.is_virtual_scroll()
      || cmd
        .verb()
        .is_some_and(|v| matches!(v, VerbCmd(_, Verb::HistoryUp | Verb::HistoryDown)))
      || cmd.verb().is_none()
        && (cmd
          .motion()
          .is_some_and(|m| matches!(m, MotionCmd(_, Motion::LineUp)))
          && self.editor.start_of_line() == 0)
      || (cmd
        .motion()
        .is_some_and(|m| matches!(m, MotionCmd(_, Motion::LineDown)))
        && self.editor.on_last_line())
        && !cmd.flags.contains(CmdFlags::IS_SUBMIT)
  }

  pub fn print_line(&mut self, final_draw: bool) -> ShResult<()> {
    let line = self.editor.display_window_joined();
    let mut new_layout = self.get_layout(&line);

    let pending_seq = self.mode.pending_seq();
    let mut prompt_string_right = self.prompt.psr_expanded.clone();

    if prompt_string_right
      .as_ref()
      .is_some_and(|psr| psr.lines().count() > 1)
    {
      log::warn!("PSR has multiple lines, truncating to one line");
      prompt_string_right =
        prompt_string_right.map(|psr| psr.lines().next().unwrap_or_default().to_string());
    }
    let mut buf = String::new();

    let row0_used = self.prompt
      .get_ps1()
      .lines()
      .next()
      .map(|l| Layout::calc_pos(self.writer.t_cols, l, Pos { col: 0, row: 0 }, 0, false))
      .map(|p| p.col)
      .unwrap_or_default() as usize;
    let one_line = new_layout.end.row == 0;

    self.completer.clear(&mut self.writer)?;
    {
      let mut writer = std::mem::take(&mut self.writer);
      self.focused_history().fuzzy_finder.clear(&mut writer)?;
      self.writer = writer;
    }

    if let Some(layout) = self.old_layout.as_ref() {
      self.writer.clear_rows(layout)?;
    }

    self.writer.redraw(
      self.prompt.get_ps1(),
      &line,
      &new_layout,
      self.editor.scroll_offset,
      self.editor.lines.len(),
    )?;

    let seq_fits = pending_seq
      .as_ref()
      .is_some_and(|seq| row0_used + 1 < (self.writer.t_cols as usize).saturating_sub(seq.width()));
    let psr_fits = prompt_string_right.as_ref().is_some_and(|psr| {
      new_layout.end.col as usize + 1 < (self.writer.t_cols as usize).saturating_sub(psr.width())
    });

    if !final_draw
      && let Some(seq) = pending_seq
      && !seq.is_empty()
      && !(prompt_string_right.is_some() && one_line)
      && seq_fits
      && self.mode.report_mode() != ModeReport::Ex
    {
      let to_col = self.writer.t_cols - calc_str_width(&seq);
      let up = new_layout.cursor.row; // rows to move up from cursor to top line of prompt

      let move_up = if up > 0 {
        format!("\x1b[{up}A")
      } else {
        String::new()
      };

      // Save cursor, move up to top row, move right to column, write sequence,
      // restore cursor
      write!(buf, "\x1b7{move_up}\x1b[{to_col}G{seq}\x1b8").unwrap();
    } else if !final_draw
      && let Some(psr) = prompt_string_right
      && psr_fits
    {
      let to_col = self.writer.t_cols - calc_str_width(&psr);
      let down = new_layout.end.row.saturating_sub(new_layout.cursor.row);
      let move_down = if down > 0 {
        format!("\x1b[{down}B")
      } else {
        String::new()
      };

      write!(buf, "\x1b7{move_down}\x1b[{to_col}G{psr}\x1b8").unwrap();

      // Record where the PSR ends so clear_rows can account for wrapping
      // if the terminal shrinks.
      let psr_start = Pos {
        row: new_layout.end.row,
        col: to_col,
      };
      new_layout.psr_end = Some(Layout::calc_pos(
        self.writer.t_cols,
        &psr,
        psr_start,
        0,
        false,
      ));
    }

    if let ModeReport::Ex = self.mode.report_mode() {
      let pending_seq = self.mode.pending_seq().unwrap_or_default();
      let down = new_layout.end.row - new_layout.cursor.row;
      let move_down = if down > 0 {
        format!("\x1b[{down}B")
      } else {
        String::new()
      };
      write!(buf, "{move_down}\x1b[1G\n: {pending_seq}").unwrap();
      new_layout.end.row += 1;
      new_layout.cursor.row = new_layout.end.row;
      new_layout.cursor.col = {
        let cursor_offset = self.mode.pending_cursor().unwrap_or(pending_seq.len());
        let before_cursor = pending_seq
          .graphemes(true)
          .take(cursor_offset)
          .collect::<String>();

        (2 + before_cursor.width()) as u16
      };

      write!(buf, "\x1b[{}G", new_layout.cursor.col + 1).unwrap();
    }

    write!(buf, "{}", &self.mode.cursor_style()).unwrap();

    self.writer.flush_write(&buf)?;

    // Move to end of layout for overlay draws (completer, history search)
    let has_overlays =
      self.completer.is_active() || self.focused_history().fuzzy_finder.is_active();

    let down = new_layout.end.row.saturating_sub(new_layout.cursor.row);
    if has_overlays && down > 0 {
      self.writer.flush_write(&format!("\x1b[{down}B"))?;
      new_layout.cursor.row = new_layout.end.row;
    }

    // Tell the completer the width of the prompt line above its \n so it can
    // account for wrapping when clearing after a resize.
    let preceding_width = if new_layout.psr_end.is_some() {
      self.writer.t_cols
    } else {
      // Without PSR, use the content width on the cursor's row
      (new_layout.end.col + 1).max(new_layout.cursor.col + 1)
    };

    let mut fuzzy_window_rows = 0;
    self
      .completer
      .set_prompt_line_context(preceding_width, new_layout.end.col);
    fuzzy_window_rows += self.completer.draw(&mut self.writer)?;

    {
      self
        .focused_history()
        .fuzzy_finder
        .set_prompt_line_context(preceding_width, new_layout.end.col);

      let mut writer = std::mem::take(&mut self.writer);
      fuzzy_window_rows += self.focused_history().fuzzy_finder.draw(&mut writer)?;
      self.writer = writer;
    }

    while let Some(msg) = write_meta(|m| m.pop_status_message()) {
      let now = Instant::now();
      self.status_msgs.push_back((msg, now));
    }

    while !final_draw && let Some((msg, time)) = self.status_msgs.front() {
      if time.elapsed().as_secs() < 5 {
        let down = new_layout.end.row - new_layout.cursor.row;
        let fuzzy_rows = fuzzy_window_rows.saturating_sub(1); // the cursor is one row below the top
        let total = down.saturating_add(fuzzy_rows as u16);
        let move_down = if total > 0 {
          format!("\x1b[{total}B")
        } else {
          String::new()
        };
        let move_up = total + 2;
        let col = new_layout.cursor.col + 1;
        self.writer.flush_write(&format!(
          "{move_down}\n\n\x1b7\x1b[2K{msg}\x1b8\x1b[{move_up}A\x1b[{col}G"
        ))?;
        new_layout.end.row += (2 + msg.chars().filter(|c| *c == '\n').count()) as u16;
        break;
      } else {
        self.status_msgs.pop_front();
      }
    }

    self.old_layout = Some(new_layout);
    self.needs_redraw = false;

    Ok(())
  }

  pub fn swap_mode(&mut self, mode: &mut Box<dyn EditMode>) {
    let pre_mode_change = read_logic(|l| l.get_autocmds(AutoCmdKind::PreModeChange));
    pre_mode_change.exec();

    std::mem::swap(&mut self.mode, mode);
    self.editor.set_cursor_clamp(self.mode.clamp_cursor());
    write_vars(|v| {
      v.set_var(
        "SHED_VI_MODE",
        VarKind::Str(self.mode.report_mode().to_string()),
        VarFlags::NONE,
      )
    })
    .ok();
    self.prompt.refresh();

    let post_mode_change = read_logic(|l| l.get_autocmds(AutoCmdKind::PostModeChange));
    post_mode_change.exec();
  }

  fn exec_mode_transition(&mut self, mut cmd: EditCmd, from_replay: bool) -> ShResult<()> {
    let mut is_insert_mode = false;
    let count = cmd.verb_count();

    let mut mode: Box<dyn EditMode> = if matches!(
      self.mode.report_mode(),
      ModeReport::Ex | ModeReport::Verbatim
    ) && cmd.flags.contains(CmdFlags::EXIT_CUR_MODE)
    {
      if self.mode.report_mode() == ModeReport::Ex
        && let Some(mode) = self.saved_mode.as_ref()
        && let ModeReport::Visual = mode.report_mode()
      {
        self.editor.stop_selecting();
        Box::new(ViNormal::new())
      } else if let Some(saved) = self.saved_mode.take() {
        saved
      } else {
        Box::new(ViNormal::new())
      }
    } else {
      match cmd.verb().unwrap().1 {
        Verb::Change | Verb::InsertModeLineBreak(_) | Verb::InsertMode => {
          is_insert_mode = true;
          Box::new(
            ViInsert::new()
              .with_count(count as u16)
              .record_cmd(cmd.clone()),
          )
        }

        Verb::ExMode => Box::new(ViEx::new(
          self.ex_history.clone(),
          self.editor.is_selecting(),
        )),

        Verb::VerbatimMode => {
          self.reader.verbatim_single = true;
          Box::new(ViVerbatim::new().with_count(count as u16))
        }

        Verb::NormalMode => Box::new(ViNormal::new()),

        Verb::ReplaceMode => Box::new(ViReplace::new()),

        Verb::VisualModeSelectLast => {
          if self.mode.report_mode() != ModeReport::Visual {
            self.editor.start_char_select();
          }
          let mut mode: Box<dyn EditMode> = Box::new(ViVisual::new());
          self.swap_mode(&mut mode);

          return self.fire_editor_command(cmd);
        }
        Verb::VisualMode => {
          self.editor.start_char_select();
          Box::new(ViVisual::new())
        }
        Verb::VisualModeLine => {
          self.editor.start_line_select();
          Box::new(ViVisual::new())
        }

        _ => unreachable!(),
      }
    };

    // The mode we just created swaps places with our current mode
    // After this line, 'mode' contains our previous mode.
    self.swap_mode(&mut mode);

		// check if we left insert/replace mode
		if matches!(
			mode.report_mode(), // 'mode' now contains the mode we just left
			ModeReport::Insert | ModeReport::Replace
		) {
			self.editor.stop_undo_merge();
		}

		// check if we entered ex/verbatim mode
    if matches!(
      self.mode.report_mode(),
      ModeReport::Ex | ModeReport::Verbatim
    ) {
      self.saved_mode = Some(mode);
      write_vars(|v| {
        v.set_var(
          "SHED_VI_MODE",
          VarKind::Str(self.mode.report_mode().to_string()),
          VarFlags::NONE,
        )
      })?;
      self.prompt.refresh();
      return Ok(());
    }

    if mode.is_repeatable() && !from_replay {
      self.repeat_action = mode.as_replay();
    }

    if let Some(range) = self.editor.select_range()
      && cmd.verb().is_some_and(|v| {
        !matches!(
          v.1,
          Verb::VisualMode | Verb::VisualModeLine | Verb::VisualModeBlock
        )
      })
    {
      cmd.motion = Some(motion!(range))
    }

    // Set cursor clamp BEFORE executing the command so that motions
    // (like EndOfLine for 'A') can reach positions valid in the new mode
    self.editor.set_cursor_clamp(self.mode.clamp_cursor());
    self.fire_editor_command(cmd)?;

    if mode.report_mode() == ModeReport::Visual && self.editor.select_range().is_some() {
      self.editor.stop_selecting();
    }

    if is_insert_mode {
      self.editor.mark_insert_mode_start_pos();
    } else {
      self.editor.clear_insert_mode_start_pos();
    }

    write_vars(|v| {
      v.set_var(
        "SHED_VI_MODE",
        VarKind::Str(self.mode.report_mode().to_string()),
        VarFlags::NONE,
      )
    })?;
    self.prompt.refresh();

    Ok(())
  }

  pub fn clone_mode(&self) -> Box<dyn EditMode> {
    match self.mode.report_mode() {
      ModeReport::Normal => Box::new(ViNormal::new()),
      ModeReport::Insert => Box::new(ViInsert::new()),
      ModeReport::Visual => Box::new(ViVisual::new()),
      ModeReport::Ex => Box::new(ViEx::new(
        self.ex_history.clone(),
        self.editor.is_selecting(),
      )),
      ModeReport::Replace => Box::new(ViReplace::new()),
      ModeReport::Verbatim => Box::new(ViVerbatim::new()),
      ModeReport::Emacs => Box::new(Emacs::new()),
      ModeReport::Unknown => unreachable!(),
    }
  }

  pub fn handle_cmd_repeat(&mut self, cmd: EditCmd) -> ShResult<()> {
    let Some(replay) = self.repeat_action.clone() else {
      return Ok(());
    };
    let EditCmd { verb, .. } = cmd;
    let VerbCmd(count, _) = verb.unwrap();
    match replay {
      CmdReplay::ModeReplay { cmds, mut repeat } => {
        if count > 1 {
          repeat = count as u16;
        }

        let old_mode = self.mode.report_mode();

        for _ in 0..repeat {
          let cmds = cmds.clone();
          for (i, cmd) in cmds.iter().enumerate() {
            self.exec_cmd(cmd.clone(), true)?;
            // After the first command, start merging so all subsequent
            // edits fold into one undo entry (e.g. cw + inserted chars)
            if i == 0
              && let Some(edit) = self.editor.undo_stack.last_mut()
            {
              edit.start_merge();
            }
          }
          // Stop merging at the end of the replay
          if let Some(edit) = self.editor.undo_stack.last_mut() {
            edit.stop_merge();
          }

          let old_mode_clone = match old_mode {
            ModeReport::Normal => Box::new(ViNormal::new()) as Box<dyn EditMode>,
            ModeReport::Insert => Box::new(ViInsert::new()) as Box<dyn EditMode>,
            ModeReport::Visual => Box::new(ViVisual::new()) as Box<dyn EditMode>,
            ModeReport::Ex => Box::new(ViEx::new(
              self.ex_history.clone(),
              self.editor.is_selecting(),
            )) as Box<dyn EditMode>,
            ModeReport::Replace => Box::new(ViReplace::new()) as Box<dyn EditMode>,
            ModeReport::Verbatim => Box::new(ViVerbatim::new()) as Box<dyn EditMode>,
            ModeReport::Emacs => Box::new(Emacs::new()) as Box<dyn EditMode>,
            ModeReport::Unknown => unreachable!(),
          };
          self.mode = old_mode_clone;
        }
      }
      CmdReplay::Single(mut cmd) => {
        if count > 1 {
          // Override the counts with the one passed to the '.' command
          if cmd.verb.is_some() {
            if let Some(v_mut) = cmd.verb.as_mut() {
              v_mut.0 = count
            }
            if let Some(m_mut) = cmd.motion.as_mut() {
              m_mut.0 = 1
            }
          } else {
            return Ok(()); // it has to have a verb to be repeatable,
            // something weird happened
          }
        }
        self.fire_editor_command(cmd)?;
      }
      _ => unreachable!("motions should be handled in the other branch"),
    }
    Ok(())
  }

  pub fn handle_motion_repeat(&mut self, cmd: EditCmd) -> ShResult<()> {
    match cmd.motion.as_ref().unwrap() {
      MotionCmd(count, Motion::RepeatMotion) => {
        let Some(motion) = self.repeat_motion.clone() else {
          return Ok(());
        };
        let repeat_cmd = EditCmd {
          register: RegisterName::default(),
          verb: cmd.verb,
          motion: Some(motion),
          raw_seq: format!("{count};"),
          flags: CmdFlags::empty(),
        };
        self.fire_editor_command(repeat_cmd)
      }
      MotionCmd(count, Motion::RepeatMotionRev) => {
        let Some(motion) = self.repeat_motion.clone() else {
          return Ok(());
        };
        let mut new_motion = motion.invert_char_motion();
        new_motion.0 = *count;
        let repeat_cmd = EditCmd {
          register: RegisterName::default(),
          verb: cmd.verb,
          motion: Some(new_motion),
          raw_seq: format!("{count},"),
          flags: CmdFlags::empty(),
        };
        self.fire_editor_command(repeat_cmd)
      }
      _ => unreachable!(),
    }
  }
  pub fn exec_cmd(&mut self, mut cmd: EditCmd, from_replay: bool) -> ShResult<()> {
    if cmd.verb().is_some()
      && let Some(range) = self.editor.select_range()
    {
      cmd.motion = Some(motion!(range))
    };

    if cmd.is_mode_transition() {
      self.exec_mode_transition(cmd, from_replay)
    } else if cmd.is_cmd_repeat() {
      self.handle_cmd_repeat(cmd)
    } else if cmd.is_motion_repeat() {
      self.handle_motion_repeat(cmd)
    } else {
      if self.mode.report_mode() == ModeReport::Visual && self.editor.select_range().is_none() {
        self.editor.stop_selecting();
        let mut mode: Box<dyn EditMode> = Box::new(ViNormal::new());
        self.swap_mode(&mut mode);
      }

      if cmd.is_repeatable() && !from_replay {
        if self.mode.report_mode() == ModeReport::Visual {
          // The motion is assigned in the line buffer execution, so we also have to
          // assign it here in order to be able to repeat it
          if let Some(range) = self.editor.select_range() {
            cmd.motion = Some(motion!(range))
          } else {
            log::warn!("You're in visual mode with no select range??");
          };
        }
        self.repeat_action = Some(CmdReplay::Single(cmd.clone()));
      }

      if cmd.is_char_search() {
        self.repeat_motion = cmd.motion.clone()
      }

      self.fire_editor_command(cmd.clone())?;

      if self.mode.report_mode() == ModeReport::Visual
        && cmd
          .verb()
          .is_some_and(|v| v.1.is_edit() || v.1 == Verb::Yank)
      {
        self.editor.stop_selecting();
        let mut mode: Box<dyn EditMode> = Box::new(ViNormal::new());
        self.swap_mode(&mut mode);
      }

      if self.mode.report_mode() != ModeReport::Visual && self.editor.select_range().is_some() {
        self.editor.stop_selecting();
      }

      if cmd.flags.contains(CmdFlags::EXIT_CUR_MODE) {
        let mut mode: Box<dyn EditMode> = if matches!(
          self.mode.report_mode(),
          ModeReport::Ex | ModeReport::Verbatim
        ) {
          if let Some(saved) = self.saved_mode.take() {
            saved
          } else {
            Box::new(ViNormal::new())
          }
        } else {
          Box::new(ViNormal::new())
        };
        self.swap_mode(&mut mode);
      }

      Ok(())
    }
  }

	pub fn fire_editor_command(&mut self, cmd: EditCmd) -> ShResult<()> {
		// just a direct wrapper for now, but might want to add some extra logic here later
		self.editor.exec_cmd(cmd)
	}
}

/// Annotates shell input with invisible Unicode markers for syntax highlighting
///
/// Takes raw shell input and inserts non-character markers (U+FDD0-U+FDEF
/// range) around syntax elements. These markers indicate:
/// - Token-level context (commands, arguments, operators, keywords)
/// - Sub-token constructs (strings, variables, command substitutions, globs)
///
/// The annotated string is suitable for processing by the highlighter, which
/// interprets the markers and generates ANSI escape codes.
///
/// # Strategy
/// Tokens are processed in reverse order so that later insertions don't
/// invalidate earlier positions. Each token is annotated independently.
///
/// # Example
/// ```text
/// "echo $USER"  ->  "COMMAND echo RESET ARG VAR_SUB $USER VAR_SUB_END RESET"
/// ```
/// (where COMMAND, RESET, etc. are invisible Unicode markers)
pub fn annotate_input(input: &str) -> String {
  let mut annotated = input.to_string();
  let input = input.into();
  let tokens: Vec<Tk> = lex::LexStream::new(Rc::clone(&input), LexFlags::LEX_UNFINISHED)
    .flatten()
    .filter(|tk| !matches!(tk.class, TkRule::SOI | TkRule::EOI | TkRule::Null))
    .collect();

  for tk in tokens.into_iter().rev() {
    let insertions = annotate_token(tk);
    for (pos, marker) in insertions {
      let pos = pos.max(0).min(annotated.len());
      annotated.insert(pos, marker);
    }
  }

  annotated
}

/// Recursively annotates nested constructs in the input string
pub fn annotate_input_recursive(input: &str) -> String {
  let mut annotated = annotate_input(input);
  let mut chars = annotated.char_indices().peekable();
  let mut changes = vec![];

  match_loop!(chars.next() => (pos, ch) => ch, {
    markers::CMD_SUB | markers::SUBSH | markers::PROC_SUB => {
      let mut body = String::new();
      let span_start = pos + ch.len_utf8();
      let mut span_end = span_start;
      let closing_marker = match ch {
        markers::CMD_SUB => markers::CMD_SUB_END,
        markers::SUBSH => markers::SUBSH_END,
        markers::PROC_SUB => markers::PROC_SUB_END,
        _ => unreachable!(),
      };
      match_loop!(chars.next() => (sub_pos, sub_ch) => sub_ch, {
        _ if sub_ch == closing_marker => {
          span_end = sub_pos;
          break;
        }
        _ => body.push(sub_ch),
      });
      let prefix = match ch {
        markers::PROC_SUB => match chars.peek().map(|(_, c)| *c) {
          Some('>') => ">(",
          Some('<') => "<(",
          _ => "<(",
        },
        markers::CMD_SUB => "$(",
        markers::SUBSH => "(",
        _ => unreachable!(),
      };

      body = body.trim_start_matches(prefix).to_string();
      let annotated_body = annotate_input_recursive(&body);
      let final_str = format!("{prefix}{annotated_body})");
      changes.push((span_start, span_end, final_str));
    }
    _ => {}
  });

  for change in changes.into_iter().rev() {
    let (start, end, replacement) = change;
    annotated.replace_range(start..end, &replacement);
  }

  annotated
}

pub fn get_insertions(input: &str) -> Vec<(usize, Marker)> {
  let input = input.into();
  let tokens: Vec<Tk> = lex::LexStream::new(Rc::clone(&input), LexFlags::LEX_UNFINISHED)
    .flatten()
    .collect();

  let mut insertions = vec![];
  for tk in tokens.into_iter().rev() {
    insertions.extend(annotate_token(tk));
  }
  insertions
}

/// Maps token class to its corresponding marker character
///
/// Returns the appropriate Unicode marker for token-level syntax elements.
/// Token-level markers are derived directly from the lexer's token
/// classification and represent complete tokens (operators, separators, etc.).
///
/// Returns `None` for:
/// - String tokens (which need sub-token scanning for variables, quotes, etc.)
/// - Structural markers (SOI, EOI, Null)
/// - Unimplemented features (comments, brace groups)
pub fn marker_for(class: &TkRule) -> Option<Marker> {
  match class {
    TkRule::Pipe
    | TkRule::Bang
    | TkRule::ErrPipe
    | TkRule::And
    | TkRule::Or
    | TkRule::Bg
    | TkRule::BraceGrpStart
    | TkRule::BraceGrpEnd => Some(markers::OPERATOR),
    TkRule::Sep => Some(markers::CMD_SEP),
    TkRule::Redir => Some(markers::REDIRECT),
    TkRule::Comment => Some(markers::COMMENT),
    TkRule::Expanded { exp: _ }
    | TkRule::EOI
    | TkRule::SOI
    | TkRule::Null
    | TkRule::Str
    | TkRule::CasePattern => None,
  }
}

#[allow(unused_assignments)]
pub fn annotate_token(token: Tk) -> Vec<(usize, Marker)> {
  // Sort by position descending, with priority ordering at same position:
  // - RESET first (inserted first, ends up rightmost)
  // - Regular markers middle
  // - END markers last (inserted last, ends up leftmost)
  // Result: [END][TOGGLE][RESET]
  let sort_insertions = |insertions: &mut Vec<(usize, Marker)>| {
    insertions.sort_by(|a, b| match b.0.cmp(&a.0) {
      std::cmp::Ordering::Equal => {
        let priority = |m: Marker| -> u8 {
          match m {
            markers::VISUAL_MODE_END | markers::VISUAL_MODE_START | markers::RESET => 0,
            markers::VAR_SUB
            | markers::VAR_SUB_END
            | markers::CMD_SUB
            | markers::CMD_SUB_END
            | markers::PROC_SUB
            | markers::PROC_SUB_END
            | markers::STRING_DQ
            | markers::STRING_DQ_END
            | markers::STRING_SQ
            | markers::STRING_SQ_END
            | markers::SUBSH_END => 2,
            markers::ARG => 3,
            _ => 1,
          }
        };
        priority(a.1).cmp(&priority(b.1))
      }
      other => other,
    });
  };

  let in_context = |c: Marker, insertions: &[(usize, Marker)]| -> bool {
    let mut stack = insertions.to_vec();
    stack.sort_by(|a, b| {
      match b.0.cmp(&a.0) {
        std::cmp::Ordering::Equal => {
          let priority = |m: Marker| -> u8 {
            match m {
              markers::VISUAL_MODE_END | markers::VISUAL_MODE_START | markers::RESET => 0,
              markers::VAR_SUB
              | markers::VAR_SUB_END
              | markers::CMD_SUB
              | markers::CMD_SUB_END
              | markers::PROC_SUB
              | markers::PROC_SUB_END
              | markers::STRING_DQ
              | markers::STRING_DQ_END
              | markers::STRING_SQ
              | markers::STRING_SQ_END
              | markers::SUBSH_END => 2,

              markers::ARG => 3, // Lowest priority - processed first, overridden by sub-tokens
              _ => 1,
            }
          };
          priority(a.1).cmp(&priority(b.1))
        }
        other => other,
      }
    });
    stack.retain(|(i, m)| *i <= token.span.range().start && !markers::END_MARKERS.contains(m));

    let Some(ctx) = stack.last() else {
      return false;
    };

    ctx.1 == c
  };

  let mut insertions: Vec<(usize, Marker)> = vec![];

  // Heredoc tokens have spans covering the body content far from the <<
  // operator, which breaks position tracking after marker insertions
  if token.flags.contains(TkFlags::IS_HEREDOC) {
    return insertions;
  }

  if token.class != TkRule::Str
    && let Some(marker) = marker_for(&token.class)
  {
    insertions.push((token.span.range().end, markers::RESET));
    insertions.push((token.span.range().start, marker));
    return insertions;
  } else if token.flags.contains(TkFlags::IS_SUBSH) {
    let token_raw = token.span.as_str();
    if token_raw.ends_with(')') {
      insertions.push((token.span.range().end, markers::SUBSH_END));
    }
    insertions.push((token.span.range().start, markers::SUBSH));
    return insertions;
  } else if token.class == TkRule::CasePattern {
    insertions.push((token.span.range().end, markers::RESET));
    insertions.push((token.span.range().end - 1, markers::CASE_PAT));
    insertions.push((token.span.range().start, markers::OPERATOR));
    return insertions;
  }

  let token_raw = token.span.as_str();
  let mut token_chars = token_raw.char_indices().peekable();

  let span_start = token.span.range().start;

  let mut qt_state = QuoteState::default();
  let mut in_backtick = false;
  let mut cmd_sub_depth = 0;
  let mut proc_sub_depth = 0;

  if token.flags.contains(TkFlags::BUILTIN) {
    insertions.insert(0, (span_start, markers::BUILTIN));
  } else if token.flags.contains(TkFlags::IS_CMD) {
    insertions.insert(0, (span_start, markers::COMMAND));
  } else if !token.flags.contains(TkFlags::KEYWORD) && !token.flags.contains(TkFlags::ASSIGN) {
    insertions.insert(0, (span_start, markers::ARG));
  }

  if token.flags.contains(TkFlags::KEYWORD) {
    insertions.insert(0, (span_start, markers::KEYWORD));
  }

  if token.flags.contains(TkFlags::ASSIGN) {
    insertions.insert(0, (span_start, markers::ASSIGNMENT));
  }

  insertions.insert(0, (token.span.range().end, markers::RESET)); // reset at the end of the token

  match_loop!(token_chars.peek() => (i, ch) => ch, {
  '`' if cmd_sub_depth == 0 => {
    let i = *i;
    in_backtick = !in_backtick;
    token_chars.next(); // consume the backtick
    if !in_backtick {
      insertions.push((span_start + i + 1, markers::BACKTICK_SUB_END));
    } else {
      insertions.push((span_start + i, markers::BACKTICK_SUB));
    }
  }
  ')' if cmd_sub_depth > 0 || proc_sub_depth > 0 => {
    let i = *i;
    token_chars.next(); // consume the paren
    if cmd_sub_depth > 0 {
      cmd_sub_depth -= 1;
      if cmd_sub_depth == 0 {
        insertions.push((span_start + i + 1, markers::CMD_SUB_END));
      }
    } else if proc_sub_depth > 0 {
      proc_sub_depth -= 1;
      if proc_sub_depth == 0 {
        insertions.push((span_start + i + 1, markers::PROC_SUB_END));
      }
    }
  }
  '$' if !qt_state.in_single() => {
    let dollar_pos = *i;
    token_chars.next(); // consume the dollar
    if let Some((_, dollar_ch)) = token_chars.peek() {
      match dollar_ch {
        '(' => {
          cmd_sub_depth += 1;
          if cmd_sub_depth == 1 {
            // only mark top level command subs
            insertions.push((span_start + dollar_pos, markers::CMD_SUB));
          }
          token_chars.next(); // consume the paren
        }
        '{' if cmd_sub_depth == 0 => {
          insertions.push((span_start + dollar_pos, markers::VAR_SUB));
          token_chars.next(); // consume the brace
          let mut end_pos; // position after ${
          match_loop!(token_chars.peek() => (cur_i, br_ch) => br_ch, {
            // TODO: implement better parameter expansion awareness here
            // this is a little too permissive
            _ if br_ch.is_ascii_alphanumeric() || "_!#%:-+=/?$".contains(*br_ch) => {
              end_pos = *cur_i + 1;
              token_chars.next();
            }
            '}' => {
              end_pos = *cur_i;
              token_chars.next(); // consume the closing brace
              insertions.push((span_start + end_pos + 1, markers::VAR_SUB_END));
              break;
            }
            _ => {
              end_pos = *cur_i;
              // malformed, insert end at current position
              insertions.push((span_start + end_pos, markers::VAR_SUB_END));
              break;
            }
          });
        }
        _ if cmd_sub_depth == 0 && (dollar_ch.is_ascii_alphanumeric() || *dollar_ch == '_') => {
          insertions.push((span_start + dollar_pos, markers::VAR_SUB));
          let mut end_pos = dollar_pos + 1;
          // consume the var name
          match_loop!(token_chars.peek() => (cur_i, var_ch) => var_ch, {
            _ if var_ch.is_ascii_alphanumeric() || *var_ch == '_' || ShellParam::from_char(var_ch).is_some() => {
              end_pos = *cur_i + 1;
              token_chars.next();
            }
            _ => break,
          });
          insertions.push((span_start + end_pos, markers::VAR_SUB_END));
        }
        _ => { /* Just a plain dollar sign, no marker needed */ }
        }
      }
    }
    _ if cmd_sub_depth > 0 || proc_sub_depth > 0 || in_backtick => {
      // We are inside of a command sub or process sub right now
      // We don't mark any of this text. It will later be recursively annotated
      // by the syntax highlighter
      token_chars.next(); // consume the char with no special handling
    }

    '\\' if !qt_state.in_single() => {
      token_chars.next(); // consume the backslash
      if token_chars.peek().is_some() {
        token_chars.next(); // consume the escaped char
      }
    }
    '\\' if qt_state.in_single() => {
      token_chars.next();
      if let Some(&(_, '\'')) = token_chars.peek() {
        token_chars.next(); // consume the escaped single quote
      }
    }
    '`' if !qt_state.in_single() => {
      token_chars.next();
    }
    '<' | '>' if !qt_state.in_quote() && cmd_sub_depth == 0 && proc_sub_depth == 0 => {
      let i = *i;
      token_chars.next();
      if let Some((_, proc_sub_ch)) = token_chars.peek()
        && *proc_sub_ch == '('
      {
        proc_sub_depth += 1;
        token_chars.next(); // consume the paren
        if proc_sub_depth == 1 {
          insertions.push((span_start + i, markers::PROC_SUB));
        }
      }
    }
    '"' if !qt_state.in_single() => {
      if qt_state.in_double() {
        insertions.push((span_start + *i + 1, markers::STRING_DQ_END));
      } else {
        insertions.push((span_start + *i, markers::STRING_DQ));
      }
      qt_state.toggle_double();
      token_chars.next(); // consume the quote
    }
    '\'' if !qt_state.in_double() => {
      if qt_state.in_single() {
        insertions.push((span_start + *i + 1, markers::STRING_SQ_END));
      } else {
        insertions.push((span_start + *i, markers::STRING_SQ));
      }
      qt_state.toggle_single();
      token_chars.next(); // consume the quote
    }
    '[' if !qt_state.in_quote() && !token.flags.contains(TkFlags::ASSIGN) => {
      let i = *i;
      token_chars.next(); // consume the opening bracket
      let start_pos = span_start + i;
      let mut is_glob_pat = false;
      const VALID_CHARS: &[char] = &['!', '^', '-'];

      match_loop!(token_chars.peek() => (cur_i, ch) => ch, {
        ']' => {
          is_glob_pat = true;
          insertions.push((span_start + *cur_i + 1, markers::RESET));
          insertions.push((span_start + *cur_i, markers::GLOB));
          token_chars.next(); // consume the closing bracket
        }
        _ if !ch.is_ascii_alphanumeric() && !VALID_CHARS.contains(ch) => {
          token_chars.next();
          break;
        }
        _ => {
          token_chars.next();
        }
      });
      if is_glob_pat {
        insertions.push((start_pos + 1, markers::RESET));
        insertions.push((start_pos, markers::GLOB));
      }
    }
    '*' | '?' if !qt_state.in_quote() && !token.flags.contains(TkFlags::ASSIGN) => {
      let i = *i;
      let glob_ch = *ch;
      token_chars.next(); // consume the first glob char
      if !in_context(markers::COMMAND, &insertions) {
        let offset = if glob_ch == '*' && token_chars.peek().is_some_and(|(_, c)| *c == '*') {
          // it's one of these probably: ./dir/**/*.txt
          token_chars.next(); // consume the second *
          2
        } else {
          // just a regular glob char
          1
        };
        insertions.push((span_start + i + offset, markers::RESET));
        insertions.push((span_start + i, markers::GLOB));
      }
    }
    '!' if !qt_state.in_single() && cmd_sub_depth == 0 && proc_sub_depth == 0 => {
      let bang_pos = *i;
      token_chars.next(); // consume the '!'
      if let Some((_, next_ch)) = token_chars.peek() {
        match next_ch {
          '!' | '$' => {
            // !! or !$
            token_chars.next();
            insertions.push((span_start + bang_pos, markers::HIST_EXP));
            insertions.push((span_start + bang_pos + 2, markers::HIST_EXP_END));
          }
          c if c.is_ascii_alphanumeric() || *c == '-' => {
            // !word, !-N, !N
            let mut end_pos = bang_pos + 1;
            match_loop!(token_chars.peek() => (cur_i, wch) => wch, {
              _ if wch.is_ascii_alphanumeric() || *wch == '-' || *wch == '_' => {
                end_pos = *cur_i + 1;
                token_chars.next();
              }
              _ => break,
            });
            insertions.push((span_start + bang_pos, markers::HIST_EXP));
            insertions.push((span_start + end_pos, markers::HIST_EXP_END));
          }
          _ => { /* lone ! before non-expansion char, ignore */ }
        }
      }
    }
    _ => {
      token_chars.next(); // consume the char with no special handling
    }
  });

  sort_insertions(&mut insertions);

  insertions
}
