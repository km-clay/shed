use crate::{readline::RegisterName, state::vars::VarStr, varstr};

use super::{
  CmdReplay, E as KeyEvent, EditMode, LineBuf, ModeReport, SimpleEditor,
  editcmd::{CmdFlags, Direction, EditCmd, Motion},
  history::History,
  key, motion,
  state::terminal::CursorStyle,
  status_msg,
};

trait SearchMode {
  fn command(&self) -> EditCmd {
    EditCmd {
      register: RegisterName::default(),
      verb: None,
      motion: Some(motion!(
        self.count(),
        Motion::Search(self.pattern(), self.direction())
      )),
      raw_seq: self.pattern().to_string(),
      flags: CmdFlags::EXIT_CUR_MODE,
    }
  }
  fn count(&self) -> usize;
  fn query_handle_key(&mut self, key: KeyEvent) -> Option<EditCmd> {
    self.query_mut().handle_key(key).map(|()| None).ok()?
  }
  fn pattern(&self) -> VarStr {
    varstr!("{}", self.query().buf)
  }
  fn clear(&mut self) {
    self.query_mut().buf.clear_buffer();
  }
  fn query_history(&mut self) -> Option<&mut History> {
    self.query_mut().history.as_mut()
  }
  fn query_cursor(&self) -> Option<usize> {
    Some(self.query().buf.cursor_to_flat())
  }

  fn direction(&self) -> Direction;
  fn query(&self) -> &SimpleEditor;
  fn query_mut(&mut self) -> &mut SimpleEditor;
  fn report_search_mode(&self) -> ModeReport;
}

pub struct ViSearch {
  query: SimpleEditor,
  count: usize,
}

impl ViSearch {
  pub fn new(count: usize) -> Self {
    Self {
      query: SimpleEditor::new(Some("search_history")),
      count,
    }
  }
}

impl Default for ViSearch {
  fn default() -> Self {
    Self::new(1)
  }
}

pub struct ViSearchRev {
  query: SimpleEditor,
  count: usize,
}

impl ViSearchRev {
  pub fn new(count: usize) -> Self {
    Self {
      query: SimpleEditor::new(Some("search_history")),
      count,
    }
  }
}

impl Default for ViSearchRev {
  fn default() -> Self {
    Self::new(1)
  }
}

impl SearchMode for ViSearch {
  fn direction(&self) -> Direction {
    Direction::Forward
  }

  fn count(&self) -> usize {
    self.count
  }

  fn query(&self) -> &SimpleEditor {
    &self.query
  }

  fn query_mut(&mut self) -> &mut SimpleEditor {
    &mut self.query
  }

  fn report_search_mode(&self) -> ModeReport {
    ModeReport::Search
  }
}

impl SearchMode for ViSearchRev {
  fn direction(&self) -> Direction {
    Direction::Backward
  }

  fn count(&self) -> usize {
    self.count
  }

  fn query(&self) -> &SimpleEditor {
    &self.query
  }

  fn query_mut(&mut self) -> &mut SimpleEditor {
    &mut self.query
  }

  fn report_search_mode(&self) -> ModeReport {
    ModeReport::RevSearch
  }
}

impl<S: SearchMode> EditMode for S {
  #[expect(clippy::unnested_or_patterns)]
  fn handle_key(&mut self, key: KeyEvent) -> Option<EditCmd> {
    match key {
      key!('\r') | key!(Enter) => {
        let cmd = self.command();
        let pat = self.pattern();

        if let Some(hist) = self.history()
          && let Err(e) = hist.push(&pat.to_str_lossy())
        {
          status_msg!("Failed to save search to history: {e}");
        }

        Some(cmd)
      }
      key!(Ctrl + 'c') => {
        self.clear();
        None
      }
      key!(Backspace) if self.pattern().is_empty() => Some(EditCmd {
        register: RegisterName::default(),
        verb: None,
        motion: None,
        flags: CmdFlags::EXIT_CUR_MODE | CmdFlags::IS_CANCEL,
        raw_seq: String::new(),
      }),
      key!(Esc) => Some(EditCmd {
        register: RegisterName::default(),
        verb: None,
        motion: None,
        flags: CmdFlags::EXIT_CUR_MODE | CmdFlags::IS_CANCEL,
        raw_seq: String::new(),
      }),
      _ => self.query_handle_key(key),
    }
  }
  fn history(&mut self) -> Option<&mut History> {
    self.query_history()
  }
  fn cursor_style(&self) -> CursorStyle {
    CursorStyle::Beam(false)
  }
  fn editor(&mut self) -> Option<&mut LineBuf> {
    Some(&mut self.query_mut().buf)
  }
  fn is_input_mode(&self) -> bool {
    true
  }
  fn is_repeatable(&self) -> bool {
    false
  }
  fn as_replay(&self) -> Option<CmdReplay> {
    None
  }
  fn pending_seq(&self) -> Option<VarStr> {
    Some(self.pattern())
  }
  fn pending_cursor(&self) -> Option<usize> {
    self.query_cursor()
  }
  fn clamp_cursor(&self) -> bool {
    false
  }
  fn report_mode(&self) -> ModeReport {
    self.report_search_mode()
  }
}
