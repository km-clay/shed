use super::{
  ShResult,
  editcmd::{Anchor, Bound, Cmd, Direction, EditCmd, Motion, To, Verb, Word},
  motion,
  register::RegisterContent,
  status_msg,
};
use crate::verb;

use super::{
  Grapheme, Line, Lines, MotionKind, Pos, SelectMode, killring, ordered, rot13_char,
  toggle_case_char,
};

impl super::LineBuf {
  #[allow(clippy::unit_arg)]
  pub(super) fn exec_verb(&mut self, cmd: &EditCmd) -> ShResult<()> {
    let EditCmd { verb, motion, .. } = cmd;

    let Some(Cmd(_, verb)) = verb else {
      // For verb-less motions in insert mode, merge hint before evaluating
      // so motions like `w` can see into the hint text
      let result = self.eval_motion_with_hint(cmd)?;
      if let Some(motion_kind) = result {
        self.apply_motion_with_hint(motion_kind)?;
      }
      return Ok(());
    };
    let count = motion.as_ref().map(|m| m.0).unwrap_or(1);

    match verb {
      Verb::Delete |
      Verb::Change |
      Verb::Yank /*--------------------*/ => self.delete_change_yank(cmd),
      Verb::Kill /*====================*/ => self.kill(cmd),
      Verb::KillCycle /*---------------*/ => self.kill_cycle(),
      Verb::KillPut /*=================*/ => self.kill_put(),
      Verb::Rot13 /*-------------------*/ => self.rot13(cmd),
      Verb::ReplaceChar(ch) /*=========*/ => self.replace_char(cmd, *ch),
      Verb::ReplaceCharInplace(ch, count) => self.replace_char_inplace(cmd, *ch, *count),
      Verb::ToggleCaseRange /*---------*/ => self.toggle_case_range(cmd),
      Verb::ToLower /*=================*/ => self.make_lower(cmd),
      Verb::ToUpper /*-----------------*/ => self.make_upper(cmd),
      Verb::Capitalize /*==============*/ => self.capitalize(cmd),
      Verb::Undo /*--------------------*/ => self.undo(),
      Verb::Redo /*====================*/ => self.redo(),
      Verb::Put(anchor) /*-------------*/ => self.put(cmd, anchor),
      Verb::SwapVisualAnchor /*========*/ => self.swap_visual_anchor(),
      Verb::JoinLines /*---------------*/ => self.join_lines(cmd, count),
      Verb::InsertChar(ch) /*==========*/ => self.insert_char_verb(cmd, *ch),
      Verb::Indent /*==================*/ => self.indent(cmd),
      Verb::Dedent /*------------------*/ => self.dedent(cmd),
      Verb::Equalize /*================*/ => self.equalize_verb(cmd),
      Verb::PrintPosition /*-----------*/ => self.report_position(),
      Verb::TransposeChar /*===========*/ => self.transpose_char(),
      Verb::TransposeWord /*-----------*/ => self.transpose_word(),
      Verb::ExCmd(_) /*================*/ => self.dispatch_ex_node(cmd),
      Verb::ToggleCaseInplace(c) /*----*/ => self.toggle_case_inplace(*c),
      Verb::InsertModeLineBreak(a) /*==*/ => self.break_line_verb(a),
      Verb::IncrementNumber(n) /*======*/ => Ok(self.adjust_number(*n as i64)),
      Verb::DecrementNumber(n) /*------*/ => Ok(self.adjust_number(-(*n as i64))),
      Verb::Insert(s) /*---------------*/ => Ok(self.insert_str(s)),
      Verb::AcceptLineOrNewline /*-----*/ => Ok(self.insert(Grapheme::from('\n'))),
      Verb::EndOfFile /*===============*/ => Ok(self.lines.clear()),

      Verb::Complete
      | Verb::ExMode
      | Verb::InsertMode
      | Verb::SearchMode
      | Verb::RevSearchMode
      | Verb::NormalMode
      | Verb::VisualMode
      | Verb::VerbatimMode
      | Verb::ReplaceMode
      | Verb::VisualModeLine
      | Verb::VisualModeSelectLast => {
        let Some(motion_kind) = self.eval_motion_with_hint(cmd)? else {
          return Ok(());
        };
        self.apply_motion_with_hint(motion_kind)
      }
      Verb::RepeatLast
      | Verb::RecordMacro
      | Verb::PlayMacro
      | Verb::Interrupt
      | Verb::HistoryDown
      | Verb::HistoryUp
      | Verb::DeleteOrEof
      | Verb::AcceptHint
      | Verb::ClearScreen => {
        log::warn!("{verb:?} should be handled in readline/mod.rs");
        Ok(())
      }
    }
  }
  fn inplace_mutation(&mut self, count: u16, f: impl Fn(&Grapheme) -> Grapheme) {
    let mut first = true;
    for _ in 0..count {
      if first {
        first = false
      } else {
        self.cursor.pos = self.offset_cursor(0, 1);
      }
      let pos = self.cursor.pos;
      let motion = MotionKind::Char {
        start: pos,
        end: pos,
        inclusive: true,
      };
      self.motion_mutation(&motion, &f);
    }
  }
  fn delete_change_yank(&mut self, cmd: &EditCmd) -> ShResult<()> {
    let EditCmd {
      register,
      verb: Some(Cmd(_, verb)),
      ..
    } = cmd
    else {
      return Ok(());
    };
    let Some(motion) = self.eval_motion(cmd)? else {
      return Ok(());
    };
    let content = if *verb == Verb::Yank {
      self.yank_range(&motion)
    } else if *verb == Verb::Change && matches!(motion, MotionKind::Line { .. }) {
      let MotionKind::Line { start, end, .. } = &motion else {
        unreachable!()
      };
      let (insert_target, _) = ordered(*start, *end);
      let n_lines = self.lines.len();
      let content = self.delete_range(&motion);
      self.fix_cursor();
      if n_lines > 1 {
        // clamp insert target at new length
        let insert_at = insert_target.min(self.lines.len());
        self.lines.insert(insert_at, Line::default());
      }
      content
    } else {
      let lines = self.delete_range(&motion);
      self.fix_cursor();
      lines
    };
    let reg_content = match &motion {
      MotionKind::Char { .. } => RegisterContent::Span(content.0),
      MotionKind::Line { .. } => RegisterContent::Line(content.0),
      MotionKind::Block { .. } => RegisterContent::Block(content.0),
    };
    register.write_to_register(reg_content);

    match motion {
      MotionKind::Char { start, end, .. } => {
        let (s, _) = ordered(start, end);
        self.set_cursor(s);
      }
      MotionKind::Line {
        start,
        end,
        inclusive,
      } => {
        let end = if inclusive {
          end
        } else {
          end.saturating_sub(1)
        };
        let (s, _) = ordered(start, end);
        self.set_row(s);
        if *verb == Verb::Change {
          // we've gotta indent
          let (start, _) = self.indent_levels_for_row(self.row());
          let line = self.cur_line_mut();
          let mut col = 0;
          for tab in std::iter::repeat_n(Grapheme::from('\t'), start) {
            line.0.insert(col, tab);
            col += 1;
          }
          self.cursor.pos = self.offset_cursor(0, col as isize);
        }
      }
      MotionKind::Block { start, .. } => {
        let (s, _) = ordered(self.cursor.pos, start);
        self.set_cursor(s);
      }
    }

    Ok(())
  }
  fn kill(&mut self, cmd: &EditCmd) -> ShResult<()> {
    let Some(motion) = self.eval_motion(cmd)? else {
      return Ok(());
    };
    let mut content = self.delete_range(&motion);
    if self.kill_ring.merging
      && let Some(last) = self.kill_ring.kills.back_mut()
    {
      last.append(&mut content);
    } else {
      self.kill_ring.push_back(content);
      if self.kill_ring.len() > killring::MAX_KILL_RING {
        self.kill_ring.pop_front();
      }
    }

    self.kill_ring.merging = true;
    Ok(())
  }
  fn kill_cycle(&mut self) -> ShResult<()> {
    let Some(content) = self.kill_ring.next() else {
      return Ok(());
    };
    let Some(span) = self.kill_ring.kill_cycle_span else {
      return Ok(());
    };
    let total_len: usize =
      content.iter().map(|l| l.len()).sum::<usize>() + content.len().saturating_sub(1); // adds the newlines too

    let (s, e) = ordered(span.0, span.1);
    let _old = self.extract_span((s, e), false);

    self.set_cursor(s);
    self.insert_lines_at(s, content);
    self.cursor.pos = self.offset_cursor_wrapping(0, total_len as isize);
    self.kill_ring.kill_cycle_span = Some((s, self.cursor.pos));
    Ok(())
  }
  fn kill_put(&mut self) -> ShResult<()> {
    let Some(content) = self.kill_ring.next() else {
      return Ok(());
    };
    let paste_pos = self.cursor.pos;
    let total_len: usize =
      content.iter().map(|l| l.len()).sum::<usize>() + content.len().saturating_sub(1); // adds the newlines too
    self.insert_lines_at(paste_pos, content);
    self.cursor.pos = self.offset_cursor_wrapping(0, total_len as isize);
    self.kill_ring.kill_cycle_span = Some((paste_pos, self.cursor.pos));
    Ok(())
  }
  fn capitalize(&mut self, cmd: &EditCmd) -> ShResult<()> {
    // Emacs Alt+C capitalization
    let Some(motion) = self.eval_motion(cmd)? else {
      return Ok(());
    };
    let mut capitalized = false;
    self.motion_mutation(&motion, |gr| {
      let Some(ch) = gr.as_char() else {
        return gr.clone();
      };
      if !ch.is_ascii_alphabetic() {
        return gr.clone();
      }

      if capitalized {
        gr.as_char()
          .map(|c| c.to_ascii_lowercase())
          .map(Grapheme::from)
          .unwrap_or_else(|| gr.clone())
      } else {
        capitalized = true;
        gr.as_char()
          .map(|c| c.to_ascii_uppercase())
          .map(Grapheme::from)
          .unwrap_or_else(|| gr.clone())
      }
    });
    self.apply_motion(motion)?;
    self.cursor.pos = self.cursor.pos.col_add(1);
    Ok(())
  }
  fn replace_char(&mut self, cmd: &EditCmd, ch: char) -> ShResult<()> {
    let Some(motion) = self.eval_motion(cmd)? else {
      return Ok(());
    };
    self.motion_mutation(&motion, |_| Grapheme::from(ch));
    self.move_to_start(motion);
    Ok(())
  }
  fn replace_char_inplace(&mut self, cmd: &EditCmd, ch: char, count: u16) -> ShResult<()> {
    self.inplace_mutation(count, |_| Grapheme::from(ch));
    if let Some(motion) = self.eval_motion_with_hint(cmd)? {
      self.apply_motion_with_hint(motion)?;
    }
    Ok(())
  }
  fn toggle_case_range(&mut self, cmd: &EditCmd) -> ShResult<()> {
    self.eval_map(cmd, toggle_case_char)
  }
  fn rot13(&mut self, cmd: &EditCmd) -> ShResult<()> {
    self.eval_map(cmd, rot13_char)
  }
  fn make_lower(&mut self, cmd: &EditCmd) -> ShResult<()> {
    self.eval_map(cmd, |c| c.to_ascii_lowercase())
  }
  fn make_upper(&mut self, cmd: &EditCmd) -> ShResult<()> {
    self.eval_map(cmd, |c| c.to_ascii_uppercase())
  }
  fn eval_map(&mut self, cmd: &EditCmd, map: fn(char) -> char) -> ShResult<()> {
    let Some(motion) = self.eval_motion(cmd)? else {
      return Ok(());
    };
    self.motion_mutation(&motion, |gr| {
      gr.as_char()
        .map(map)
        .map(Grapheme::from)
        .unwrap_or_else(|| gr.clone())
    });
    self.move_to_start(motion);
    Ok(())
  }
  fn toggle_case_inplace(&mut self, count: u16) -> ShResult<()> {
    self.eval_map_inplace(count, toggle_case_char)
  }
  fn eval_map_inplace(&mut self, count: u16, map: fn(char) -> char) -> ShResult<()> {
    self.inplace_mutation(count, |gr| {
      gr.as_char()
        .map(map)
        .map(Grapheme::from)
        .unwrap_or_else(|| gr.clone())
    });
    self.cursor.pos = self.cursor.pos.col_add(1);
    Ok(())
  }
  fn undo(&mut self) -> ShResult<()> {
    self.edit_stack_op(true)
  }
  fn redo(&mut self) -> ShResult<()> {
    self.edit_stack_op(false)
  }
  fn edit_stack_op(&mut self, is_undo: bool) -> ShResult<()> {
    let (from, to) = if is_undo {
      (&mut self.undo_stack, &mut self.redo_stack)
    } else {
      (&mut self.redo_stack, &mut self.undo_stack)
    };

    if let Some(mut edit) = from.pop() {
      while edit.is_empty() {
        if let Some(next) = from.pop() {
          edit = next;
        } else {
          return Ok(());
        }
      }
      let (lines, cursor) = if is_undo {
        (edit.old.clone(), edit.old_cursor)
      } else {
        (edit.new.clone(), edit.new_cursor)
      };
      self.lines = lines;
      self.cursor.pos = cursor;
      to.push(edit);
    }
    Ok(())
  }
  fn put(&mut self, cmd: &EditCmd, anchor: &Anchor) -> ShResult<()> {
    let EditCmd { register, .. } = cmd;
    let Some(content) = register.read_from_register() else {
      return Ok(());
    };
    let mut effective_anchor = *anchor;
    let mut selection_start: Option<Pos> = None;

    if let Some(motion) = self.select_range() {
      if let Motion::CharRange(s, e) = &motion {
        let (start, _) = ordered(*s, *e);
        selection_start = Some(start);
      }
      let rec_cmd = cmd
        .new_with_verb(Some(verb!(Verb::Delete)))
        .new_with_motion(Some(motion!(motion)));

      self.exec_verb(&rec_cmd)?;
      effective_anchor = Anchor::Before;
    }
    match content {
      RegisterContent::Span(lines) => {
        let move_cursor = lines.len() == 1 && lines[0].len() > 1;
        let content_len: usize = lines.iter().map(|l| l.len()).sum();
        let row = selection_start.map(|p| p.row).unwrap_or_else(|| self.row());
        let col = if let Some(start) = selection_start {
          start
            .col
            .min(self.lines.get(row).map(|l| l.len()).unwrap_or(0))
        } else {
          match effective_anchor {
            Anchor::After => (self.col() + 1).min(self.cur_line().len()),
            Anchor::Before => self.col(),
          }
        };
        let pos = Pos {
          row: self.row(),
          col,
        };
        let start_len = self.lines[row].len();

        self.insert_lines_at(pos, Lines(lines));

        let end_len = self.lines[row].len();
        let mut delta = end_len.saturating_sub(start_len);
        if let Anchor::Before = effective_anchor {
          delta = delta.saturating_sub(1);
        }
        if selection_start.is_some() {
          self.cursor.pos = Pos {
            row,
            col: col + content_len.saturating_sub(1),
          };
        } else if move_cursor {
          self.cursor.pos = self.offset_cursor(0, delta as isize);
        } else if content_len > 1 || effective_anchor == Anchor::After {
          self.cursor.pos = self.offset_cursor(0, 1);
        }
      }
      RegisterContent::Line(lines) => {
        let row = match anchor {
          Anchor::After => self.row() + 1,
          Anchor::Before => self.row(),
        };
        for (i, line) in lines.iter().cloned().enumerate() {
          self.lines.insert(row + i, line);
          self.set_row(row + i);
        }
      }
      RegisterContent::Block(_) => unimplemented!(),
      RegisterContent::Macro(keys) => {
        // Pasting a macro: render to vim-style text and paste as a single
        // span line. Mirrors vim, where macros and text registers are
        // string-equivalent on paste.
        let rendered: String = keys.iter().filter_map(|k| k.as_vim_seq().ok()).collect();
        let mut line = Line::default();
        line.push_str(&rendered);
        let pos = Pos {
          row: self.row(),
          col: match effective_anchor {
            Anchor::After => (self.col() + 1).min(self.cur_line().len()),
            Anchor::Before => self.col(),
          },
        };
        self.insert_lines_at(pos, Lines(vec![line]));
      }
      RegisterContent::Empty => {}
    }
    Ok(())
  }
  fn break_line_verb(&mut self, anchor: &Anchor) -> ShResult<()> {
    match anchor {
      Anchor::After => {
        let row = self.row();
        let target = (row + 1).min(self.lines.len());
        self.lines.insert(target, Line::default());

        let (start, _) = self.indent_levels_for_row(target);
        let line = self.line_mut(target);
        let mut col = 0;
        for tab in std::iter::repeat_n(Grapheme::from('\t'), start) {
          line.insert(0, tab);
          col += 1;
        }

        self.cursor.pos = Pos { row: row + 1, col };
      }
      Anchor::Before => {
        let row = self.row();
        self.lines.insert(row, Line::default());

        let (start, _) = self.indent_levels_for_row(row);
        let line = self.line_mut(row);
        let mut col = 0;
        for tab in std::iter::repeat_n(Grapheme::from('\t'), start) {
          line.insert(0, tab);
          col += 1;
        }

        self.cursor.pos = Pos { row, col };
      }
    }
    Ok(())
  }
  fn swap_visual_anchor(&mut self) -> ShResult<()> {
    let cur_pos = self.cursor.pos;
    let new_anchor;
    {
      let Some(select) = self.select_mode.as_mut() else {
        return Ok(());
      };
      match select {
        SelectMode::Block(select_anchor)
        | SelectMode::Line(select_anchor)
        | SelectMode::Char(select_anchor) => {
          new_anchor = *select_anchor;
          *select_anchor = cur_pos;
        }
      }
    }

    self.set_cursor(new_anchor);
    Ok(())
  }
  fn join_lines(&mut self, cmd: &EditCmd, count: usize) -> ShResult<()> {
    let old_exclusive = self.cursor.exclusive;
    let mut row = self.row();
    let mut count = count;
    if self.select_range().is_some() {
      // Derive the row range to join. Prefer a resolved motion (e.g.
      // when a caller passed Motion::WholeLine); fall back to the
      // visual selection when the EditCmd carries no motion — that's
      // the path taken by ViVisual's `J` arm.
      let (start_row, end_row) = match self.eval_motion(cmd)? {
        Some(MotionKind::Line { start, end, .. }) => (start, end),
        Some(MotionKind::Char { start, end, .. }) => (start.row, end.row),
        Some(MotionKind::Block { .. }) => return Ok(()),
        None => match self.select_range() {
          Some(Motion::CharRange(s, e)) => (s.row, e.row),
          Some(Motion::LineRange(s_addr, e_addr)) => {
            let s = self
              .resolve_line_addr(&s_addr)
              .ok()
              .flatten()
              .unwrap_or(self.row());
            let e = self
              .resolve_line_addr(&e_addr)
              .ok()
              .flatten()
              .unwrap_or(self.row());
            (s, e)
          }
          Some(Motion::BlockRange(s, e)) => (s.row, e.row),
          _ => return Ok(()),
        },
      };
      let (s, e) = ordered(start_row, end_row);
      count = (e - s).max(1);
      row = s;
    }
    self.cursor.exclusive = false;
    for _ in 0..count {
      let target_pos = Pos {
        row,
        col: self.offset_col(row, isize::MAX),
      };
      if row == self.lines.len() - 1 {
        break;
      }

      let mut next_line = self.lines.remove(row + 1).trim_start();
      let this_line = self.line_mut(row);
      let this_has_ws = this_line.0.last().is_some_and(|g| g.is_ws());
      let join_with_space = !this_has_ws && !this_line.is_empty() && !next_line.is_empty();

      if join_with_space {
        next_line.insert_char(0, ' ');
      }

      this_line.append(&mut next_line);
      self.set_cursor(target_pos);
    }

    self.cursor.exclusive = old_exclusive;
    Ok(())
  }
  fn insert_char_verb(&mut self, cmd: &EditCmd, ch: char) -> ShResult<()> {
    self.insert(Grapheme::from(ch));
    if let Some(motion) = self.eval_motion(cmd)? {
      self.apply_motion(motion)?;
    }
    Ok(())
  }
  fn indent(&mut self, cmd: &EditCmd) -> ShResult<()> {
    self.alter_indentation(cmd, true)
  }
  fn dedent(&mut self, cmd: &EditCmd) -> ShResult<()> {
    self.alter_indentation(cmd, false)
  }
  fn alter_indentation(&mut self, cmd: &EditCmd, is_indent: bool) -> ShResult<()> {
    let Some(motion) = self.eval_motion(cmd)? else {
      return Ok(());
    };
    let lines = match motion {
      MotionKind::Char { start, end, .. } => self.line_iter_mut(ordered(start.row, end.row)),
      MotionKind::Line { start, end, .. } => self.line_iter_mut(ordered(start, end)),
      MotionKind::Block { .. } => unimplemented!(),
    };
    let mut col_offset = 0;
    for line in lines {
      if is_indent {
        line.insert(0, Grapheme::from('\t'));
        col_offset += 1;
      } else {
        if line.0.first().is_some_and(|c| c.as_char() == Some('\t')) {
          line.0.remove(0);
          col_offset -= 1;
        }
      }
    }
    self.cursor.pos = self.cursor.pos.col_add_signed(col_offset);
    Ok(())
  }
  fn equalize_verb(&mut self, cmd: &EditCmd) -> ShResult<()> {
    let Some(motion) = self.eval_motion(cmd)? else {
      return Ok(());
    };
    let line_nums = match motion {
      MotionKind::Char { start, end, .. } => {
        let (s, e) = ordered(start.row, end.row);
        s..=e
      }
      MotionKind::Line { start, end, .. } => {
        let (s, e) = ordered(start, end);
        s..=e
      }
      MotionKind::Block { .. } => unimplemented!(),
    };
    let line_nums: Vec<usize> = line_nums.collect();
    self.equalize_rows(line_nums);
    Ok(())
  }
  fn report_position(&mut self) -> ShResult<()> {
    let num_lines = self.lines.len();
    let row = self.row() + 1;
    let col = self.col() + 1;
    let total_graphemes = self.count_graphemes();
    let (left, _) = self.lines.clone().split_lines(self.cursor.pos);
    let total_in_left = left.iter().map(|l| l.len()).sum::<usize>();
    let percentage = if total_graphemes > 0 {
      (total_in_left as f64 / total_graphemes as f64) * 100.0
    } else {
      100.0
    }
    .round() as usize;

    status_msg!("line: {row}/{num_lines}, col: {col} --{percentage}%--");
    Ok(())
  }
  fn transpose_char(&mut self) -> ShResult<()> {
    let Pos { row, col: c_col } = self.cursor.pos;
    let prev_char = Pos {
      row,
      col: c_col.saturating_sub(1),
    };

    let Some(gr) = self.remove_at(prev_char) else {
      return Ok(());
    };

    self.insert_at(self.cursor.pos, gr);
    self.cursor.pos = self.cursor.pos.col_add(1);
    Ok(())
  }
  fn transpose_word(&mut self) -> ShResult<()> {
    // Find the word at/after cursor
    let this_word = if self.cursor_on_ws() {
      let Some(pos) = self.eval_word_motion(
        1,
        &To::Start,
        &Word::Normal,
        &Direction::Forward,
        false,
        false,
      ) else {
        return Ok(());
      };
      let MotionKind::Char { end, .. } = pos else {
        unreachable!()
      };
      end
    } else {
      self.cursor.pos
    };
    let Some(MotionKind::Char {
      start,
      end,
      inclusive,
    }) = self.text_obj_word(this_word, Word::Normal, Bound::Inside)
    else {
      return Ok(());
    };
    let end = if inclusive { end.col_add(1) } else { end };
    let this_word_span = (start, end);

    let back_count = if self.cursor_on_ws() { 1 } else { 2 };

    // Find the previous word
    let prev_word = if let Some(pos) = self.eval_word_motion(
      back_count,
      &To::Start,
      &Word::Normal,
      &Direction::Backward,
      false,
      false,
    ) {
      let MotionKind::Char { end, .. } = pos else {
        unreachable!()
      };
      end
    } else {
      return Ok(());
    };
    let Some(MotionKind::Char {
      start,
      end,
      inclusive,
    }) = self.text_obj_word(prev_word, Word::Normal, Bound::Inside)
    else {
      return Ok(());
    };
    let end = if inclusive { end.col_add(1) } else { end };
    let prev_word_span = (start, end);

    // Bail if the spans overlap or are the same word
    if prev_word_span.0 >= this_word_span.0 {
      return Ok(());
    }

    // Yank both words non-destructively
    let this_content = self.yank_span(this_word_span, false);
    let prev_content = self.yank_span(prev_word_span, false);

    // Compute lengths before we move the content vecs
    let this_content_len: usize =
      this_content.iter().map(|l| l.len()).sum::<usize>() + this_content.len().saturating_sub(1);
    let prev_content_len: usize =
      prev_content.iter().map(|l| l.len()).sum::<usize>() + prev_content.len().saturating_sub(1);

    // Remove later word first so earlier positions stay valid
    self.extract_span(this_word_span, false);
    self.insert_lines_at(this_word_span.0, prev_content);

    // Remove earlier word (its positions are unaffected by later changes)
    self.extract_span(prev_word_span, false);
    self.insert_lines_at(prev_word_span.0, this_content);

    // Cursor goes after the later word, which now holds prev_content.
    // The later word's start shifted by the size difference from
    // replacing the earlier word with different-length content.
    let shift = this_content_len as isize - prev_content_len as isize;
    let new_later_start = Pos {
      row: this_word_span.0.row,
      col: (this_word_span.0.col as isize + shift) as usize,
    };
    self.set_cursor(new_later_start);
    self.cursor.pos = self.offset_cursor_wrapping(0, prev_content_len as isize);
    Ok(())
  }
  fn format_adjusted(word: &str, inc: i64) -> Option<String> {
    if word.starts_with("0x") {
      let body = word.strip_prefix("0x").unwrap();
      let width = body.len();
      let num = i64::from_str_radix(body, 16).ok()?;
      let new_num = num + inc;

      Some(format!("0x{new_num:0>width$x}"))
    } else if word.starts_with("0b") {
      let body = word.strip_prefix("0b").unwrap();
      let width = body.len();
      let num = i64::from_str_radix(body, 2).ok()?;
      let new_num = num + inc;

      Some(format!("0b{new_num:0>width$b}"))
    } else if word.starts_with("0o") {
      let body = word.strip_prefix("0o").unwrap();
      let width = body.len();
      let num = i64::from_str_radix(body, 8).ok()?;
      let new_num = num + inc;

      Some(format!("0o{new_num:0>width$o}"))
    } else if let Ok(num) = word.parse::<i64>() {
      let width = word.len();
      let new_num = num + inc;

      if new_num < 0 {
        let abs = new_num.unsigned_abs();
        let digit_width = if num < 0 { width - 1 } else { width };
        Some(format!("-{abs:0>digit_width$}"))
      } else if num < 0 {
        let digit_width = width - 1;
        Some(format!("{new_num:0>digit_width$}"))
      } else {
        Some(format!("{new_num:0>width$}"))
      }
    } else {
      None
    }
  }
  fn adjust_number(&mut self, inc: i64) {
    let (s, e) = if let Some(range) = self.select_range() {
      match range {
        Motion::CharRange(s, e) => (s, e),
        _ => return,
      }
    } else if let Some((s, e)) = self.number_at_cursor() {
      (s, e)
    } else {
      return;
    };

    let word = self.pos_slice_str(s, e);

    let Some(num_fmt) = Self::format_adjusted(&word, inc) else {
      return;
    };

    self.replace_range((s, e), &num_fmt);
    self.cursor.pos.col -= 1;
  }
}
