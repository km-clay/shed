use scopeguard::defer;

use super::editcmd::{Cmd, CmdFlags, EditCmd, Motion, Verb, invert_char_motion};
use super::editmode::{
  CmdReplay, EditMode, Emacs, ModeReport, RemoteMode, ViEx, ViInsert, ViNormal, ViReplace,
  ViSearch, ViSearchRev, ViVerbatim, ViVisual,
};
use super::linebuf::LineBuf;
use super::register::RegisterName;

use crate::{
  autocmd,
  keys::KeyEvent,
  motion,
  state::{
    Shed,
    vars::{VarFlags, VarKind},
  },
  util::ShResult,
};

pub(super) struct EditorCore {
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
    }
  }

  /// Construct a core seeded with `input`, starting in normal mode. Used by
  /// headless drivers (e.g. the `vicut` builtin).
  pub fn headless(input: &str) -> Self {
    let mut core = Self::new(Box::new(ViNormal::new()));
    core.editor = LineBuf::new().with_initial(input, 0);
    core
  }

  /// Feed one key: resolve it through the current mode into a command and
  /// execute it. No keymap matching, completion, or history; for headless and
  /// replay use.
  pub fn feed_key(&mut self, key: KeyEvent) -> ShResult<()> {
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
