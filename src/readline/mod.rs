use nix::poll::PollTimeout;
use std::{collections::VecDeque, io::Write, sync::mpsc, time::Instant};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

mod complete;
mod context;
mod core;
mod editcmd;
mod editmode;
mod highlight;
mod histimport;
mod history;
mod layout;
mod linebuf;
mod register;
pub(super) mod stash;

pub(crate) use self::core::EditorCore;

use complete::{
  CompResponse, Completer, FuzzyCompleter, FuzzySelector, GridCompleter, SelectorResponse,
  SimpleCompleter,
};
use editcmd::{Cmd, CmdFlags, EditCmd, Motion, Verb};
use editmode::{EditMode, Emacs, ViInsert, ViNormal};

use layout::{Layout, move_cursor_to_end, redraw};
use linebuf::LineBuf;
use register::{RegisterContent, RegisterName};

use crate::interactive::{LoopAction, run_prompt_command};
use crate::state::logic::AutoCmdKind;
use crate::state::terminal::{Scroll, TermCtl};
use crate::{exec_term, queue_term};

use super::state::meta::MetaTab;
use super::state::terminal::Terminal;
use super::{
  autocmd, builtin, eval,
  expand::{self, expand_keymap, expand_prompt},
  key, keys,
  keys::{KeyCode, KeyEvent, KeyMapFlags, KeyMapMatch, ModKeys},
  match_loop, motion, procio, sherr, shopt, socket,
  state::{
    self, Shed,
    shopt::CompleteStyle,
    terminal::{SyncOutputGuard, calc_str_width, truncate_with_ellipsis},
    util::with_vars,
    vars::{Var, VarFlags, VarKind},
  },
  status_msg, system_msg, try_var,
  util::{self, ShResult},
  var, verb, write_term,
};

pub(super) use complete::{
  BashCompSpec, Candidate, CompContext, CompFlags, CompMatch, CompOptFlags, CompOpts, CompSpec,
  ScoredCandidate,
};
pub(super) use editcmd::Direction;
pub(super) use editmode::ModeReport;
pub(super) use histimport::import_history;
pub(super) use history::{HistEntry, History};
pub(super) use linebuf::{Hint, Lines, Pos};

#[cfg(test)]
pub(super) use register::{restore_registers, save_registers};

#[cfg(test)]
pub mod tests;
pub(super) const DEFAULT_PS1: &str =
  "\\e[0m\\n\\e[1;0m\\u\\e[1;36m@\\e[1;31m\\h\\n\\e[1;36m\\W\\e[1;32m/\\n\\e[1;32m\\$\\e[0m ";

/// A simple line editor with optional history
///
/// Used for simpler text inputs like Ex mode and the help builtin's search bar
/// Do note that passing a table name to this struct will create a database table if it doesn't already exist.
#[derive(Default, Debug)]
pub(super) struct SimpleEditor {
  pub buf: LineBuf,
  pub mode: Emacs,
  pub history: Option<History>,
}

impl SimpleEditor {
  pub fn new(history_table: Option<&str>) -> Self {
    let history = history_table.map(|name| {
      state::util::get_db_conn()
        .and_then(|conn| History::new(conn, name).ok())
        .unwrap_or(History::empty(name))
    });
    Self {
      history,
      buf: LineBuf::default(),
      mode: Emacs::default(),
    }
  }
  fn should_grab_history(&mut self, cmd: &EditCmd) -> bool {
    cmd.verb().is_none()
      && (cmd
        .motion()
        .is_some_and(|m| matches!(m, Cmd(_, Motion::LineUp)))
        && self.buf.start_of_line() == 0)
      || (cmd
        .motion()
        .is_some_and(|m| matches!(m, Cmd(_, Motion::LineDown)))
        && self.buf.on_last_line())
  }
  fn scroll_history(&mut self, count: isize) {
    let Some(history) = self.history.as_mut() else {
      return;
    };
    let entry = history.scroll(count);
    if let Some(entry) = entry {
      let buf = std::mem::take(&mut self.buf);
      self.buf.set_buffer(entry.command());
      if history.pending.is_none() {
        history.pending = Some(buf);
      }
      self.buf.set_hint(None);
      self.buf.move_cursor_to_end();
    } else if let Some(pending) = history.pending.take() {
      self.buf = pending;
    }
  }
  pub fn handle_key(&mut self, key: KeyEvent) -> ShResult<()> {
    let Some(mut cmd) = self.mode.handle_key(key) else {
      return Ok(());
    };
    if self.should_grab_history(&cmd) {
      let count = match cmd.motion().unwrap() {
        Cmd(_, Motion::LineUp) => -1,
        Cmd(_, Motion::LineDown) => 1,
        _ => unreachable!(),
      };
      self.scroll_history(count);
      return Ok(());
    }
    if let Some(Cmd(_, Verb::DeleteOrEof)) = cmd.verb_mut() {
      // user pressed Ctrl+D in emacs mode
      // we've gotta resolve this into either Delete or EndOfFile here
      if self.buf.is_empty() {
        cmd.verb_mut().unwrap().1 = Verb::EndOfFile;
      } else {
        cmd.verb_mut().unwrap().1 = Verb::Delete;
      }
    }

    self.buf.exec_cmd(&cmd)
  }
}

/// Non-blocking readline result
#[derive(Debug)]
pub(super) enum ReadlineEvent {
  /// A complete line was entered
  Line(String),
  /// Ctrl+D on empty line - request to exit
  Eof,
  /// No complete input yet, need more bytes
  Pending,
}

pub(super) struct LineData {
  pub buffer: String,
  pub cursor: usize,
  pub anchor: Option<usize>,
  pub hint: Option<String>,
  pub mode: String,
}

pub(super) struct StatusLine {
  left: String,
  middle: String,
  right: String,
  dirty: bool,
}

impl StatusLine {
  pub fn new() -> Self {
    let (left_raw, middle_raw, right_raw) = Shed::shopts(|o| {
      let s = &o.statline;
      (
        s.left_string.clone(),
        s.middle_string.clone(),
        s.right_string.clone(),
      )
    });
    let saved_status = state::Shed::get_status();
    let left = expand_prompt(&left_raw).unwrap_or(left_raw.clone());
    let middle = expand_prompt(&middle_raw).unwrap_or(middle_raw.clone());
    let right = expand_prompt(&right_raw).unwrap_or(right_raw.clone());
    state::Shed::set_status(saved_status);

    Self {
      left,
      middle,
      right,
      dirty: false,
    }
  }
  pub fn parts(&mut self) -> (&str, &str, &str) {
    if self.dirty {
      self.refresh_now();
    }
    (&self.left, &self.middle, &self.right)
  }
  pub fn render(&mut self, term_width: usize) -> String {
    let (left, middle, right) = self.parts();

    let lw = calc_str_width(left);
    let mw = calc_str_width(middle);
    let rw = calc_str_width(right);

    let right_w = rw.min(term_width);
    let after_right = term_width.saturating_sub(right_w);

    let middle_w = mw.min(after_right);
    let after_middle = after_right.saturating_sub(middle_w);

    let left_w = lw.min(after_middle);
    let leftover = after_middle.saturating_sub(left_w);

    let middle_str = if middle_w < mw {
      truncate_with_ellipsis(middle, middle_w)
    } else {
      middle.to_string()
    };

    let left_str = if left_w < lw {
      truncate_with_ellipsis(left, left_w)
    } else {
      left.to_string()
    };

    let pad_lm = " ".repeat(leftover / 2);
    let pad_mr = " ".repeat(leftover - (leftover / 2));

    format!("{left_str}{pad_lm}{middle_str}{pad_mr}{right}")
  }
  pub fn refresh(&mut self) {
    self.dirty = true;
  }
  pub fn refresh_now(&mut self) {
    *self = Self::new();
  }
}

impl Default for StatusLine {
  fn default() -> Self {
    Self::new()
  }
}

pub(super) struct Prompt {
  ps1_expanded: String,
  psr_expanded: Option<String>,
  dirty: bool,
}

#[expect(clippy::similar_names)]
impl Prompt {
  pub fn new() -> Self {
    autocmd!(PrePrompt);

    let Some(ps1_raw) = try_var!("PS1") else {
      return Self::default();
    };
    // PS1 expansion may involve running commands (e.g., for \h or \W), which can modify shell state
    let saved_status = state::Shed::get_status();

    let Ok(ps1_expanded) = expand_prompt(&ps1_raw) else {
      return Self::default();
    };
    let psr_raw = try_var!("PSR");
    let psr_expanded = psr_raw
      .clone()
      .map(|r| expand_prompt(&r))
      .transpose()
      .ok()
      .flatten();

    // Restore shell state after prompt expansion, since it may have been modified by command substitutions in the prompt
    state::Shed::set_status(saved_status);

    autocmd!(PostPrompt);

    Self {
      ps1_expanded,
      psr_expanded,
      dirty: false,
    }
  }

