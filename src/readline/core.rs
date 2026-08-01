use std::fmt::Debug;

use scopeguard::defer;

use super::editcmd::{Cmd, CmdFlags, EditCmd, Motion, Verb, invert_char_motion};
use super::editmode::{
  CmdReplay, EditMode, Emacs, ModeReport, RemoteMode, ViEx, ViInsert, ViNormal, ViReplace,
  ViSearch, ViSearchRev, ViVerbatim, ViVisual,
};
use super::linebuf::{LineBuf, Pos};
use super::register::RegisterName;

use crate::keys::{KeyCode, ModKeys};
use crate::{
  autocmd,
  expand::expand_keymap,
  keys::KeyEvent,
  motion,
  state::{
    Shed,
    vars::{VarFlags, VarKind},
  },
  util::ShResult,
};

pub(crate) struct EditorCore {
  pub editor: LineBuf,
  pub mode: Box<dyn EditMode>,
  pub saved_mode: Option<Box<dyn EditMode>>,
  pub repeat_action: Option<CmdReplay>,
  pub repeat_motion: Option<Cmd<Motion>>,

  /// Set whenever an executed command mutated the buffer; the interactive
  /// wrapper reads and clears this to schedule a redraw.
  pub needs_redraw: bool,
  /// Set when an executed command shelled out, so the wrapper can refresh the
  /// prompt. Drained by the wrapper.
  pub shell_cmd_ran: bool,
  /// Set when the editing mode changed, so the wrapper can refresh a
  /// mode-aware prompt. Drained by the wrapper.
  pub mode_changed: bool,
}

impl EditorCore {
  pub fn new(mode: Box<dyn EditMode>) -> Self {
    Self {
      editor: LineBuf::new(),
      mode,
      saved_mode: None,
      repeat_action: None,
      repeat_motion: None,
      needs_redraw: false,
      shell_cmd_ran: false,
      mode_changed: false,
    }
  }

  /// Construct a core seeded with `input`, starting in normal mode. Used by
  /// headless drivers (e.g. the `vicut` builtin).
  pub fn headless(input: &str) -> Self {
    let mut core = Self::new(Box::new(ViNormal::new()));
    core.editor = LineBuf::new().with_initial(input, 0);
    core
  }

  pub fn empty() -> Self {
    Self::new(Box::new(ViNormal::new()))
  }

  pub fn set_buffer(&mut self, input: &str) {
    self.editor.set_buffer(input);
    self.editor.set_cursor_from_flat(0);
  }

  /// Feed one key: resolve it through the current mode into a command and
  /// execute it. No keymap matching, completion, or history; for headless and
  /// replay use.
  pub fn feed_key(&mut self, key: KeyEvent) -> ShResult<()> {
    self.editor.set_cursor_clamp(self.mode.clamp_cursor());
    let Some(cmd) = self.mode.handle_key(key) else {
      return Ok(());
    };
    self.exec_cmd(cmd, false)
  }

  pub fn feed_keys(&mut self, keys: impl IntoIterator<Item = KeyEvent>) -> ShResult<()> {
    for key in keys {
      self.feed_key(key)?;
    }
    Ok(())
  }

  pub fn feed_key_fallible(&mut self, key: KeyEvent) -> ShResult<bool> {
    self.editor.set_cursor_clamp(self.mode.clamp_cursor());
    let Some(cmd) = self.mode.handle_key(key) else {
      return Ok(true);
    };
    self.exec_cmd(cmd, false)?;
    Ok(!self.editor.search_failed())
  }

  pub fn feed_keys_fallible(&mut self, keys: impl IntoIterator<Item = KeyEvent>) -> ShResult<bool> {
    for key in keys {
      if !self.feed_key_fallible(key)? {
        return Ok(false);
      }
    }
    Ok(true)
  }

  /// The full buffer contents as a string.
  pub fn text(&self) -> String {
    self.editor.to_string()
  }

