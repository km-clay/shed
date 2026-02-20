use history::History;
use keys::{KeyCode, KeyEvent, ModKeys};
use linebuf::{LineBuf, SelectAnchor, SelectMode};
use nix::libc::STDOUT_FILENO;
use term::{KeyReader, Layout, LineWriter, PollReader, TermWriter, get_win_size};
use vicmd::{CmdFlags, Motion, MotionCmd, RegisterName, Verb, VerbCmd, ViCmd};
use vimode::{CmdReplay, ModeReport, ViInsert, ViMode, ViNormal, ViReplace, ViVisual};

use crate::libsh::sys::TTY_FILENO;
use crate::prelude::*;
use crate::{
  libsh::{
    error::ShResult,
    term::{Style, Styled},
  },
  parse::lex::{self, LexFlags, Tk, TkFlags, TkRule},
  prompt::readline::{complete::Completer, highlight::Highlighter},
};

pub mod complete;
pub mod highlight;
pub mod history;
pub mod keys;
pub mod layout;
pub mod linebuf;
pub mod register;
pub mod term;
pub mod vicmd;
pub mod vimode;

pub mod markers {
  use super::Marker;

  // token-level (derived from token class)
  pub const COMMAND: Marker = '\u{fdd0}';
  pub const BUILTIN: Marker = '\u{fdd1}';
  pub const ARG: Marker = '\u{fdd2}';
  pub const KEYWORD: Marker = '\u{fdd3}';
  pub const OPERATOR: Marker = '\u{fdd4}';
  pub const REDIRECT: Marker = '\u{fdd5}';
  pub const COMMENT: Marker = '\u{fdd6}';
  pub const ASSIGNMENT: Marker = '\u{fdd7}';
  pub const CMD_SEP: Marker = '\u{fde0}';
  pub const CASE_PAT: Marker = '\u{fde1}';
  pub const SUBSH: Marker = '\u{fde7}';
  pub const SUBSH_END: Marker = '\u{fde8}';

  // sub-token (needs scanning)
  pub const VAR_SUB: Marker = '\u{fdda}';
  pub const VAR_SUB_END: Marker = '\u{fde3}';
  pub const CMD_SUB: Marker = '\u{fdd8}';
  pub const CMD_SUB_END: Marker = '\u{fde4}';
  pub const PROC_SUB: Marker = '\u{fdd9}';
  pub const PROC_SUB_END: Marker = '\u{fde9}';
  pub const STRING_DQ: Marker = '\u{fddb}';
  pub const STRING_DQ_END: Marker = '\u{fde5}';
  pub const STRING_SQ: Marker = '\u{fddc}';
  pub const STRING_SQ_END: Marker = '\u{fde6}';
  pub const ESCAPE: Marker = '\u{fddd}';
  pub const GLOB: Marker = '\u{fdde}';

  pub const RESET: Marker = '\u{fde2}';

  pub const NULL: Marker = '\u{fdef}';

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

  pub fn is_marker(c: Marker) -> bool {
    TOKEN_LEVEL.contains(&c) || SUB_TOKEN.contains(&c) || END_MARKERS.contains(&c)
  }
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

pub struct FernVi {
  pub reader: PollReader,
  pub writer: Box<dyn LineWriter>,

  pub prompt: String,
  pub highlighter: Highlighter,
  pub completer: Completer,

  pub mode: Box<dyn ViMode>,
  pub repeat_action: Option<CmdReplay>,
  pub repeat_motion: Option<MotionCmd>,
  pub editor: LineBuf,

  pub old_layout: Option<Layout>,
  pub history: History,

  pub needs_redraw: bool,
}

impl FernVi {
  pub fn new(prompt: Option<String>, tty: RawFd) -> ShResult<Self> {
    let mut new = Self {
      reader: PollReader::new(),
      writer: Box::new(TermWriter::new(tty)),
      prompt: prompt.unwrap_or("$ ".styled(Style::Green)),
      completer: Completer::new(),
      highlighter: Highlighter::new(),
      mode: Box::new(ViInsert::new()),
      old_layout: None,
      repeat_action: None,
      repeat_motion: None,
      editor: LineBuf::new(),
      history: History::new()?,
      needs_redraw: true,
    };
    new.print_line()?;
    Ok(new)
  }