  pub fn get_ps1(&mut self) -> &str {
    if self.dirty {
      self.refresh_now();
    }
    &self.ps1_expanded
  }
  fn refresh_now(&mut self) {
    let saved_status = state::Shed::get_status();
    *self = Self::new();
    state::Shed::set_status(saved_status);
    self.dirty = false;
  }

  pub fn refresh(&mut self) {
    self.dirty = true;
  }
}

impl Default for Prompt {
  fn default() -> Self {
    Self {
      ps1_expanded: expand_prompt(DEFAULT_PS1).unwrap_or_else(|_| DEFAULT_PS1.to_string()),
      psr_expanded: None,
      dirty: false,
    }
  }
}

enum LineCmd {
  Execute(EditCmd),
  SubmitLine(EditCmd),
  AppendHint,
  ScrollHist(isize),
  ScrollHistVirtual(EditCmd),
  EndOfFile,
  Quit,
  WriteQuit,
  ClearScreen,
  ResetWidget,
  NormalSeq(Vec<usize>, String),
  TriggerCompletion,
  TriggerHistSearch,
}

impl LineCmd {
  pub fn switch_to_normal() -> Self {
    Self::Execute(EditCmd {
      register: RegisterName::default(),
      verb: Some(verb!(Verb::NormalMode)),
      motion: None,
      raw_seq: String::new(),
      flags: CmdFlags::empty(),
    })
  }
}

#[derive(Default, Debug)]
enum MacroRecord {
  #[default]
  Idle,
  Recording(RegisterName, Vec<KeyEvent>),
}

impl MacroRecord {
  pub fn is_recording(&self) -> bool {
    matches!(self, MacroRecord::Recording(_, _))
  }
  pub fn feed_key_event(&mut self, event: KeyEvent) {
    if let MacroRecord::Recording(_, keys) = self {
      keys.push(event);
    }
  }
  pub fn commit_recording(&mut self) -> Option<RegisterName> {
    if let MacroRecord::Recording(reg, keys) = std::mem::take(self) {
      reg.write_to_register(register::RegisterContent::Macro(keys));

      Some(reg)
    } else {
      None
    }
  }
  pub fn start_recording(&mut self, reg: RegisterName) {
    *self = MacroRecord::Recording(reg, vec![]);
  }
  pub fn status(&self) -> Option<String> {
    match self {
      MacroRecord::Recording(reg, _) => {
        let name = reg.display()?;
        Some(format!("recording {name}"))
      }
      MacroRecord::Idle => None,
    }
  }
}

struct CompHintRequest {
  req_gen: u64,
  buffer: String,
  cursor_pos: usize,
}

struct HintWorker {
  channel: Option<mpsc::Sender<CompHintRequest>>,
  req_gen: u64,
  last_sent: Option<(String, usize)>,
}

impl HintWorker {
  pub fn new() -> Self {
    let (channel, receiver) = mpsc::channel::<CompHintRequest>();
    std::thread::spawn(move || Self::main(&receiver));
    Self {
      channel: Some(channel),
      req_gen: 0,
      last_sent: None,
    }
  }
  pub fn dispatch_worker(&mut self, buffer: String, cursor_pos: usize) {
    if self
      .last_sent
      .as_ref()
      .is_some_and(|(b, c)| b == &buffer && *c == cursor_pos)
    {
      return;
    }
    self.last_sent = Some((buffer.clone(), cursor_pos));
    self.req_gen = self.req_gen.wrapping_add(1);
    let req = CompHintRequest {
      req_gen: self.req_gen,
      buffer,
      cursor_pos,
    };
    if let Some(channel) = &self.channel {
      channel.send(req).ok();
    }
  }
  fn main(receiver: &mpsc::Receiver<CompHintRequest>) {
    let mut completer = SimpleCompleter::default();
    let token = &*socket::PRIVATE_TOKEN;
    while let Ok(mut req) = receiver.recv() {
      while let Ok(newer) = receiver.try_recv() {
        // drain until newest
        req = newer;
      }
      let CompHintRequest {
        req_gen,
        buffer,
        cursor_pos,
      } = req;
      completer.reset();
      let source = complete::CompSource::Shell;
      let outcome = completer
        .complete(buffer, cursor_pos, 1, source)
        .ok()
        .flatten();

      let Some(outcome) = outcome else { continue };

      match outcome {
        CompMatch::Exact { line }
        | CompMatch::CommonPrefix { line }
        | CompMatch::Cycled { line } => {
          let token_start = completer.token_span().0;
          let msg = format!("PRIVATE {token} set-comp-hint {req_gen} {token_start} {line}");
          socket::send_to_socket(&msg).ok();
        }
      }
    }
  }
}

pub(super) struct ShedLine {
  prompt: Prompt,
  statline: Option<StatusLine>,
  completer: Option<Box<dyn Completer>>,

  core: EditorCore,
  pending_keymap: Vec<KeyEvent>,
  repeat_macro: Option<RegisterName>,
  macro_record: MacroRecord,

  old_layout: Option<Layout>,
  blank_rows_above: u16,
  overlay_displacement: u16,
  history: History,
  ex_history: History,

  needs_redraw: bool,
  ctrl_d_warning_counter: usize,
  status_msgs: VecDeque<(String, Instant)>,

  worker: HintWorker,
}

impl ShedLine {
  pub fn new(prompt: Prompt) -> ShResult<Self> {
    Self::new_private(prompt, true)
  }

  pub fn new_no_hist(prompt: Prompt) -> ShResult<Self> {
    Self::new_private(prompt, false)
  }

  fn new_private(prompt: Prompt, with_hist: bool) -> ShResult<Self> {
    let statline = shopt!(statline.enable).then(StatusLine::new);

    let history = if with_hist {
      if let Some(conn) = state::util::get_db_conn() {
        History::new(conn, "shed_history")?
      } else {
        History::empty("shed_history")
      }
    } else {
      History::empty("shed_history")
    };
    let ex_history = if let Some(conn) = state::util::get_db_conn() {
      History::new(conn, "ex_history")?
    } else {
      History::empty("ex_history")
    };
    let mode = if shopt!(set.vi) {
      Box::new(ViInsert::new()) as Box<dyn EditMode>
    } else {
      Box::new(Emacs::new()) as Box<dyn EditMode>
    };
    let mut new = Self {
      prompt,
      statline,
      completer: None,
      core: EditorCore::new(mode),
      pending_keymap: Vec::new(),
      old_layout: None,
      blank_rows_above: 0,
      overlay_displacement: 0,
      repeat_macro: None,
      macro_record: MacroRecord::Idle,
      history,
      ex_history,
      needs_redraw: true,
      ctrl_d_warning_counter: 0,
      status_msgs: VecDeque::new(),
      worker: HintWorker::new(),
    };
    Shed::vars_mut(|v| {
      v.set_var(
        "SHED_EDIT_MODE",
        VarKind::string(new.core.mode.report_mode().to_string()),
        VarFlags::empty(),
      )
    })?;
    new.refresh_ui();
    queue_term!(TermCtl::PrintChar('\n')).ok();
    new.print_line(false)?;
    Ok(new)
  }

  pub fn get_line_data(&self) -> LineData {
    LineData {
      buffer: self.core.editor.to_string().replace('\n', "\\n"),
      cursor: self.core.editor.cursor_to_flat(),
      anchor: self.core.editor.anchor_to_flat(),
      hint: self
        .core
        .editor
        .try_join_hint()
        .map(|s| s.replace('\n', "\\n")),
      mode: self.core.mode.report_mode().to_string(),
    }
  }

  /// A mutable reference to the currently focused editor
  /// This includes the main `LineBuf`, and sub-editors for modes like Ex mode.
  /// A mutable reference to the currently focused history, if any.
  /// This includes the main history struct, and history for sub-editors like Ex mode.
  fn focused_history(&mut self) -> &mut History {
    self.core.mode.history().unwrap_or(&mut self.history)
  }

  fn history_fzf(&mut self) -> Option<&mut FuzzySelector> {
    self.focused_history().fuzzy_finder.as_mut()
  }

  fn refresh_prompt(&mut self) {
    self.prompt.refresh();
  }

  fn refresh_statline(&mut self) {
    if let Some(line) = self.statline.as_mut() {
      line.refresh();
    }
  }

  fn refresh_ui(&mut self) {
    self.refresh_prompt();
    self.refresh_statline();
  }

  /// Mark that the display needs to be redrawn (e.g., after SIGWINCH)
  pub fn mark_dirty(&mut self) {
    self.needs_redraw = true;
    self.refresh_ui();
  }