  /// The text currently selected, if any. Used by headless drivers to capture
  /// the span a motion traversed.
  pub fn selection(&mut self) -> Option<String> {
    self.editor.selection_str()
  }

  /// The editor sub-buffer that currently has focus. Ex mode supplies its own
  /// line buffer; everything else edits the main one.
  pub fn focused_editor(&mut self) -> &mut LineBuf {
    self.mode.editor().unwrap_or(&mut self.editor)
  }

  pub fn update_editor_search(&mut self) {
    if matches!(
      self.mode.report_mode(),
      ModeReport::RevSearch | ModeReport::Search
    ) {
      self.editor.update_pending_search(self.mode.pending_seq());
      self.needs_redraw = true;
    }
  }

  /// Execute a command against the focused buffer. The pure editor half of the
  /// old `fire_editor_command`; the wrapper handles statline/prompt refresh by
  /// draining the flags raised here.
  fn fire(&mut self, cmd: &EditCmd) -> ShResult<()> {
    if cmd.is_shell_cmd() {
      self.shell_cmd_ran = true;
    }
    let res = self.editor.exec_cmd(cmd);
    self.needs_redraw = true;
    res
  }

  pub fn try_swap_mode_from_str(&mut self, name: &str) -> bool {
    let Ok(mode) = name.parse::<ModeReport>() else {
      return false;
    };
    let mut mode = mode.as_edit_mode();
    self.swap_mode(&mut mode);
    true
  }

  pub fn swap_mode(&mut self, mode: &mut Box<dyn EditMode>) {
    autocmd!(PreModeChange);
    defer!(autocmd!(PostModeChange));

    std::mem::swap(&mut self.mode, mode);
    self.mode_changed = true;
    self.editor.set_cursor_clamp(self.mode.clamp_cursor());
    Shed::vars_mut(|v| {
      v.set_var(
        "SHED_EDIT_MODE",
        VarKind::string(self.mode.report_mode().to_string()),
        VarFlags::empty(),
      )
    })
    .ok();
  }

  /// The line numbers an ex command's address resolves to, defaulting to the
  /// current line. Mirrors `ShedLine::extract_line_nums`.
  fn normal_seq_lines(&self, cmd: &EditCmd) -> ShResult<Vec<usize>> {
    if let Some(Cmd(_, Verb::ExCmd(node))) = cmd.verb() {
      self.editor.lines_for_ex_node(node)
    } else {
      Ok(vec![self.editor.row()])
    }
  }

  /// Run a `:normal` key sequence on each addressed line, in normal mode,
  /// folded into one undo step.
  fn run_normal_seq(&mut self, lines: &[usize], seq: &str) -> ShResult<()> {
    let keys = expand_keymap(seq);
    self.editor.start_undo_merge();
    for &line in lines {
      self.editor.set_cursor(Pos { row: line, col: 0 });
      self.reset_mode(false)?;
      if let Err(e) = self.feed_keys(keys.clone()) {
        self.editor.stop_undo_merge();
        return Err(e);
      }
    }
    self.editor.stop_undo_merge();
    Ok(())
  }

  /// Finalize a pending command-line mode (Ex / Search / `RevSearch`) by feeding
  /// `Enter`, the way pressing it would. No-op in any other mode.
  pub fn submit_cmdline(&mut self) -> ShResult<()> {
    if matches!(
      self.mode.report_mode(),
      ModeReport::Ex | ModeReport::Search | ModeReport::RevSearch
    ) {
      self.feed_key(KeyEvent(KeyCode::Enter, ModKeys::NONE))?;
    }
    Ok(())
  }

  pub fn reset_mode(&mut self, submit_pending: bool) -> ShResult<()> {
    if submit_pending {
      self.submit_cmdline()?;
    }

    let mut mode: Box<dyn EditMode> = Box::new(ViNormal::new());
    self.swap_mode(&mut mode);
    Ok(())
  }