  pub fn with_initial(mut self, initial: &str) -> Self {
    self.editor = LineBuf::new().with_initial(initial, 0);
    self
      .history
      .update_pending_cmd((self.editor.as_str(), self.editor.cursor.get()));
    self
  }

  /// Feed raw bytes from stdin into the reader's buffer
  pub fn feed_bytes(&mut self, bytes: &[u8]) {
    self.reader.feed_bytes(bytes);
  }

  /// Mark that the display needs to be redrawn (e.g., after SIGWINCH)
  pub fn mark_dirty(&mut self) {
    self.needs_redraw = true;
  }

  /// Reset readline state for a new prompt
  pub fn reset(&mut self, prompt: Option<String>) {
    if let Some(p) = prompt {
      self.prompt = p;
    }
    self.editor = Default::default();
    self.mode = Box::new(ViInsert::new());
    self.old_layout = None;
    self.needs_redraw = true;
    self.history.pending = None;
    self.history.reset();
  }

  /// Process any available input and return readline event
  /// This is non-blocking - returns Pending if no complete line yet
  pub fn process_input(&mut self) -> ShResult<ReadlineEvent> {
    // Redraw if needed
    if self.needs_redraw {
      self.print_line()?;
      self.needs_redraw = false;
    }

    // Process all available keys
    while let Some(key) = self.reader.read_key()? {
      if self.should_accept_hint(&key) {
        self.editor.accept_hint();
        if !self.history.at_pending() {
          self.history.reset_to_pending();
        }
        self
          .history
          .update_pending_cmd((self.editor.as_str(), self.editor.cursor.get()));
        self.needs_redraw = true;
        continue;
      }

      if let KeyEvent(KeyCode::Tab, mod_keys) = key {
        let direction = match mod_keys {
          ModKeys::SHIFT => -1,
          _ => 1,
        };
        let line = self.editor.as_str().to_string();
        let cursor_pos = self.editor.cursor_byte_pos();

        match self.completer.complete(line, cursor_pos, direction)? {
          Some(line) => {
            let span_start = self.completer.token_span.0;
            let new_cursor = span_start
              + self
                .completer
                .selected_candidate()
                .map(|c| c.len())
                .unwrap_or_default();

            self.editor.set_buffer(line);
            self.editor.cursor.set(new_cursor);

            if !self.history.at_pending() {
              self.history.reset_to_pending();
            }
            self
              .history
              .update_pending_cmd((self.editor.as_str(), self.editor.cursor.get()));
            let hint = self.history.get_hint();
            self.editor.set_hint(hint);
          }
          None => match crate::state::read_shopts(|s| s.core.bell_style) {
            crate::shopt::FernBellStyle::Audible => {
              self.writer.flush_write("\x07")?;
            }
            crate::shopt::FernBellStyle::Visible | crate::shopt::FernBellStyle::Disable => {}
          },
        }

        self.needs_redraw = true;
        continue;
      }

      // if we are here, we didnt press tab
      // so we should reset the completer state
      self.completer.reset();

      let Some(mut cmd) = self.mode.handle_key(key) else {
        continue;
      };
      cmd.alter_line_motion_if_no_verb();

      if self.should_grab_history(&cmd) {
        self.scroll_history(cmd);
        self.needs_redraw = true;
        continue;
      }

      if cmd.should_submit() {
        self.editor.set_hint(None);
        self.print_line()?;
        self.writer.flush_write("\n")?;
        let buf = self.editor.take_buf();
        // Save command to history if auto_hist is enabled
        if crate::state::read_shopts(|s| s.core.auto_hist) && !buf.is_empty() {
          self.history.push(buf.clone());
          if let Err(e) = self.history.save() {
            eprintln!("Failed to save history: {e}");
          }
        }
        self.history.reset();
        return Ok(ReadlineEvent::Line(buf));
      }

      if cmd.verb().is_some_and(|v| v.1 == Verb::EndOfFile) {
        if self.editor.buffer.is_empty() {
          return Ok(ReadlineEvent::Eof);
        } else {
          self.editor.buffer.clear();
          self.needs_redraw = true;
          continue;
        }
      }

      let before = self.editor.buffer.clone();
      self.exec_cmd(cmd)?;
      let after = self.editor.as_str();

      if before != after {
        self
          .history
          .update_pending_cmd((self.editor.as_str(), self.editor.cursor.get()));
      }

      let hint = self.history.get_hint();
      self.editor.set_hint(hint);
      self.needs_redraw = true;
    }

    // Redraw if we processed any input
    if self.needs_redraw {
      self.print_line()?;
      self.needs_redraw = false;
    }

    Ok(ReadlineEvent::Pending)
  }