  pub fn reset_active_widget(&mut self, full_redraw: bool) -> ShResult<()> {
    if let Some(comp) = self.completer.as_mut() {
      comp.reset_stay_active();
      self.needs_redraw = true;
      Ok(())
    } else if let Some(finder) = self.history_fzf() {
      finder.reset_query();
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
    self.refresh_ui();
    self.core.editor = LineBuf::default();
    Shed::vars_mut(|v| {
      v.set_var("EDITOR_LINES", VarKind::Int(1), VarFlags::READONLY)?;
      v.set_var("EDITOR_LINE", VarKind::Int(1), VarFlags::READONLY)?;
      v.set_var(
        "EDITOR_FILE",
        VarKind::string(String::new()),
        VarFlags::READONLY,
      )
    })?;

    let mut mode = if shopt!(set.vi) {
      Box::new(ViInsert::new()) as Box<dyn EditMode>
    } else {
      Box::new(Emacs::new()) as Box<dyn EditMode>
    };
    self.core.swap_mode(&mut mode);
    self.needs_redraw = true;
    if full_redraw {
      self.old_layout = None;
    }
    if self.statline.is_none() && shopt!(statline.enable) {
      let new_bot = Shed::term_mut(|t| -> ShResult<u16> {
        let total_rows = t.t_rows() as u16;
        let new_bottom = total_rows.saturating_sub(2).max(1);
        let cursor_row = t
          .get_cursor_pos()
          .ok()
          .flatten()
          .map_or(new_bottom, |(r, _)| r.0 as u16);
        if cursor_row > new_bottom {
          let scroll_amount = (cursor_row - new_bottom) as usize;
          t.execute_control(&TermCtl::Scroll(Scroll::Up(scroll_amount as u16)))
            .ok();

          // scroll_up shifts content; the visual cursor row doesn't
          // change. Move it up so it tracks the prompt's new row.
          t.write_direct(&format!("\x1b[{scroll_amount}A")).ok();
        }
        Ok(new_bottom)
      })?;
      queue_term!(TermCtl::Scroll(Scroll::SetRegion(1, new_bot))).ok();
      self.old_layout = None;
      self.statline = Some(StatusLine::new());
    }
    self.focused_history().pending = None;
    self.focused_history().reset();

    self.print_line(false)
  }

  pub fn prompt_mut(&mut self) -> &mut Prompt {
    &mut self.prompt
  }

  pub fn curr_keymap_flags(&self) -> KeyMapFlags {
    let mut flags = KeyMapFlags::empty();
    match self.core.mode.report_mode() {
      ModeReport::Insert => flags |= KeyMapFlags::INSERT,
      ModeReport::Normal => flags |= KeyMapFlags::NORMAL,
      ModeReport::Ex => flags |= KeyMapFlags::EX,
      ModeReport::Visual => flags |= KeyMapFlags::VISUAL,
      ModeReport::Replace => flags |= KeyMapFlags::REPLACE,
      ModeReport::Verbatim => flags |= KeyMapFlags::VERBATIM,
      ModeReport::Emacs => flags |= KeyMapFlags::EMACS,
      ModeReport::Remote => flags |= KeyMapFlags::REMOTE,
      ModeReport::Search | ModeReport::RevSearch => {}
    }

    if self
      .core
      .mode
      .pending_seq()
      .is_some_and(|seq| !seq.is_empty())
    {
      flags |= KeyMapFlags::OP_PENDING;
    }

    flags
  }

  /// This method ensures that the editing mode (Vi or Emacs) matches the 'vi' option, and switches modes if necessary.
  pub fn fix_editing_mode(&mut self) {
    if shopt!(set.vi) && self.core.mode.report_mode() == ModeReport::Emacs {
      self
        .core
        .swap_mode(&mut (Box::new(ViInsert::new()) as Box<dyn EditMode>));
    } else if !shopt!(set.vi) && self.core.mode.report_mode() != ModeReport::Emacs {
      self
        .core
        .swap_mode(&mut (Box::new(Emacs::new()) as Box<dyn EditMode>));
    }
  }

  fn should_complete(&mut self) -> bool {
    !self.core.focused_editor().cursor_in_leading_ws()
  }

  fn should_submit(&mut self) -> bool {
    if self.core.mode.report_mode() == ModeReport::Normal {
      return true;
    }
    if self.core.editor.cursor_is_escaped()
      && matches!(
        self.core.mode.report_mode(),
        ModeReport::Emacs | ModeReport::Insert
      )
    {
      return false;
    }
    let (depth, failed) = self.core.editor.cursor_indent_level();
    depth == 0 && !failed
  }

  fn handle_hist_search_key(&mut self, key: KeyEvent) -> ShResult<()> {
    let finder = self.history_fzf().unwrap();
    match finder.handle_key(key)? {
      SelectorResponse::Accept(cmd) => {
        let entry_idx = cmd.id().unwrap(); // history entries having an id to unwrap is an invariant.
        self.scroll_history_to(entry_idx);
        if let Some(finder) = self.history_fzf() {
          finder.clear();
        }
        self.focused_history().stop_search();

        with_vars([("HIST_ENTRY".into(), cmd.content().to_string())], || {
          autocmd!(OnHistorySelect);
        });

        Shed::vars_mut(|v| {
          v.set_var(
            "SHED_EDIT_MODE",
            VarKind::string(self.core.mode.report_mode().to_string()),
            VarFlags::empty(),
          )
        })
        .ok();
        self.refresh_ui();
        self.needs_redraw = true;
      }
      SelectorResponse::Dismiss => {
        autocmd!(OnHistoryClose);

        self.core.editor.clear_hint();
        if let Some(finder) = self.history_fzf() {
          finder.clear();
        }
        self.focused_history().stop_search();
        Shed::vars_mut(|v| {
          v.set_var(
            "SHED_EDIT_MODE",
            VarKind::string(self.core.mode.report_mode().to_string()),
            VarFlags::empty(),
          )
        })
        .ok();
        self.refresh_ui();
        self.needs_redraw = true;
      }
      SelectorResponse::Consumed => {
        self.needs_redraw = true;
      }
    }
    Ok(())
  }

  fn handle_completion_key(&mut self, key: &KeyEvent) -> ShResult<bool> {
    let dismiss_completer = |this: &mut Self| -> ShResult<()> {
      autocmd!(OnCompletionCancel);

      this.update_editor_hint();
      if let Some(comp) = this.completer.as_mut() {
        comp.clear();
      }
      this.completer = None;
      Shed::vars_mut(|v| {
        v.set_var(
          "SHED_EDIT_MODE",
          VarKind::string(this.core.mode.report_mode().to_string()),
          VarFlags::empty(),
        )
      })
      .ok();
      this.refresh_ui();
      this.needs_redraw = true;
      Ok(())
    };

    let comp = self.completer.as_mut().unwrap();
    match comp.handle_key(key.clone())? {
      CompResponse::Accept(candidate) => {
        let comp = self.completer.as_ref().unwrap();
        let span_start = comp.token_span().0;
        let new_cursor = span_start + candidate.len();
        let line = comp.get_completed_line(&candidate);
        self.core.focused_editor().set_buffer(&line);
        self.core.focused_editor().set_cursor_from_flat(new_cursor);

        if !self.focused_history().at_pending() {
          self.focused_history().reset_to_pending();
        }
        self.update_editor_hint();
        // clear() needs old_layout to erase the selector, so clear before dropping
        if let Some(comp) = self.completer.as_mut() {
          comp.clear();
        }
        self.completer = None;
        self.needs_redraw = true;

        Shed::vars_mut(|v| {
          v.set_var(
            "SHED_EDIT_MODE",
            VarKind::string(self.core.mode.report_mode().to_string()),
            VarFlags::empty(),
          )
        })
        .ok();
        self.refresh_ui();

        with_vars(
          [("COMP_CANDIDATE".into(), candidate.content().to_string())],
          || autocmd!(OnCompletionSelect),
        );

        Ok(true)
      }
      CompResponse::Preview(candidate) => {
        // Splice the candidate into the buffer the same way Accept does,
        // but DON'T dismiss the completer. The user is still cycling.
        let comp = self.completer.as_ref().unwrap();
        let span_start = comp.token_span().0;
        let new_cursor = span_start + candidate.len();
        let line = comp.get_completed_line(&candidate);
        self.core.focused_editor().set_buffer(&line);
        self.core.focused_editor().set_cursor_from_flat(new_cursor);
        self.update_editor_hint();
        self.needs_redraw = true;
        Ok(true)
      }
      CompResponse::Consumed => {
        /* just redraw */
        self.needs_redraw = true;
        Ok(true)
      }
      CompResponse::Passthrough => Ok(false),
      CompResponse::Dismiss => {
        dismiss_completer(self)?;
        Ok(true)
      }
      CompResponse::DismissPassthrough => {
        dismiss_completer(self)?;
        Ok(false)
      }
    }
  }

  fn handle_keymap(&mut self, key: &KeyEvent) -> ShResult<Option<ReadlineEvent>> {
    let keymap_flags = self.curr_keymap_flags();
    self.pending_keymap.push(key.clone());

    let mut matches = Shed::logic(|l| l.keymaps_filtered(keymap_flags, &self.pending_keymap));
    let is_exact =
      matches.len() == 1 && matches[0].compare(&self.pending_keymap) == KeyMapMatch::IsExact;

    if matches.is_empty() {
      // No matches. Drain the buffered keys and execute them.
      for key in std::mem::take(&mut self.pending_keymap) {
        if let Some(event) = self.handle_key(&key)? {
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
        if let Some(event) = self.handle_key(&key)? {
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
  pub fn process_input(&mut self, keys: Vec<KeyEvent>) -> ShResult<ReadlineEvent> {
    // Redraw if needed
    if self.needs_redraw {
      self.print_line(false)?;
      self.needs_redraw = false;
    }

    // Process all available keys
    for key in keys {
      if self.macro_record.is_recording() {
        if let KeyEvent(KeyCode::Char('q'), ModKeys::NONE) = key {
          self.repeat_macro = self.macro_record.commit_recording();
          self.mark_dirty();
          continue;
        }
        self.macro_record.feed_key_event(key.clone());
      }
      if let Some(ev) = self.dispatch_key(key)? {
        return Ok(ev);
      }
    }
    if self.completer.is_none() && self.history_fzf().is_none() {
      Shed::vars_mut(|v| {
        v.set_var(
          "SHED_EDIT_MODE",
          VarKind::string(self.core.mode.report_mode().to_string()),
          VarFlags::empty(),
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
    Shed::notify_line_edit(line_data);

    self.try_comp_hint();

    Ok(ReadlineEvent::Pending)
  }

  fn try_comp_hint(&mut self) {
    if !self.core.editor.cursor_at_max() {
      return;
    }

    let buf = self.core.editor.to_string();
    let cursor_pos = self.core.editor.cursor_to_flat();
    if !buf.is_empty() {
      self.worker.dispatch_worker(buf, cursor_pos);
    }
  }

  pub fn worker_req_gen(&mut self) -> u64 {
    self.worker.req_gen
  }

  fn dispatch_key(&mut self, key: KeyEvent) -> ShResult<Option<ReadlineEvent>> {
    if self.history_fzf().is_some() {
      self.handle_hist_search_key(key)?;
      Ok(None)
    } else if self.completer.is_some() && self.handle_completion_key(&key)? {
      // self.handle_completion_key() returns true if we need to continue the loop
      Ok(None)
    } else if self
      .core
      .mode
      .pending_seq()
      .is_some_and(|seq| !seq.is_empty())
      || self.core.mode.is_input_mode()
    {
      // Vi mode is waiting for more input (e.g. after 'f', 'd', etc.)
      // Bypass keymap matching and send directly to the mode handler
      let ev = self.handle_key(&key)?;
      self.core.update_editor_search();
      self
        .core
        .editor
        .set_cursor_clamp(self.core.mode.clamp_cursor());

      Ok(ev)
    } else {
      self.handle_keymap(&key)
    }
  }

  /// Replay a sequence of `KeyEvent`s as if they came from the input stream.
  pub fn replay_keys(
    &mut self,
    keys: Vec<KeyEvent>,
    with_keymaps: bool,
  ) -> ShResult<Option<ReadlineEvent>> {
    for key in keys {
      let ev = if with_keymaps {
        self.dispatch_key(key)?
      } else {
        self.handle_key(&key)?
      };
      if let Some(ev) = ev {
        return Ok(Some(ev));
      }
      // Abort the replay if a search-style motion found no target, matching
      // vim's behavior of cancelling macro playback on a failed `f`/`/`.
      if self.core.editor.search_failed() {
        break;
      }
    }
    Ok(None)
  }

  fn accept_hint(&mut self) -> Option<ReadlineEvent> {
    self.core.editor.edit(|e| {
      e.accept_hint();
    });
    if !self.focused_history().at_pending() {
      self.focused_history().reset_to_pending();
    }
    self.history.update_pending_cmd((
      &self.core.editor.to_string(),
      self.core.editor.cursor_to_flat(),
    ));
    self.needs_redraw = true;

    None
  }

  fn handle_tab(&mut self, key: &KeyEvent) -> Option<ReadlineEvent> {
    let KeyEvent(KeyCode::Tab, mod_keys) = key else {
      return None;
    };

    if self.core.mode.report_mode() != ModeReport::Ex
      && self
        .core
        .editor
        .edit(|e| e.attempt_inline_expansion(&self.history))
    {
      // If history expansion occurred, don't attempt completion yet
      self.update_editor_hint();
      return None;
    }

    let direction = match *mod_keys {
      ModKeys::SHIFT => -1,
      _ => 1,
    };
    let line = self.core.focused_editor().to_string();
    let cursor_pos = self.core.focused_editor().cursor_byte_pos();

    let mut comp = self
      .completer
      .take()
      .unwrap_or_else(|| match shopt!(prompt.complete_style) {
        CompleteStyle::Grid => Box::new(GridCompleter::new()),
        CompleteStyle::Fuzzy => Box::new(FuzzyCompleter::default()),
      });
    let source = if self.core.mode.report_mode() == ModeReport::Ex {
      complete::CompSource::ExMode
    } else {
      complete::CompSource::Shell
    };
    match comp.complete(line, cursor_pos, direction, source) {
      Err(e) => {
        e.print_error();
        // Printing the error invalidates the layout
        self.old_layout = None;
      }
      Ok(Some(comp_match)) => {
        let line = comp_match.into_line();
        let cand = comp.selected_candidate().unwrap_or_default();
        with_vars(
          [("COMP_CANDIDATE".into(), cand.content().to_string())],
          || autocmd!(OnCompletionSelect),
        );

        let span_start = comp.token_span().0;

        let new_cursor = span_start
          + comp
            .selected_candidate()
            .map(|c| c.len())
            .unwrap_or_default();

        self.core.focused_editor().set_buffer(&line);
        self.core.focused_editor().set_cursor_from_flat(new_cursor);

        if !self.focused_history().at_pending() {
          self.focused_history().reset_to_pending();
        }
        self.update_editor_hint();
        Shed::vars_mut(|v| {
          v.set_var(
            "SHED_EDIT_MODE",
            VarKind::string(self.core.mode.report_mode().to_string()),
            VarFlags::empty(),
          )
        })
        .ok();

        // Single candidate, don't store the completer
      }
      Ok(None) => {
        let candidates = comp.all_candidates();
        let num_candidates = candidates.len();

        let cand_assoc: VarKind = candidates
          .into_iter()
          .fold(vec![], |mut acc, cand| {
            let desc = cand.desc().map(|d| d.to_string()).unwrap_or_default();
            let name = cand.content().to_string();
            acc.push((name, desc));
            acc
          })
          .into();
        Shed::vars_mut(|v| v.set_var("MATCHES", cand_assoc, VarFlags::LOCAL).unwrap());
        Shed::vars_mut(|v| {
          v.set_var(
            "NUM_MATCHES",
            VarKind::Int(num_candidates as i32),
            VarFlags::LOCAL,
          )
        })
        .unwrap();
        Shed::vars_mut(|v| v.set_var("SEARCH_STR", VarKind::string(comp.token()), VarFlags::LOCAL))
          .unwrap();

        let cmds = Shed::logic(|l| l.get_autocmds(AutoCmdKind::OnCompletionStart));
        Shed::notify_autocmd(AutoCmdKind::OnCompletionStart);
        let saved_status = Shed::get_status();
        let mut res = LoopAction::Continue;

        for cmd in cmds {
          if let LoopAction::Break =
            run_prompt_command(cmd.command().to_string(), false, None).ok()?
          {
            res = LoopAction::Break;
            break;
          }
        }

        scopeguard::defer! {
          Shed::vars_mut(|v| {
            v.unset_var("MATCHES").ok();
            v.unset_var("NUM_MATCHES").ok();
            v.unset_var("SEARCH_STR").ok();
          })
        }

        let cancelled = res == LoopAction::Break || Shed::get_status() != 0;
        Shed::set_status(saved_status);
        if cancelled {
          autocmd!(OnCompletionCancel)
        } else if comp.is_active() {
          let VarKind::AssocArr(filtered) = Shed::vars_mut(|v| v.try_take_var_kind("MATCHES"))?
          else {
            system_msg!("completion error: MATCHES variable must be an associative array");

            return None;
          };

          let candidates: Vec<Candidate> =
            filtered.into_iter().fold(vec![], |mut acc, (name, desc)| {
              let cand: Candidate = Candidate::from(name).with_desc(desc.to_string());
              acc.push(cand);
              acc
            });

          match candidates.len() {
            1 => {
              let cand = &candidates[0];
              let line = comp.get_completed_line(cand.content());
              with_vars(
                [("COMP_CANDIDATE".into(), cand.content().to_string())],
                || autocmd!(OnCompletionSelect),
              );

              let span_start = comp.token_span().0;
              let new_cursor = span_start + cand.len();

              self.core.focused_editor().set_buffer(&line);
              self.core.focused_editor().set_cursor_from_flat(new_cursor);

              if !self.focused_history().at_pending() {
                self.focused_history().reset_to_pending();
              }
              self.update_editor_hint();
              Shed::vars_mut(|v| {
                v.set_var(
                  "SHED_EDIT_MODE",
                  VarKind::string(self.core.mode.report_mode().to_string()),
                  VarFlags::empty(),
                )
              })
              .ok();
            }
            _ => {
              self.completer = Some(comp);
              Shed::vars_mut(|v| {
                v.set_var(
                  "SHED_EDIT_MODE",
                  VarKind::string("COMPLETE"),
                  VarFlags::empty(),
                )
              })
              .ok();
              self.refresh_ui();
              self.needs_redraw = true;
              self.core.editor.clear_hint();
            }
          }
        } else {
          Shed::term_mut(Terminal::send_bell).ok();
        }
      }
    }

    self.needs_redraw = true;
    None
  }

  fn start_hist_search(&mut self) {
    let initial = self.core.focused_editor().to_string();
    if let Some(entry) = self.focused_history().start_search(&initial) {
      with_vars([("HIST_ENTRY".into(), entry.clone())], || {
        autocmd!(OnHistorySelect);
      });

      self.core.focused_editor().set_buffer(&entry);
      self.core.focused_editor().move_cursor_to_end();
      self.history.update_pending_cmd((
        &self.core.editor.to_string(),
        self.core.editor.cursor_to_flat(),
      ));
      self.core.editor.clear_hint();
    } else {
      let finder = self.history_fzf().unwrap();
      let entries = finder.candidates().to_vec();
      let matches = finder
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
        || autocmd!(OnHistoryOpen),
      );

      if self.history_fzf().is_some() {
        Shed::vars_mut(|v| {
          v.set_var(
            "SHED_EDIT_MODE",
            VarKind::string("SEARCH"),
            VarFlags::empty(),
          )
        })
        .ok();
        self.refresh_ui();
        self.needs_redraw = true;
        self.core.editor.clear_hint();
      } else {
        Shed::term_mut(Terminal::send_bell).ok();
      }
    }
  }

  pub(crate) fn in_insert_mode(&self) -> bool {
    matches!(self.core.mode.report_mode(), ModeReport::Insert)
  }

  fn extract_line_nums(&self, cmd: &EditCmd) -> ShResult<Vec<usize>> {
    if let Some(Cmd(_, Verb::ExCmd(node))) = cmd.verb() {
      return self.core.editor.lines_for_ex_node(node);
    }
    Ok(vec![self.core.editor.row()])
  }

  fn submit(&mut self) -> ShResult<Option<ReadlineEvent>> {
    self.core.editor.clear_hint();
    self
      .core
      .editor
      .set_cursor_from_flat(self.core.editor.cursor_max());
    self.print_line(true)?;
    if let Some(layout) = &self.old_layout {
      move_cursor_to_end(layout);
    }
    if shopt!(line.trim_on_submit) {
      self.core.editor.trim();
    }

    queue_term!(TermCtl::PrintChar('\r')).ok();
    queue_term!(TermCtl::PrintChar('\n')).ok();

    // Command output fills the region from below the prompt; tracked
    // blank rows above will scroll into scrollback as it does, and any
    // overlay displacement is moot once the prompt is gone.
    self.blank_rows_above = 0;
    self.overlay_displacement = 0;
    let buf = self.core.editor.take_buf();
    self.focused_history().reset();
    Ok(Some(ReadlineEvent::Line(buf)))
  }

  fn resolve_key(&mut self, key: &KeyEvent) -> ShResult<Option<LineCmd>> {
    if self.should_accept_hint(key) {
      return Ok(Some(LineCmd::AppendHint));
    } else if let KeyEvent(KeyCode::Tab, _) = key
      && self.should_complete()
    {
      return Ok(Some(LineCmd::TriggerCompletion));
    } else if let key!(Ctrl + 'r') = key
      && matches!(
        self.core.mode.report_mode(),
        ModeReport::Emacs | ModeReport::Insert | ModeReport::Ex
      )
    {
      return Ok(Some(LineCmd::TriggerHistSearch));
    }

    let Ok(cmd) = self.core.mode.handle_key_fallible(key.clone()) else {
      // it's an ex mode error
      return Ok(Some(LineCmd::switch_to_normal()));
    };

    let Some(cmd) = cmd else { return Ok(None) };

    self.resolve_cmd(cmd)
  }

  fn resolve_cmd(&mut self, mut cmd: EditCmd) -> ShResult<Option<LineCmd>> {
    if let Some(Cmd(_, Verb::Interrupt)) = cmd.verb() {
      return Ok(Some(LineCmd::ResetWidget));
    }

    if let Some(seq) = cmd.try_get_normal_seq() {
      let line_nums = self.extract_line_nums(&cmd)?;
      return Ok(Some(LineCmd::NormalSeq(line_nums, seq.to_string())));
    }

    if self.should_grab_history(&cmd) {
      let offset = cmd.history_scroll_offset().unwrap();

      if shopt!(prompt.hist_cat)
        && cmd
          .flags
          .intersects(CmdFlags::HAS_SHIFT | CmdFlags::HAS_CTRL)
      {
        return Ok(Some(LineCmd::ScrollHistVirtual(cmd)));
      }
      return Ok(Some(LineCmd::ScrollHist(offset)));
    }

    if cmd.is_submit_action() {
      return Ok(Some(LineCmd::SubmitLine(cmd)));
    }

    if let Some(Cmd(_, Verb::DeleteOrEof)) = cmd.verb_mut() {
      // user pressed Ctrl+D in emacs mode
      // we've gotta resolve this into either Delete or EndOfFile here
      if self.core.focused_editor().is_empty() {
        return Ok(Some(LineCmd::EndOfFile));
      }
      cmd.verb_mut().unwrap().1 = Verb::Delete;
      return Ok(Some(LineCmd::Execute(cmd)));
    } else if let Some(Cmd(_, Verb::ClearScreen)) = cmd.verb() {
      return Ok(Some(LineCmd::ClearScreen));
    }

    if cmd.verb_is(&Verb::EndOfFile) && self.core.focused_editor().is_empty() {
      return Ok(Some(LineCmd::EndOfFile));
    } else if cmd.is_quit() {
      if self.core.editor.open_file().is_some() {
        return Ok(Some(LineCmd::ResetWidget));
      }
      return Ok(Some(LineCmd::Quit));
    } else if cmd.is_write_quit() {
      if self.core.editor.open_file().is_some() {
        return Ok(Some(LineCmd::WriteQuit));
      }
      return Ok(Some(LineCmd::Quit));
    } else if cmd.verb_is(&Verb::AcceptHint) {
      return Ok(Some(LineCmd::AppendHint));
    }

    Ok(Some(LineCmd::Execute(cmd)))
  }

  fn run_cmd(&mut self, cmd: EditCmd) -> ShResult<Option<ReadlineEvent>> {
    // check if it's an edit
    // we don't count Verb::Change since its possible for it to be called and not actually change anything
    // e.g. 'cc' on an empty line, 'C' at the end of a line, etc.
    // this is only used for ringing the bell
    let has_edit_verb = cmd
      .verb()
      .is_some_and(|v| v.1.is_edit() && v.1 != Verb::Change);

    let is_ctrl_d_motion = cmd.motion_is(&Motion::HalfScreenDown);

    let is_ex_cmd = cmd.flags.contains(CmdFlags::IS_EX_CMD);
    if is_ex_cmd {
      self.ex_history.push(&cmd.raw_seq).ok();
      self.ex_history.reset();
    }

    if cmd.verb_is(&Verb::RecordMacro) {
      log::debug!("starting macro recording with cmd: {cmd:?}");
      if cmd.register.name().is_none() {
        return Ok(None);
      }
      cmd.register.write_to_register(RegisterContent::Empty);

      self.macro_record.start_recording(cmd.register);
      self.mark_dirty();
      return Ok(None);
    }

    if cmd.verb_is(&Verb::PlayMacro) {
      let target = if cmd.register.name().is_some() {
        cmd.register
      } else if let Some(reg) = self.repeat_macro {
        reg
      } else {
        return Ok(None);
      };

      let events = match target.read_from_register() {
        None => return Ok(None),
        Some(content) => match content {
          RegisterContent::Empty => return Ok(None),
          RegisterContent::Span(s) | RegisterContent::Line(s) | RegisterContent::Block(s) => {
            let joined = Lines::from(s).join();
            expand_keymap(&joined)
          }
          RegisterContent::Macro(keys) => keys,
        },
      };

      self.core.editor.start_undo_merge();
      if let Ok(Some(event)) = self.replay_keys(events, false) {
        self.core.editor.stop_undo_merge();
        return Ok(Some(event));
      }
      self.core.editor.stop_undo_merge();
      return Ok(None);
    }

    let before = self.core.editor.to_string();
    let before_cursor = self.core.editor.cursor();

    self.core.exec_cmd(cmd, false)?;

    if let Some(keys) = Shed::meta_mut(MetaTab::take_pending_widget_keys) {
      self.replay_keys(keys, false)?;
    }
    let after = self.core.editor.to_string();
    let after_cursor = self.core.editor.cursor();

    if before != after {
      self.history.mark_mask_stale();
    } else if before == after && has_edit_verb {
      Shed::term_mut(Terminal::send_bell).ok();
    } else if before_cursor == after_cursor && is_ctrl_d_motion {
      if self.ctrl_d_warning_counter == 3 || self.core.editor.is_empty() {
        // our silly user is spamming ctrl+d for some reason
        // maybe they want to exit the shell?
        status_msg!("Ctrl+D only quits in insert mode. try ':q' or entering insert mode with 'i'");
        self.ctrl_d_warning_counter = 0;
      } else {
        self.ctrl_d_warning_counter += 1;
      }
    }

    // Drain the UI signals the core raised during execution. These used to be
    // refreshed inline by fire_editor_command/swap_mode before the core split.
    let shell_cmd_ran = std::mem::take(&mut self.core.shell_cmd_ran);
    let mode_changed = std::mem::take(&mut self.core.mode_changed);
    self.core.needs_redraw = false;
    self.refresh_statline();
    if shell_cmd_ran || mode_changed {
      self.refresh_prompt();
    }

    self.update_editor_hint();
    self.needs_redraw = true;
    Ok(None)
  }

  pub fn handle_key(&mut self, key: &KeyEvent) -> ShResult<Option<ReadlineEvent>> {
    let Some(linecmd) = self.resolve_key(key)? else {
      self.core.update_editor_search();
      self.needs_redraw = true;
      return Ok(None);
    };
    if !matches!(&linecmd, LineCmd::ScrollHistVirtual(_)) {
      self.focused_history().stop_virtual_scroll();
      self.core.editor.clear_concats();
    }

    match linecmd {
      LineCmd::Execute(cmd) => self.run_cmd(cmd),
      LineCmd::ScrollHist(off) => {
        self.scroll_history(off);
        self.needs_redraw = true;
        Ok(None)
      }
      LineCmd::ScrollHistVirtual(cmd) => {
        self.scroll_history_virtual(cmd);
        self.needs_redraw = true;
        Ok(None)
      }
      LineCmd::EndOfFile => {
        if self.core.focused_editor().to_string().is_empty() {
          Ok(Some(ReadlineEvent::Eof))
        } else {
          self.reset_active_widget(false)?;
          Ok(None)
        }
      }
      LineCmd::WriteQuit => {
        let write_cmd = EditCmd::plain_write();
        self.run_cmd(write_cmd)?;

        self.reset_active_widget(false)?;
        Ok(None)
      }
      LineCmd::Quit => Ok(Some(ReadlineEvent::Eof)),
      LineCmd::ClearScreen => {
        let cursor_row = Shed::term_mut(Terminal::get_cursor_pos)
          .ok()
          .flatten()
          .map_or(1, |(r, _)| r.0);

        let prompt_cursor_offset = self.old_layout.as_ref().map_or(0, |l| l.cursor.row);

        let prompt_top = cursor_row.saturating_sub(prompt_cursor_offset);
        let scroll_amount = prompt_top.saturating_sub(1);

        if scroll_amount > 0 {
          queue_term!(TermCtl::Scroll(Scroll::Up(scroll_amount as u16))).ok();
          // Move cursor up to track the prompt's new position
          exec_term!(TermCtl::Cursor(Up(scroll_amount as u16)))?;
        }
        self.needs_redraw = true;
        Ok(None)
      }
      LineCmd::ResetWidget => {
        self.reset_active_widget(false)?;
        Ok(None)
      }
      LineCmd::NormalSeq(line_nums, seq) => {
        let keys = expand_keymap(&seq);

        self.core.editor.start_undo_merge();
        for line in line_nums {
          self
            .core
            .editor
            .set_cursor(linebuf::Pos { row: line, col: 0 });
          self
            .core
            .swap_mode(&mut (Box::new(ViNormal::new()) as Box<dyn EditMode>));

          if let Err(e) = self.replay_keys(keys.clone(), false) {
            self.core.editor.stop_undo_merge();
            return Err(e);
          }
        }
        self.core.editor.stop_undo_merge();

        // just in case
        self
          .core
          .swap_mode(&mut (Box::new(ViNormal::new()) as Box<dyn EditMode>));

        Ok(None)
      }
      LineCmd::TriggerCompletion => Ok(self.handle_tab(key)),
      LineCmd::TriggerHistSearch => {
        self.start_hist_search();
        Ok(None)
      }
      LineCmd::SubmitLine(cmd) => {
        if self.core.editor.attempt_alias_expansion() {
          self.update_editor_hint();
        }
        if self.core.editor.attempt_history_expansion(&self.history) {
          // If history expansion occurred, don't submit yet
          self.update_editor_hint();

          Ok(None)
        } else if self.should_submit() || !shopt!(line.linebreak_on_incomplete) {
          self.submit()
        } else {
          self.run_cmd(cmd)
        }
      }
      LineCmd::AppendHint => Ok(self.accept_hint()),
    }
  }

  fn get_layout(&mut self, line: &str) -> Layout {
    let to_cursor = self.core.editor.window_slice_to_cursor();
    let cols = Shed::term(Terminal::t_cols);
    let prompt = layout::pad_prompt_for_gutter(
      self.prompt.get_ps1(),
      line,
      self.core.editor.scroll_offset(),
      cols,
    );
    Layout::from_parts(cols, &prompt, &to_cursor, line)
  }
  fn scroll_history_virtual(&mut self, cmd: EditCmd) {
    // This function is used for the Shift/Ctrl+Up/Down history concatenation.
    // Instead of replacing the buffer with a scrolled-to history entry
    // This function appends it to the end of the current buffer with '&&' or ';'
    // depending on if the user is holding shift or ctrl.

    let Cmd(count, motion) = &cmd.motion.unwrap();
    let sep = if cmd.flags.contains(CmdFlags::HAS_SHIFT) {
      " && "
    } else {
      "; "
    };
    match motion {
      Motion::LineUp => {
        self
          .core
          .editor
          .edit(|e| match self.history.virtual_scroll_direction() {
            Some(Direction::Forward) => {
              for _ in 0..*count {
                if !e.pop_right() {
                  e.clear_buffer();
                  self.history.stop_virtual_scroll();
                  break;
                }
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
          .core
          .editor
          .edit(|e| match self.history.virtual_scroll_direction() {
            Some(Direction::Backward) => {
              for _ in 0..*count {
                if !e.pop_left() {
                  e.clear_buffer();
                  self.history.stop_virtual_scroll();
                  break;
                }
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
  fn scroll_history_to(&mut self, hist_idx: usize) {
    let hist = self.focused_history();
    hist.merge_search_entries();
    hist.constrain_entries(None);
    let entry = self.focused_history().scroll_to(hist_idx).cloned();
    if entry.is_some() {
      let total = self.focused_history().search_mask_count();
      status_msg!("jumped to hist entry: {}/{}", hist_idx + 1, total);
    }
    self.swap_history_editor(entry);
  }
  fn scroll_history(&mut self, count: isize) {
    if self.focused_history().pending.is_none() {
      if count >= 0 {
        // if count >= 0, we are scrolling down
        // but if we are here, it means we are already at the pending command,
        // so return and bell
        Shed::term_mut(Terminal::send_bell).ok();
        return;
      }
      // We are scrolling up from a pending command
      // Let's refresh the search mask to make sure
      // our history is up to date
      let joined = self.core.editor.to_string();
      self.focused_history().update_search_mask(Some(&joined));
    }
    let entry = self.focused_history().scroll(count).cloned();
    self.swap_history_editor(entry);
  }
  fn swap_history_editor(&mut self, entry: Option<HistEntry>) {
    if let Some(entry) = entry {
      let editor = std::mem::take(self.core.focused_editor());
      self.core.focused_editor().set_buffer(entry.command());
      if self.focused_history().pending.is_none() {
        self.focused_history().pending = Some(editor);
      }
      self.core.focused_editor().clear_hint();
      self.core.focused_editor().move_cursor_to_end();
    } else if let Some(pending) = self.focused_history().pending.take() {
      *self.core.focused_editor() = pending;
    } else {
      // If we are here it should mean we are on our pending command
      // And the user tried to scroll history down
      // Since there is no "future" history, we should just bell and do nothing
      Shed::term_mut(Terminal::send_bell).ok();
      return;
    }
    let clamp = self.core.mode.clamp_cursor();
    self.core.focused_editor().set_cursor_clamp(clamp);
    self.core.focused_editor().fix_cursor();
  }
  fn should_accept_hint(&self, event: &KeyEvent) -> bool {
    if self.core.editor.cursor_at_max() && self.core.editor.has_hint() {
      match self.core.mode.report_mode() {
        ModeReport::Replace | ModeReport::Insert | ModeReport::Emacs => {
          matches!(event, KeyEvent(KeyCode::Right, ModKeys::NONE))
        }
        ModeReport::Visual | ModeReport::Normal => {
          matches!(event, KeyEvent(KeyCode::Right, ModKeys::NONE))
            || (self.core.mode.pending_seq().unwrap(/* always Some on normal mode */).is_empty()
              && matches!(event, KeyEvent(KeyCode::Char('l'), ModKeys::NONE)))
        }
        _ => false,
      }
    } else {
      false
    }
  }

  fn should_grab_history(&mut self, cmd: &EditCmd) -> bool {
    cmd.is_virtual_scroll()
      || cmd
        .verb()
        .is_some_and(|v| matches!(v, Cmd(_, Verb::HistoryUp | Verb::HistoryDown)))
      || cmd.verb().is_none()
        && (cmd
          .motion()
          .is_some_and(|m| matches!(m, Cmd(_, Motion::LineUp)))
          && self.core.editor.start_of_line() == 0)
      || (cmd
        .motion()
        .is_some_and(|m| matches!(m, Cmd(_, Motion::LineDown)))
        && self.core.editor.on_last_line())
        && !cmd.flags.contains(CmdFlags::IS_SUBMIT)
  }

  pub fn needs_redraw(&self) -> bool {
    self.needs_redraw
  }

  #[expect(clippy::too_many_lines)]
  pub fn print_line(&mut self, final_draw: bool) -> ShResult<()> {
    let _sync = SyncOutputGuard::begin();
    if self.statline.is_some() && !shopt!(statline.enable) {
      self.statline = None;
      let row = Shed::term(Terminal::t_rows) as u16;

      queue_term!(
        TermCtl::Cursor(SavePos),
        TermCtl::Cursor(Absolute { row, col: 1 }),
        TermCtl::Clear(WholeLine),
        TermCtl::Cursor(RestorePos),
        TermCtl::Scroll(Scroll::ResetRegion),
      )
      .ok();
    }

    // Cap the viewport so a tall buffer can't push the prompt off-screen.
    let t_rows = Shed::term(Terminal::t_rows);
    let t_cols = Shed::term(Terminal::t_cols);
    let prompt_end = Layout::calc_pos(
      t_cols,
      self.prompt.get_ps1(),
      Pos { col: 0, row: 0 },
      0,
      false,
    );
    let prompt_lines = prompt_end.row;
    // Always reserve at least one row at the bottom for ephemeral status
    // messages; reserve two when the full statline is on as well.
    let reserved = Terminal::reserved_rows() as usize;
    let viewport_cap = t_rows.saturating_sub(prompt_lines + reserved).max(1);
    self.core.editor.set_viewport_cap(Some(viewport_cap));

    let line = self.core.editor.display_window_joined();
    let mut new_layout = self.get_layout(&line);

    let pending_seq = self
      .macro_record
      .status()
      .or_else(|| self.core.mode.pending_seq());
    let mut prompt_string_right = self.prompt.psr_expanded.clone();
    let has_sub_editor = matches!(
      self.core.mode.report_mode(),
      ModeReport::Ex | ModeReport::RevSearch | ModeReport::Search
    );

    if prompt_string_right
      .as_ref()
      .is_some_and(|psr| psr.lines().count() > 1)
    {
      log::warn!("PSR has multiple lines, truncating to one line");
      prompt_string_right =
        prompt_string_right.map(|psr| psr.lines().next().unwrap_or_default().to_string());
    }

    let row0_used = self
      .prompt
      .get_ps1()
      .lines()
      .next()
      .map(|l| Layout::calc_pos(t_cols, l, Pos { col: 0, row: 0 }, 0, false))
      .map(|p| p.col)
      .unwrap_or_default();
    let one_line = new_layout.end.row == 0;

    if let Some(comp) = self.completer.as_mut() {
      comp.clear();
    }
    if let Some(finder) = self.history_fzf() {
      finder.clear();
    }

    let predicted_overlay_rows: u16 = self
      .completer
      .as_ref()
      .and_then(|c| c.predicted_rows())
      .unwrap_or(0)
      .saturating_add(
        self
          .focused_history()
          .fuzzy_finder
          .as_ref()
          .map_or(0, FuzzySelector::predicted_rows),
      )
      .try_into()
      .unwrap_or(u16::MAX);

    let mut system_msg = String::new();
    if Shed::system_msg_pending() {
      use std::fmt::Write as FmtWrite;
      while let Some(msg) = Shed::pop_system_msg() {
        writeln!(system_msg, "{msg}").ok();
      }
    }
    let system_msg_layout = Layout::from_parts(t_cols, "", &system_msg, &system_msg);

    if let Some(layout) = self.old_layout.as_ref() {
      layout::clear_rows(layout);

      let prev_overlay_rows = std::mem::take(&mut self.overlay_displacement);

      if shopt!(statline.enable) {
        let old_h = layout.end.row as i32 + i32::from(prev_overlay_rows);
        let mut new_h = new_layout.end.row as i32
          + i32::from(predicted_overlay_rows)
          + system_msg_layout.end.row as i32;
        if has_sub_editor {
          new_h += 1;
        }
        let diff = new_h - old_h;

        if diff < 0 {
          // Prompt shrank. Freed rows are now BELOW the prompt;
          // clear them so the fill-below pass at the end of print_line
          // can populate them with tildes.
          let delta = (-diff) as u16;
          Shed::term_mut(|t| {
            t.with_saved_cursor(|term| {
              for _ in 0..delta {
                write!(term, "\x1b[1B\x1b[2K").ok();
              }
            });
          });
          self.blank_rows_above = self.blank_rows_above.saturating_add(delta);
        }
      }
    }

    if !system_msg.is_empty() {
      queue_term!(TermCtl::Clear(ScreenFromCursor)).ok();
      write_term!("{system_msg}").ok();
    }

    redraw(
      self.prompt.get_ps1(),
      &line,
      &new_layout,
      self.core.editor.scroll_offset(),
      self.core.editor.lines().len(),
    );

    let seq_fits = pending_seq
      .as_ref()
      .is_some_and(|seq| row0_used + 1 < t_cols.saturating_sub(seq.width()));
    let psr_fits = prompt_string_right
      .as_ref()
      .is_some_and(|psr| new_layout.end.col + 1 < t_cols.saturating_sub(psr.width()));

    if !final_draw
      && let Some(seq) = pending_seq
      && !seq.is_empty()
      && !(prompt_string_right.is_some() && one_line)
      && seq_fits
      && !self.core.mode.is_input_mode()
    {
      // write our pending sequence
      let to_col = (t_cols - calc_str_width(&seq)) as u16;
      let up = new_layout.cursor.row as u16; // rows to move up from cursor to top line of prompt

      // Save cursor, move up to top row, move right to column, write sequence,
      // restore cursor
      queue_term!(
        TermCtl::Cursor(SavePos),
        TermCtl::Cursor(Up(up)),
        TermCtl::Cursor(Col(to_col)),
      )
      .ok();
      write_term!("{seq}").unwrap();
      queue_term!(TermCtl::Cursor(RestorePos)).ok();
    } else if !final_draw
      && let Some(psr) = prompt_string_right
      && psr_fits
    {
      // write PSR
      let to_col = (t_cols - calc_str_width(&psr)) as u16;
      let down = new_layout.end.row.saturating_sub(new_layout.cursor.row) as u16;

      queue_term!(
        TermCtl::Cursor(SavePos),
        TermCtl::Cursor(Down(down)),
        TermCtl::Cursor(Col(to_col)),
      )
      .ok();
      write_term!("{psr}").unwrap();
      queue_term!(TermCtl::Cursor(RestorePos)).ok();

      // Record where the PSR ends so clear_rows can account for wrapping
      // if the terminal shrinks.
      let psr_start = Pos {
        row: new_layout.end.row,
        col: to_col as usize,
      };
      new_layout.psr_end = Some(Layout::calc_pos(t_cols, &psr, psr_start, 0, false));
    }

    queue_term!(TermCtl::Cursor(SetStyle(self.core.mode.cursor_style()),)).ok();

    // Move to end of layout for overlay draws (completer, history search)
    let has_overlays = self.completer.is_some() || self.history_fzf().is_some();

    let down = new_layout.end.row.saturating_sub(new_layout.cursor.row);
    if has_overlays && down > 0 {
      queue_term!(TermCtl::Cursor(Down(down as u16))).ok();
      new_layout.cursor.row = new_layout.end.row;
    }

    // write sub-prompts for stuff like ex mode
    if let ModeReport::Ex | ModeReport::RevSearch | ModeReport::Search =
      self.core.mode.report_mode()
    {
      let mut pending_seq = self.core.mode.pending_seq().unwrap_or_default();
      let prefix_seq = match self.core.mode.report_mode() {
        ModeReport::Ex => ": ",
        ModeReport::RevSearch => "?",
        ModeReport::Search => "/",
        _ => unreachable!(),
      };
      let down = new_layout.end.row - new_layout.cursor.row;
      if let ModeReport::Ex = self.core.mode.report_mode()
        && shopt!(highlight.enable)
      {
        let cursor_pos = self.core.focused_editor().cursor_to_flat();
        let mut highlighted = String::new();
        highlight::highlight_ex(
          &mut highlighted,
          &pending_seq,
          &highlight::Palette::new(),
          cursor_pos,
        )
        .ok();
        pending_seq = highlighted;
      }

      queue_term!(
        TermCtl::Cursor(Down(down as u16)),
        TermCtl::Cursor(Col(1)),
        TermCtl::PrintChar('\n')
      )
      .ok();
      write_term!("{prefix_seq}{pending_seq}").ok();

      new_layout.end.row += 1;
      new_layout.cursor.row = new_layout.end.row;
      new_layout.cursor.col = {
        let cursor_offset = self.core.mode.pending_cursor().unwrap_or(pending_seq.len());
        let before_cursor = pending_seq
          .graphemes(true)
          .take(cursor_offset)
          .collect::<String>();

        prefix_seq.width() + before_cursor.width()
      };

      queue_term!(TermCtl::Cursor(Col(new_layout.cursor.col as u16 + 1))).ok();
    }

    // Tell the completer the width of the prompt line above its \n so it can
    // account for wrapping when clearing after a resize.
    let preceding_width = if new_layout.psr_end.is_some() {
      t_cols
    } else {
      // Without PSR, use the content width on the cursor's row
      (new_layout.end.col + 1).max(new_layout.cursor.col + 1)
    };

    let mut overlay_rows: usize = 0;
    if let Some(comp) = self.completer.as_mut() {
      comp.set_prompt_line_context(preceding_width, new_layout.end.col);
      overlay_rows += comp.draw();
    }

    if let Some(finder) = self.history_fzf() {
      finder.set_prompt_line_context(preceding_width, new_layout.end.col);
      overlay_rows += finder.draw();
    }
    self.overlay_displacement = overlay_rows.try_into().unwrap_or(u16::MAX);

    if let Some(statline) = self.statline.as_mut()
      && !final_draw
    {
      let cols = Shed::term(Terminal::t_cols);
      let rendered = statline.render(cols);
      Shed::term_mut(|t| t.draw_status_line(&rendered));
    }

    while let Some(msg) = Shed::pop_status_msg() {
      let now = Instant::now();
      self.status_msgs.push_back((msg, now));
    }
    while self.status_msgs.len() > 1 {
      self.status_msgs.pop_front();
    }

    if !final_draw {
      let content = if let Some((msg, time)) = self.status_msgs.front() {
        let elapsed = time.elapsed().as_secs();
        if elapsed < 5 {
          // Schedule a wakeup so the row clears when the message expires
          // even if the user isn't typing.
          let diff = 5000.0 - time.elapsed().as_millis() as f64;
          let timeout = PollTimeout::try_from(diff.max(0.0) as i32).unwrap_or(PollTimeout::NONE);
          Shed::meta_mut(|m| m.set_poll_timeout(Some(timeout)));
          // Reserved row is single-line; if the message has multiple lines,
          // show only the first one.
          msg.lines().next().unwrap_or("").to_string()
        } else {
          self.status_msgs.pop_front();
          String::new()
        }
      } else {
        String::new()
      };

      if !content.is_empty() {
        if shopt!(statline.enable) {
          Shed::term_mut(|t| t.draw_status_message(&content));
        } else {
          let gap_to_end = new_layout.end.row.saturating_sub(new_layout.cursor.row) as u16;
          let gap_to_msg = gap_to_end + if has_sub_editor { 1 } else { 2 };
          let return_col = new_layout.cursor.col as u16 + 1;
          for _ in 0..gap_to_msg {
            queue_term!(TermCtl::PrintChar('\n')).ok();
          }
          queue_term!(TermCtl::PrintChar('\r'), TermCtl::Clear(WholeLine),).ok();
          write_term!("{content}").ok();
          queue_term!(
            TermCtl::Cursor(Up(gap_to_msg)),
            TermCtl::Cursor(Col(return_col)),
          )
          .ok();
        }
      }
    }

    let finish = |this: &mut Self| {
      this.old_layout = Some(new_layout);
      this.needs_redraw = false;
      Ok(())
    };

    if !shopt!(statline.enable) || final_draw {
      return finish(self);
    }
    // if the status line is enabled, fill empty rows under the prompt with tildes

    let term_rows = Shed::term(Terminal::t_rows) as u16;
    let from_cursor_to_end =
      new_layout.end.row.saturating_sub(new_layout.cursor.row) as u16 + overlay_rows as u16;

    // Get the input cursor's absolute row so we can compute the
    // prompt's bottom row without escape-sequence ping-pong.
    let input_row = Shed::term_mut(Terminal::get_cursor_pos)
      .ok()
      .flatten()
      .map(|(r, _)| r.0 as u16);

    let Some(input_row) = input_row else {
      return finish(self);
    };

    let bottom_row = input_row.saturating_add(from_cursor_to_end);

    // status line reserves two rows
    let gap = (term_rows.saturating_sub(2)).saturating_sub(bottom_row);
    if gap > 0 {
      queue_term!(
        TermCtl::Cursor(SavePos),
        TermCtl::Cursor(Down(from_cursor_to_end)),
        TermCtl::Cursor(Col(1)),
      )
      .ok();

      for _ in 0..gap {
        queue_term!(
          TermCtl::Cursor(Down(1)),
          TermCtl::Cursor(Col(1)),
          TermCtl::Clear(WholeLine),
        )
        .ok();
        write_term!("\x1b[90m~\x1b[0m").ok();
      }

      queue_term!(TermCtl::Cursor(RestorePos),).ok();
    }

    finish(self)
  }

  pub fn try_swap_mode_from_str(&mut self, name: &str) -> bool {
    self.core.try_swap_mode_from_str(name)
  }

  fn update_editor_hint(&mut self) {
    self.history.update_pending_cmd((
      &self.core.editor.to_string(),
      self.core.editor.cursor_to_flat(),
    ));
    let hint = self.history.get_hint();
    self.core.editor.set_hint(hint);
  }

  pub(super) fn editor(&self) -> &LineBuf {
    &self.core.editor
  }

  pub(super) fn editor_mut(&mut self) -> &mut LineBuf {
    &mut self.core.editor
  }

  pub(super) fn pending_keymap(&self) -> &[KeyEvent] {
    &self.pending_keymap
  }

  pub(super) fn history(&self) -> &History {
    &self.history
  }

  pub(super) fn history_mut(&mut self) -> &mut History {
    &mut self.history
  }

  pub(super) fn pending_keymap_mut(&mut self) -> &mut Vec<KeyEvent> {
    &mut self.pending_keymap
  }

  #[cfg(test)]
  pub fn with_initial(mut self, initial: &str) -> Self {
    self.core.editor = LineBuf::new().with_initial(initial, 0);
    {
      let s = self.core.editor.to_string();
      let c = self.core.editor.cursor_to_flat();
      self.focused_history().update_pending_cmd((&s, c));
    }
    self
  }
}