  #[expect(clippy::too_many_lines)]
  pub fn exec_mode_transition(&mut self, mut cmd: EditCmd, from_replay: bool) -> ShResult<()> {
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

        Verb::ExMode => Box::new(ViEx::new(self.editor.is_selecting())),

        Verb::VerbatimMode => {
          Shed::term_mut(|t| t.verbatim_single(true));
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

          return self.fire(&cmd);
        }
        Verb::VisualMode => {
          self.editor.start_char_select();
          Box::new(ViVisual::new())
        }
        Verb::VisualModeLine => {
          self.editor.start_line_select();
          Box::new(ViVisual::new())
        }

        Verb::SearchMode => Box::new(ViSearch::new(count)),
        Verb::RevSearchMode => Box::new(ViSearchRev::new(count)),

        _ => unreachable!(),
      }
    };

    // The mode we just created swaps places with our current mode.
    // After this line, 'mode' contains our previous mode.
    self.swap_mode(&mut mode);

    if matches!(mode.report_mode(), ModeReport::Insert | ModeReport::Replace) {
      self.editor.stop_undo_merge();
    }

    if matches!(
      self.mode.report_mode(),
      ModeReport::Ex | ModeReport::Verbatim
    ) {
      self.saved_mode = Some(mode);
      Shed::vars_mut(|v| {
        v.set_var(
          "SHED_EDIT_MODE",
          VarKind::string(self.mode.report_mode().to_string()),
          VarFlags::empty(),
        )
      })?;
      return Ok(());
    }

    if mode.is_repeatable() && !from_replay {
      self.repeat_action = mode.as_replay();
    }

    if let Some(range) = self.editor.select_range()
      && cmd
        .verb()
        .is_some_and(|v| !matches!(v.1, Verb::VisualMode | Verb::VisualModeLine))
    {
      cmd.motion = Some(motion!(range));
    }

    // Set cursor clamp BEFORE executing the command so that motions
    // (like EndOfLine for 'A') can reach positions valid in the new mode
    self.editor.set_cursor_clamp(self.mode.clamp_cursor());
    self.fire(&cmd)?;

    if mode.report_mode() == ModeReport::Visual && self.editor.select_range().is_some() {
      self.editor.stop_selecting();
    }

    if is_insert_mode {
      self.editor.mark_insert_mode_start_pos();
    } else {
      self.editor.clear_insert_mode_start_pos();
    }

    Shed::vars_mut(|v| {
      v.set_var(
        "SHED_EDIT_MODE",
        VarKind::string(self.mode.report_mode().to_string()),
        VarFlags::empty(),
      )
    })?;

    Ok(())
  }

  fn handle_cmd_repeat(&mut self, cmd: EditCmd) -> ShResult<()> {
    let Some(replay) = self.repeat_action.clone() else {
      return Ok(());
    };
    let EditCmd { verb, .. } = cmd;
    let Cmd(count, _) = verb.unwrap();
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
            if i == 0 {
              self.editor.start_undo_merge();
            }
          }
          self.editor.stop_undo_merge();

          let old_mode_clone: Box<dyn EditMode> = match old_mode {
            ModeReport::Normal => Box::new(ViNormal::new()),
            ModeReport::Insert => Box::new(ViInsert::new()),
            ModeReport::Visual => Box::new(ViVisual::new()),
            ModeReport::Replace => Box::new(ViReplace::new()),
            ModeReport::Verbatim => Box::new(ViVerbatim::new()),
            ModeReport::Emacs => Box::new(Emacs::new()),
            ModeReport::Remote => Box::new(RemoteMode),
            ModeReport::Ex => Box::new(ViEx::new(self.editor.is_selecting())),
            ModeReport::Search => Box::new(ViSearch::new(1)),
            ModeReport::RevSearch => Box::new(ViSearchRev::new(1)),
          };
          self.mode = old_mode_clone;
        }
      }
      CmdReplay::Single(mut cmd) => {
        if count > 1 {
          if cmd.verb.is_some() {
            if let Some(v_mut) = cmd.verb.as_mut() {
              v_mut.0 = count;
            }
            if let Some(m_mut) = cmd.motion.as_mut() {
              m_mut.0 = 1;
            }
          } else {
            return Ok(());
          }
        }
        self.fire(&cmd)?;
      }
    }
    Ok(())
  }

  fn handle_motion_repeat(&mut self, cmd: EditCmd) -> ShResult<()> {
    match cmd.motion.as_ref().unwrap() {
      Cmd(count, Motion::RepeatMotion) => {
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
        self.fire(&repeat_cmd)
      }
      Cmd(count, Motion::RepeatMotionRev) => {
        let Some(motion) = self.repeat_motion.clone() else {
          return Ok(());
        };
        let mut new_motion = invert_char_motion(motion);
        new_motion.0 = *count;
        let repeat_cmd = EditCmd {
          register: RegisterName::default(),
          verb: cmd.verb,
          motion: Some(new_motion),
          raw_seq: format!("{count},"),
          flags: CmdFlags::empty(),
        };
        self.fire(&repeat_cmd)
      }
      _ => unreachable!(),
    }
  }

  pub fn exec_cmd(&mut self, mut cmd: EditCmd, from_replay: bool) -> ShResult<()> {
    // `:normal` runs a key sequence on each addressed line. It needs the mode
    // machine, so it's handled here rather than in LineBuf's ex dispatch (the
    // interactive layer intercepts it earlier; this catches the headless path).
    if let Some((seq, _)) = cmd.try_get_normal_seq() {
      let seq = seq.to_string();
      let lines = self.normal_seq_lines(&cmd)?;
      return self.run_normal_seq(&lines, &seq);
    }
    // `:q`/`:wq` are interactive-submit concepts; there is nothing to quit when
    // driving the editor headlessly.
    if cmd.is_quit() || cmd.is_write_quit() {
      return Ok(());
    }

    if cmd.verb().is_some()
      && let Some(range) = self.editor.select_range()
    {
      cmd.motion = Some(motion!(range));
    }

    if cmd.flags.contains(CmdFlags::IS_CANCEL) {
      self.editor.clear_pending_search();
    }

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
        let mut replay_cmd = cmd.clone();
        if self.mode.report_mode() == ModeReport::Visual {
          if let Some(shape_motion) = self.editor.select_mode() {
            replay_cmd.motion = Some(motion!(shape_motion));
          } else {
            log::warn!("You're in visual mode with no select range??");
          }
        }
        self.repeat_action = Some(CmdReplay::Single(Box::new(replay_cmd)));
      }

      if cmd.is_char_search() {
        self.repeat_motion.clone_from(&cmd.motion);
      }

      self.fire(&cmd)?;

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
}

impl Debug for EditorCore {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("EditorCore")
      .field("editor", &self.editor)
      .field("mode", &self.mode.report_mode())
      .field(
        "saved_mode",
        &self.saved_mode.as_ref().map(|m| m.report_mode()),
      )
      .field("repeat_action", &self.repeat_action)
      .field("repeat_motion", &self.repeat_motion)
      .field("needs_redraw", &self.needs_redraw)
      .field("shell_cmd_ran", &self.shell_cmd_ran)
      .field("mode_changed", &self.mode_changed)
      .finish()
  }
}

impl Clone for EditorCore {
  fn clone(&self) -> Self {
    Self {
      editor: self.editor.clone(),
      mode: self.mode.report_mode().as_edit_mode(),
      saved_mode: self
        .saved_mode
        .as_ref()
        .map(|m| m.report_mode().as_edit_mode()),
      repeat_action: self.repeat_action.clone(),
      repeat_motion: self.repeat_motion.clone(),
      needs_redraw: self.needs_redraw,
      shell_cmd_ran: self.shell_cmd_ran,
      mode_changed: self.mode_changed,
    }
  }
}