  pub fn get_layout(&mut self, line: &str) -> Layout {
    let to_cursor = self.editor.slice_to_cursor().unwrap_or_default();
    let (cols, _) = get_win_size(*TTY_FILENO);
    let tab_stop = crate::state::read_shopts(|s| s.prompt.tab_stop) as u16;
    Layout::from_parts(tab_stop, cols, &self.prompt, to_cursor, line)
  }
  pub fn scroll_history(&mut self, cmd: ViCmd) {
    /*
    if self.history.cursor_entry().is_some_and(|ent| ent.is_new()) {
      let constraint = SearchConstraint::new(SearchKind::Prefix, self.editor.to_string());
      self.history.constrain_entries(constraint);
    }
    */
    let count = &cmd.motion().unwrap().0;
    let motion = &cmd.motion().unwrap().1;
    let count = match motion {
      Motion::LineUpCharwise => -(*count as isize),
      Motion::LineDownCharwise => *count as isize,
      _ => unreachable!(),
    };
    let entry = self.history.scroll(count);
    log::info!("Scrolled history, got entry: {:?}", entry.as_ref());
    if let Some(entry) = entry {
			let cursor_pos = self.editor.cursor.get();
			log::info!("Saving pending command to history: {:?} at cursor pos {}", self.editor.as_str(), cursor_pos);
      let pending = self.editor.take_buf();
      self.editor.set_buffer(entry.command().to_string());
      if self.history.pending.is_none() {
        self.history.pending = Some((pending, cursor_pos));
      }
      self.editor.set_hint(None);
			self.editor.move_cursor_to_end();
    } else if let Some(pending) = self.history.pending.take() {
      log::info!("Setting buffer to pending command: {:?}", &pending);
      self.editor.set_buffer(pending.0);
      self.editor.cursor.set(pending.1);
      self.editor.set_hint(None);
    }
  }
  pub fn should_accept_hint(&self, event: &KeyEvent) -> bool {
    if self.editor.cursor_at_max() && self.editor.has_hint() {
      match self.mode.report_mode() {
        ModeReport::Replace | ModeReport::Insert => {
          matches!(event, KeyEvent(KeyCode::Right, ModKeys::NONE))
        }
        ModeReport::Visual | ModeReport::Normal => {
          matches!(event, KeyEvent(KeyCode::Right, ModKeys::NONE))
            || (self.mode.pending_seq().unwrap(/* always Some on normal mode */).is_empty()
              && matches!(event, KeyEvent(KeyCode::Char('l'), ModKeys::NONE)))
        }
        _ => unimplemented!(),
      }
    } else {
      false
    }
  }

  pub fn should_grab_history(&mut self, cmd: &ViCmd) -> bool {
    cmd.verb().is_none()
      && (cmd
        .motion()
        .is_some_and(|m| matches!(m, MotionCmd(_, Motion::LineUpCharwise)))
        && self.editor.start_of_line() == 0)
      || (cmd
        .motion()
        .is_some_and(|m| matches!(m, MotionCmd(_, Motion::LineDownCharwise)))
        && self.editor.end_of_line() == self.editor.cursor_max())
  }

  pub fn line_text(&mut self) -> String {
    let line = self.editor.to_string();
    let hint = self.editor.get_hint_text();
    if crate::state::read_shopts(|s| s.prompt.highlight) {
      self.highlighter.load_input(&line,self.editor.cursor_byte_pos());
      self.highlighter.highlight();
      let highlighted = self.highlighter.take();
      format!("{highlighted}{hint}")
    } else {
      format!("{line}{hint}")
    }
  }

  pub fn print_line(&mut self) -> ShResult<()> {
    let line = self.line_text();
    let new_layout = self.get_layout(&line);
    if let Some(layout) = self.old_layout.as_ref() {
      self.writer.clear_rows(layout)?;
    }

    self.writer.redraw(&self.prompt, &line, &new_layout)?;

    self.writer.flush_write(&self.mode.cursor_style())?;

    self.old_layout = Some(new_layout);
    Ok(())
  }

