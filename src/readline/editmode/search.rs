use crate::{key, motion, readline::{SimpleEditor, editcmd::{CmdFlags, Direction, EditCmd, Motion, MotionCmd, To}, editmode::{CmdReplay, EditMode, ModeReport}, history::History, keys::KeyEvent, linebuf::LineBuf}, state::{CursorStyle, write_meta}};

trait SearchMode {
  fn command(&self) -> EditCmd {
    EditCmd {
      register: Default::default(),
      verb: None,
      motion: Some(motion!(Motion::Search(self.pattern(), self.direction()))),
      raw_seq: self.pattern(),
      flags: CmdFlags::EXIT_CUR_MODE,
    }
  }
  fn query_handle_key(&mut self, key: KeyEvent) -> Option<EditCmd> {
    self.query_mut().handle_key(key).map(|_| None).ok()?
  }
  fn pattern(&self) -> String { self.query().buf.joined() }
  fn clear(&mut self) { self.query_mut().buf.clear_buffer(); }
  fn query_history(&mut self) -> Option<&mut History> { self.query_mut().history.as_mut() }
  fn query_cursor(&self) -> Option<usize> { Some(self.query().buf.cursor_to_flat()) }

  fn direction(&self) -> Direction;
  fn query(&self) -> &SimpleEditor;
  fn query_mut(&mut self) -> &mut SimpleEditor;
  fn report_search_mode(&self) -> ModeReport;
}

pub struct ViSearch {
  query: SimpleEditor,
}

impl ViSearch {
  pub fn new() -> Self {
    Self { query: SimpleEditor::new(Some("search_history")) }
  }
}

impl Default for ViSearch {
  fn default() -> Self {
    Self::new()
  }
}

pub struct ViSearchRev {
  query: SimpleEditor,
}

impl ViSearchRev {
  pub fn new() -> Self {
    Self { query: SimpleEditor::new(Some("search_history")) }
  }
}

impl Default for ViSearchRev {
  fn default() -> Self {
    Self::new()
  }
}

impl SearchMode for ViSearch {
  fn direction(&self) -> Direction { Direction::Forward }

  fn query(&self) -> &SimpleEditor { &self.query }

  fn query_mut(&mut self) -> &mut SimpleEditor { &mut self.query }

  fn report_search_mode(&self) -> ModeReport { ModeReport::Search }
}

impl SearchMode for ViSearchRev {
  fn direction(&self) -> Direction { Direction::Backward }

  fn query(&self) -> &SimpleEditor { &self.query }

  fn query_mut(&mut self) -> &mut SimpleEditor { &mut self.query }

  fn report_search_mode(&self) -> ModeReport { ModeReport::RevSearch }
}

impl<S: SearchMode> EditMode for S {
  fn handle_key(&mut self, key: KeyEvent) -> Option<EditCmd> {
    match key {
      key!('\r') | key!(Enter) => {
        let cmd = self.command();
        let pat = self.pattern();

        if let Some(hist) = self.history()
          && let Err(e) = hist.push(pat)
        {
          write_meta(|m| {
            m.post_status_message(format!("Failed to save ex command to history: {e}"))
          });
        }

        Some(cmd)
      }
      key!(Ctrl + 'c') => {
        self.clear();
        None
      }
      key!(Backspace) if self.pattern().is_empty() => Some(EditCmd {
        register: Default::default(),
        verb: None,
        motion: None,
        flags: CmdFlags::EXIT_CUR_MODE | CmdFlags::IS_CANCEL,
        raw_seq: "".into(),
      }),
      key!(Esc) => Some(EditCmd {
        register: Default::default(),
        verb: None,
        motion: None,
        flags: CmdFlags::EXIT_CUR_MODE | CmdFlags::IS_CANCEL,
        raw_seq: "".into(),
      }),
      _ => self.query_handle_key(key)
    }
  }
  fn history(&mut self) -> Option<&mut History> {
    self.query_history()
  }
  fn cursor_style(&self) -> String {
    CursorStyle::Beam(false).to_string()
  }
  fn editor(&mut self) -> Option<&mut LineBuf> {
    Some(&mut self.query_mut().buf)
  }
  fn is_input_mode(&self) -> bool { true }
  fn is_repeatable(&self) -> bool { false }
  fn as_replay(&self) -> Option<CmdReplay> { None }
  fn pending_seq(&self) -> Option<String> { Some(self.pattern()) }
  fn pending_cursor(&self) -> Option<usize> { self.query_cursor() }
  fn move_cursor_on_undo(&self) -> bool { true }
  fn clamp_cursor(&self) -> bool { false }
  fn hist_scroll_start_pos(&self) -> Option<To> { None }
  fn report_mode(&self) -> ModeReport { self.report_search_mode() }
}