  pub fn exec_cmd(&mut self, mut cmd: ViCmd) -> ShResult<()> {
    let mut selecting = false;
    let mut is_insert_mode = false;
    if cmd.is_mode_transition() {
      let count = cmd.verb_count();
      let mut mode: Box<dyn ViMode> = match cmd.verb().unwrap().1 {
        Verb::Change | Verb::InsertModeLineBreak(_) | Verb::InsertMode => {
          is_insert_mode = true;
          Box::new(ViInsert::new().with_count(count as u16))
        }

        Verb::NormalMode => Box::new(ViNormal::new()),

        Verb::ReplaceMode => Box::new(ViReplace::new()),

        Verb::VisualModeSelectLast => {
          if self.mode.report_mode() != ModeReport::Visual {
            self
              .editor
              .start_selecting(SelectMode::Char(SelectAnchor::End));
          }
          let mut mode: Box<dyn ViMode> = Box::new(ViVisual::new());
          std::mem::swap(&mut mode, &mut self.mode);
          self.editor.set_cursor_clamp(self.mode.clamp_cursor());

          return self.editor.exec_cmd(cmd);
        }
        Verb::VisualMode => {
          selecting = true;
          Box::new(ViVisual::new())
        }

        _ => unreachable!(),
      };

      std::mem::swap(&mut mode, &mut self.mode);

      if mode.is_repeatable() {
        self.repeat_action = mode.as_replay();
      }

      // Set cursor clamp BEFORE executing the command so that motions
      // (like EndOfLine for 'A') can reach positions valid in the new mode
      self.editor.set_cursor_clamp(self.mode.clamp_cursor());
      self.editor.exec_cmd(cmd)?;

      if selecting {
        self
          .editor
          .start_selecting(SelectMode::Char(SelectAnchor::End));
      } else {
        self.editor.stop_selecting();
      }
      if is_insert_mode {
        self.editor.mark_insert_mode_start_pos();
      } else {
        self.editor.clear_insert_mode_start_pos();
      }
      return Ok(());
    } else if cmd.is_cmd_repeat() {
      let Some(replay) = self.repeat_action.clone() else {
        return Ok(());
      };
      let ViCmd { verb, .. } = cmd;
      let VerbCmd(count, _) = verb.unwrap();
      match replay {
        CmdReplay::ModeReplay { cmds, mut repeat } => {
          if count > 1 {
            repeat = count as u16;
          }
          for _ in 0..repeat {
            let cmds = cmds.clone();
            for cmd in cmds {
              self.editor.exec_cmd(cmd)?
            }
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
          self.editor.exec_cmd(cmd)?;
        }
        _ => unreachable!("motions should be handled in the other branch"),
      }
      return Ok(());
    } else if cmd.is_motion_repeat() {
      match cmd.motion.as_ref().unwrap() {
        MotionCmd(count, Motion::RepeatMotion) => {
          let Some(motion) = self.repeat_motion.clone() else {
            return Ok(());
          };
          let repeat_cmd = ViCmd {
            register: RegisterName::default(),
            verb: None,
            motion: Some(motion),
            raw_seq: format!("{count};"),
            flags: CmdFlags::empty(),
          };
          return self.editor.exec_cmd(repeat_cmd);
        }
        MotionCmd(count, Motion::RepeatMotionRev) => {
          let Some(motion) = self.repeat_motion.clone() else {
            return Ok(());
          };
          let mut new_motion = motion.invert_char_motion();
          new_motion.0 = *count;
          let repeat_cmd = ViCmd {
            register: RegisterName::default(),
            verb: None,
            motion: Some(new_motion),
            raw_seq: format!("{count},"),
            flags: CmdFlags::empty(),
          };
          return self.editor.exec_cmd(repeat_cmd);
        }
        _ => unreachable!(),
      }
    }

    if cmd.is_repeatable() {
      if self.mode.report_mode() == ModeReport::Visual {
        // The motion is assigned in the line buffer execution, so we also have to
        // assign it here in order to be able to repeat it
        let range = self.editor.select_range().unwrap();
        cmd.motion = Some(MotionCmd(1, Motion::Range(range.0, range.1)))
      }
      self.repeat_action = Some(CmdReplay::Single(cmd.clone()));
    }

    if cmd.is_char_search() {
      self.repeat_motion = cmd.motion.clone()
    }

    self.editor.exec_cmd(cmd.clone())?;

    if self.mode.report_mode() == ModeReport::Visual && cmd.verb().is_some_and(|v| v.1.is_edit()) {
      self.editor.stop_selecting();
      let mut mode: Box<dyn ViMode> = Box::new(ViNormal::new());
      std::mem::swap(&mut mode, &mut self.mode);
    }
    Ok(())
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
  let input = Arc::new(input.to_string());
  let tokens: Vec<Tk> = lex::LexStream::new(input, LexFlags::LEX_UNFINISHED)
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

  while let Some((pos, ch)) = chars.next() {
    match ch {
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
        while let Some((sub_pos, sub_ch)) = chars.next() {
          match sub_ch {
            _ if sub_ch == closing_marker => {
              span_end = sub_pos;
              break;
            }
            _ => body.push(sub_ch),
          }
        }
        let prefix = match ch {
          markers::PROC_SUB => match chars.peek().map(|(_, c)| *c) {
            Some('>') => ">(",
            Some('<') => "<(",
            _ => {
              log::error!("Unexpected character after PROC_SUB marker: expected '>' or '<'");
              "<("
            }
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
    }
  }

  for change in changes.into_iter().rev() {
    let (start, end, replacement) = change;
    annotated.replace_range(start..end, &replacement);
  }

  annotated
}

pub fn get_insertions(input: &str) -> Vec<(usize, Marker)> {
  let input = Arc::new(input.to_string());
  let tokens: Vec<Tk> = lex::LexStream::new(input, LexFlags::LEX_UNFINISHED)
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
    TkRule::Pipe | TkRule::ErrPipe | TkRule::And | TkRule::Or | TkRule::Bg => {
      Some(markers::OPERATOR)
    }
    TkRule::Sep => Some(markers::CMD_SEP),
    TkRule::Redir => Some(markers::REDIRECT),
    TkRule::CasePattern => Some(markers::CASE_PAT),
    TkRule::BraceGrpStart => todo!(),
    TkRule::BraceGrpEnd => todo!(),
    TkRule::Comment => todo!(),
    TkRule::Expanded { exp: _ } | TkRule::EOI | TkRule::SOI | TkRule::Null | TkRule::Str => None,
  }
}

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
            markers::RESET => 0,
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
              markers::RESET => 0,
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
    stack.retain(|(i, m)| *i <= token.span.start && !markers::END_MARKERS.contains(m));

    let Some(ctx) = stack.last() else {
      return false;
    };

    ctx.1 == c
  };

  let mut insertions: Vec<(usize, Marker)> = vec![];

  if token.class != TkRule::Str
    && let Some(marker) = marker_for(&token.class)
  {
    insertions.push((token.span.end, markers::RESET));
    insertions.push((token.span.start, marker));
    return insertions;
  } else if token.flags.contains(TkFlags::IS_SUBSH) {
    let token_raw = token.span.as_str();
    if token_raw.ends_with(')') {
      insertions.push((token.span.end, markers::SUBSH_END));
    }
    insertions.push((token.span.start, markers::SUBSH));
    return insertions;
  }

  let token_raw = token.span.as_str();
  let mut token_chars = token_raw.char_indices().peekable();

  let span_start = token.span.start;

  let mut in_dub_qt = false;
  let mut in_sng_qt = false;
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

  insertions.insert(0, (token.span.end, markers::RESET)); // reset at the end of the token

  while let Some((i, ch)) = token_chars.peek() {
    let index = *i; // we have to dereference this here because rustc is a very pedantic program
    match ch {
      ')' if cmd_sub_depth > 0 || proc_sub_depth > 0 => {
        token_chars.next(); // consume the paren
        if cmd_sub_depth > 0 {
          cmd_sub_depth -= 1;
          if cmd_sub_depth == 0 {
            insertions.push((span_start + index + 1, markers::CMD_SUB_END));
          }
        } else if proc_sub_depth > 0 {
          proc_sub_depth -= 1;
          if proc_sub_depth == 0 {
            insertions.push((span_start + index + 1, markers::PROC_SUB_END));
          }
        }
      }
      '$' if !in_sng_qt => {
        let dollar_pos = index;
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
              while let Some((cur_i, br_ch)) = token_chars.peek() {
                end_pos = *cur_i;
                // TODO: implement better parameter expansion awareness here
                // this is a little too permissive
                if br_ch.is_ascii_alphanumeric()
								|| *br_ch == '_'
								|| *br_ch == '!'
								|| *br_ch == '#'
								|| *br_ch == '%'
								|| *br_ch == ':'
								|| *br_ch == '-'
								|| *br_ch == '+'
								|| *br_ch == '='
								|| *br_ch == '/' // parameter expansion symbols
								|| *br_ch == '?'
                {
                  token_chars.next();
                } else if *br_ch == '}' {
                  token_chars.next(); // consume the closing brace
                  insertions.push((span_start + end_pos + 1, markers::VAR_SUB_END));
                  break;
                } else {
                  // malformed, insert end at current position
                  insertions.push((span_start + end_pos, markers::VAR_SUB_END));
                  break;
                }
              }
            }
            _ if cmd_sub_depth == 0 && (dollar_ch.is_ascii_alphanumeric() || *dollar_ch == '_') => {
              insertions.push((span_start + dollar_pos, markers::VAR_SUB));
              let mut end_pos = dollar_pos + 1;
              // consume the var name
              while let Some((cur_i, var_ch)) = token_chars.peek() {
                if var_ch.is_ascii_alphanumeric() || *var_ch == '_' {
                  end_pos = *cur_i + 1;
                  token_chars.next();
                } else {
                  break;
                }
              }
              insertions.push((span_start + end_pos, markers::VAR_SUB_END));
            }
            _ => { /* Just a plain dollar sign, no marker needed */ }
          }
        }
      }
      ch if cmd_sub_depth > 0 || proc_sub_depth > 0 => {
        // We are inside of a command sub or process sub right now
        // We don't mark any of this text. It will later be recursively annotated
        // by the syntax highlighter
        token_chars.next(); // consume the char with no special handling
      }

      '\\' if !in_sng_qt => {
        token_chars.next(); // consume the backslash
        if token_chars.peek().is_some() {
          token_chars.next(); // consume the escaped char
        }
      }
      '<' | '>' if !in_dub_qt && !in_sng_qt && cmd_sub_depth == 0 && proc_sub_depth == 0 => {
        token_chars.next();
        if let Some((_, proc_sub_ch)) = token_chars.peek()
          && *proc_sub_ch == '('
        {
          proc_sub_depth += 1;
          token_chars.next(); // consume the paren
          if proc_sub_depth == 1 {
            insertions.push((span_start + index, markers::PROC_SUB));
          }
        }
      }
      '"' if !in_sng_qt => {
        if in_dub_qt {
          insertions.push((span_start + *i + 1, markers::STRING_DQ_END));
        } else {
          insertions.push((span_start + *i, markers::STRING_DQ));
        }
        in_dub_qt = !in_dub_qt;
        token_chars.next(); // consume the quote
      }
      '\'' if !in_dub_qt => {
        if in_sng_qt {
          insertions.push((span_start + *i + 1, markers::STRING_SQ_END));
        } else {
          insertions.push((span_start + *i, markers::STRING_SQ));
        }
        in_sng_qt = !in_sng_qt;
        token_chars.next(); // consume the quote
      }
      '[' if !in_dub_qt && !in_sng_qt => {
        token_chars.next(); // consume the opening bracket
        let start_pos = span_start + index;
        let mut is_glob_pat = false;
        const VALID_CHARS: &[char] = &['!', '^', '-'];

        while let Some((cur_i, ch)) = token_chars.peek() {
          if *ch == ']' {
            is_glob_pat = true;
            insertions.push((span_start + *cur_i + 1, markers::RESET));
            insertions.push((span_start + *cur_i, markers::GLOB));
            token_chars.next(); // consume the closing bracket
            break;
          } else if !ch.is_ascii_alphanumeric() && !VALID_CHARS.contains(ch) {
            token_chars.next();
            break;
          } else {
            token_chars.next();
          }
        }

        if is_glob_pat {
          insertions.push((start_pos + 1, markers::RESET));
          insertions.push((start_pos, markers::GLOB));
        }
      }
      '*' | '?' if (!in_dub_qt && !in_sng_qt) => {
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
          insertions.push((span_start + index + offset, markers::RESET));
          insertions.push((span_start + index, markers::GLOB));
        }
      }
      _ => {
        token_chars.next(); // consume the char with no special handling
      }
    }
  }

  sort_insertions(&mut insertions);

  insertions
}
