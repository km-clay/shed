use super::{
  CharClass, Grapheme, MotionKind, Pos, ShResult, Shed,
  editcmd::{Bound, Cmd, Dest, Direction, EditCmd, LineAddr, Motion, TextObj, To, Verb, Word},
  ordered, status_msg,
};

impl super::LineBuf {
  fn find_delim_match(&mut self) -> Option<MotionKind> {
    let is_opener = |g: &Grapheme| matches!(g.as_char(), Some(c) if "([{<".contains(c));
    let is_closer = |g: &Grapheme| matches!(g.as_char(), Some(c) if ")]}>".contains(c));
    let is_delim = |g: &Grapheme| is_opener(g) || is_closer(g);
    let first = self.scan_forward(is_delim)?;

    let delim_match = if is_closer(self.gr_at(first)?) {
      let mut depth = 0;
      let opener = match self.gr_at(first)?.as_char()? {
        ')' => '(',
        ']' => '[',
        '}' => '{',
        '>' => '<',
        _ => unreachable!(),
      };
      self.scan_backward_from(first, |g| {
        if g.as_char() == self.gr_at(first).and_then(Grapheme::as_char) {
          depth += 1;
        } else if g.as_char() == Some(opener) {
          depth -= 1;
        }
        depth == 0
      })?
    } else if is_opener(self.gr_at(first)?) {
      let mut depth = 0;
      let closer = match self.gr_at(first)?.as_char()? {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        '<' => '>',
        _ => unreachable!(),
      };
      self.scan_forward_from(first, |g| {
        if g.as_char() == self.gr_at(first).and_then(Grapheme::as_char) {
          depth += 1;
        } else if g.as_char() == Some(closer) {
          depth -= 1;
        }
        depth == 0
      })?
    } else {
      unreachable!()
    };

    Some(MotionKind::Char {
      start: self.cursor.pos,
      end: delim_match,
      inclusive: true,
    })
  }
  /// Given a `LineAddr`, resolve it to an absolute line number.
  ///
  /// This is used for commands like `:3` or `:'a` where we need to convert the address into a line number in the buffer.
  pub fn resolve_line_addr(&self, addr: &LineAddr) -> ShResult<Option<usize>> {
    match addr {
      LineAddr::Number(n) => Ok(Some(
        (n.saturating_sub(1)).min(self.lines.len().saturating_sub(1)),
      )),
      LineAddr::Current => Ok(Some(self.row())),
      LineAddr::Last => Ok(Some(self.lines.len().saturating_sub(1))),
      LineAddr::Offset(i) => Ok(Some(self.row().saturating_add_signed(*i))),
      dir @ (LineAddr::Pattern(re) | LineAddr::PatternRev(re)) => {
        let reg = match Shed::meta_mut(|m| m.get_regex(re)) {
          Ok(re) => re,
          Err(e) => {
            status_msg!("{e}");
            return Ok(None);
          }
        };
        let off = if matches!(dir, LineAddr::Pattern(_)) {
          1
        } else {
          -1
        };
        let inc_acc =
          |acc: usize| (acc as isize + off).rem_euclid(self.lines.len() as isize) as usize;
        let mut acc = inc_acc(self.row());

        while let Some(row) = self.get_row(acc) {
          let row_str = row.to_string();
          if reg.is_match(&row_str) {
            return Ok(Some(acc));
          }

          if acc == self.row() {
            break;
          }
          acc = inc_acc(acc);
        }

        Ok(None)
      }
      LineAddr::Mark(ch) => {
        match ch {
          anchor @ ('<' | '>') => {
            let Some(select_range) = self.select_range() else {
              return Ok(None);
            };
            let (s, e) = match select_range {
              Motion::CharRange(s, e) => (s.row, e.row),
              Motion::LineRange(s, e) => {
                let Some(s) = self.resolve_line_addr(&s)? else {
                  return Ok(None);
                };
                let Some(e) = self.resolve_line_addr(&e)? else {
                  return Ok(None);
                };
                (s, e)
              }
              _ => unreachable!(),
            };
            match anchor {
              '<' => Ok(Some(s)),
              '>' => Ok(Some(e)),
              _ => unreachable!(),
            }
          }
          _ => Ok(None), // TODO: implement marks
        }
      }
    }
  }

  fn search(&mut self, motion: &Motion, save: bool, count: usize) -> Option<MotionKind> {
    let Motion::Search(pat, dir) = motion else {
      return None;
    };
    let re = match Shed::meta_mut(|m| m.get_regex(pat)) {
      Ok(re) => re,
      Err(e) => {
        status_msg!("{e}");
        return None;
      }
    };
    let buf = self.to_string();
    let mut offset = self.pos_to_byte(self.cursor.pos)?;
    let mut target_byte = None;

    for _ in 0..count {
      target_byte = match dir {
        Direction::Forward => re
          .find_at(&buf, offset + 1)
          .or_else(|| re.find(&buf))
          .map(|m| m.start()),
        Direction::Backward => {
          let matches: Vec<_> = re.find_iter(&buf).collect();
          matches
            .iter()
            .rev()
            .find(|m| m.start() < offset)
            .or_else(|| matches.last())
            .map(regex::Match::start)
        }
      };
      offset = target_byte?;
    }

    target_byte.and_then(|b| self.byte_to_pos(b)).map(|target| {
      if save {
        self.last_search = Some(motion.clone());
      }
      MotionKind::Char {
        start: self.cursor.pos,
        end: target,
        inclusive: false,
      }
    })
  }
  /// Wrapper for `eval_motion_inner` that calls it with `check_hint: false`
  pub(super) fn eval_motion(&mut self, cmd: &EditCmd) -> ShResult<Option<MotionKind>> {
    self.eval_motion_inner(cmd, false)
  }
  pub(super) fn eval_motion_with_hint(&mut self, cmd: &EditCmd) -> ShResult<Option<MotionKind>> {
    self.eval_motion_inner(cmd, true)
  }
  #[expect(clippy::too_many_lines)]
  fn eval_motion_inner(&mut self, cmd: &EditCmd, check_hint: bool) -> ShResult<Option<MotionKind>> {
    let EditCmd { verb, motion, .. } = cmd;
    let Some(Cmd(count, motion)) = motion.as_ref() else {
      return Ok(None);
    };
    let mut motion = motion.clone();

    if let Motion::Selection(mode) = motion {
      motion = self.evaluate_select_shape(&mode);
    }

    let eval = |this: &mut Self| -> ShResult<Option<MotionKind>> {
      let kind = match &motion {
        Motion::WholeLine => {
          let start = this.row();
          let end =
            (this.row() + (count.saturating_sub(1))).min(this.lines.len().saturating_sub(1));
          Some(MotionKind::Line {
            start,
            end,
            inclusive: true,
          })
        }
        Motion::TextObj(text_obj) => this.dispatch_text_obj(*text_obj),
        Motion::EndOfLastWord => {
          let row = this.row() + (count.saturating_sub(1));
          let line = this.line_mut(row);
          let mut target = Pos { row, col: 0 };
          for (i, gr) in line.0.iter().enumerate() {
            if !gr.is_ws() {
              target.col = i;
            }
          }

          (target != this.cursor.pos).then_some(MotionKind::Char {
            start: this.cursor.pos,
            end: target,
            inclusive: true,
          })
        }
        Motion::StartOfFirstWord => {
          let row = this.row() + count.saturating_sub(1);
          let mut target = Pos { row, col: 0 };
          let line = this.line(row);
          for (i, gr) in line.0.iter().enumerate() {
            target.col = i;
            if !gr.is_ws() {
              break;
            }
          }

          (target != this.cursor.pos).then_some(MotionKind::Char {
            start: this.cursor.pos,
            end: target,
            inclusive: true,
          })
        }
        dir @ (Motion::StartOfLine | Motion::EndOfLine) => {
          let (inclusive, off) = match dir {
            Motion::StartOfLine => (false, isize::MIN),
            Motion::EndOfLine => (true, isize::MAX),
            _ => unreachable!(),
          };
          let row_offset = count.saturating_sub(1);
          let target = this.offset_cursor(row_offset as isize, off);
          (target != this.cursor.pos).then_some(MotionKind::Char {
            start: this.cursor.pos,
            end: target,
            inclusive,
          })
        }
        Motion::WordMotion(to, word, dir) => {
          // 'cw' is a weird case
          // if you are on the word's left boundary, it will not delete whitespace after
          // the end of the word
          let ignore_trailing_ws = matches!(verb, Some(Cmd(_, Verb::Change)),)
            && matches!(
              motion,
              Motion::WordMotion(To::Start, _, Direction::Forward,)
            );
          let inclusive = verb.is_none();

          Some(this.eval_word_motion(*count, *to, *word, *dir, ignore_trailing_ws, inclusive))
        }
        Motion::CharSearch(dir, dest, char) => {
          let off = this.search_char(*dir, *dest, char, *count);
          let target = this.offset_cursor(0, off);
          let inclusive = matches!(dir, Direction::Forward);
          (target != this.cursor.pos).then_some(MotionKind::Char {
            start: this.cursor.pos,
            end: target,
            inclusive,
          })
        }
        dir @ (Motion::BackwardChar
        | Motion::ForwardChar
        | Motion::BackwardCharForced
        | Motion::ForwardCharForced) => {
          let (off, wrap) = match dir {
            Motion::BackwardChar => (-(*count as isize), false),
            Motion::ForwardChar => (*count as isize, false),
            Motion::BackwardCharForced => (-(*count as isize), true),
            Motion::ForwardCharForced => (*count as isize, true),
            _ => unreachable!(),
          };
          let target = if wrap {
            this.offset_cursor_wrapping(0, off)
          } else {
            this.offset_cursor(0, off)
          };

          (target != this.cursor.pos).then_some(MotionKind::Char {
            start: this.cursor.pos,
            end: target,
            inclusive: false,
          })
        }
        dir @ (Motion::LineDown | Motion::LineUp) => {
          let off = match dir {
            Motion::LineUp => -(*count as isize),
            Motion::LineDown => *count as isize,
            _ => unreachable!(),
          };
          if verb.is_some() {
            let row = this.row();
            let target_row = this.offset_row(off);
            let (s, e) = ordered(row, target_row);
            Some(MotionKind::Line {
              start: s,
              end: e,
              inclusive: true,
            })
          } else {
            if this.saved_col.is_none() {
              this.saved_col = Some(this.calc_cursor_display_col());
            }
            let row = this.offset_row(off);
            let limit = if this.cursor.exclusive {
              this.lines[row].len().saturating_sub(1)
            } else {
              this.lines[row].len()
            };
            let target_col = this.saved_col.unwrap();
            let col = this.display_col_to_index(row, target_col).min(limit);
            let target = Pos { row, col };
            (target != this.cursor.pos).then_some(MotionKind::Char {
              start: this.cursor.pos,
              end: target,
              inclusive: true,
            })
          }
        }
        dir @ (Motion::EndOfBuffer | Motion::StartOfBuffer) => {
          let off = match dir {
            Motion::StartOfBuffer => isize::MIN,
            Motion::EndOfBuffer => isize::MAX,
            _ => unreachable!(),
          };
          if verb.is_some() {
            let row = this.row();
            let target_row = this.offset_row(off);
            let (s, e) = ordered(row, target_row);
            Some(MotionKind::Line {
              start: s,
              end: e,
              inclusive: true,
            })
          } else {
            let target = this.offset_cursor(off, 0);
            (target != this.cursor.pos).then_some(MotionKind::Char {
              start: this.cursor.pos,
              end: target,
              inclusive: true,
            })
          }
        }
        Motion::ToColumn => {
          let row = this.row();
          let end = Pos {
            row,
            col: count.saturating_sub(1),
          };
          Some(MotionKind::Char {
            start: this.cursor.pos,
            end,
            inclusive: end > this.cursor.pos,
          })
        }

        Motion::Search(..) => this.search(&motion, true, *count),

        Motion::RepeatSearch => {
          if let Some(search) = this.last_search.clone() {
            this.search(&search, false, *count)
          } else {
            None
          }
        }

        Motion::RepeatSearchRev => {
          if let Some(search) = &this.last_search {
            let rev_search = match search {
              Motion::Search(pat, dir) => {
                let rev_dir = match dir {
                  Direction::Forward => Direction::Backward,
                  Direction::Backward => Direction::Forward,
                };
                Motion::Search(pat.clone(), rev_dir)
              }
              _ => unreachable!(),
            };
            this.search(&rev_search, false, *count)
          } else {
            None
          }
        }

        Motion::ToDelimMatch => this.find_delim_match(),
        Motion::ToParen(direction) | Motion::ToBrace(direction) => {
          let (opener, closer) = match motion {
            Motion::ToParen(_) => ('(', ')'),
            Motion::ToBrace(_) => ('{', '}'),
            _ => unreachable!(),
          };
          match direction {
            Direction::Forward => {
              let mut depth = 0;
              let Some(target_pos) = this.scan_forward(|g| {
                if g.as_char() == Some(opener) {
                  depth += 1;
                }
                if g.as_char() == Some(closer) {
                  depth -= 1;
                  if depth <= 0 {
                    return true;
                  }
                }
                false
              }) else {
                return Ok(None);
              };
              return Ok(Some(MotionKind::Char {
                start: this.cursor.pos,
                end: target_pos,
                inclusive: true,
              }));
            }
            Direction::Backward => {
              let mut depth = 0;
              let Some(target_pos) = this.scan_backward(|g| {
                if g.as_char() == Some(closer) {
                  depth += 1;
                }
                if g.as_char() == Some(opener) {
                  depth -= 1;
                  if depth <= 0 {
                    return true;
                  }
                }
                false
              }) else {
                return Ok(None);
              };
              return Ok(Some(MotionKind::Char {
                start: this.cursor.pos,
                end: target_pos,
                inclusive: true,
              }));
            }
          }
        }

        Motion::CharRange(s, e) => {
          let (s, e) = ordered(*s, *e);
          Some(MotionKind::Char {
            start: s,
            end: e,
            inclusive: true,
          })
        }
        Motion::Line(l) => {
          let Some(l) = this.resolve_line_addr(l)? else {
            return Ok(None);
          };
          Some(MotionKind::Line {
            start: l,
            end: l + 1,
            inclusive: false,
          })
        }
        Motion::LineRange(s, e) => {
          let Some(s) = this.resolve_line_addr(s)? else {
            return Ok(None);
          };
          let Some(e) = this.resolve_line_addr(e)? else {
            return Ok(None);
          };
          let (s, e) = ordered(s, e);
          Some(MotionKind::Line {
            start: s,
            end: e,
            inclusive: true,
          })
        }
        Motion::BlockRange(s, e) => {
          let (s, e) = ordered(*s, *e);
          Some(MotionKind::Block { start: s, end: e })
        }
        dir @ (Motion::HalfScreenUp | Motion::HalfScreenDown) => {
          let off = match dir {
            Motion::HalfScreenUp => -(this.get_viewport_height() as isize / 2),
            Motion::HalfScreenDown => this.get_viewport_height() as isize / 2,
            _ => unreachable!(),
          };
          let row = this.row();
          let target_row = this.offset_row(off);
          Some(MotionKind::Line {
            start: target_row,
            end: row,
            inclusive: false,
          })
        }
        Motion::RepeatMotion | Motion::RepeatMotionRev | Motion::Null => None,
        Motion::Selection(_) => {
          unreachable!()
        }
      };
      Ok(kind)
    };

    if check_hint {
      self.with_hint(eval)
    } else {
      eval(self)
    }
  }
  /// Wrapper for `apply_motion_inner` that calls it with `accept_hint: false`
  pub(super) fn apply_motion(&mut self, motion: &MotionKind) -> ShResult<()> {
    self.apply_motion_inner(motion, false)
  }
  pub(super) fn apply_motion_with_hint(&mut self, motion: &MotionKind) -> ShResult<()> {
    self.apply_motion_inner(motion, true)
  }
  fn apply_motion_inner(&mut self, motion: &MotionKind, accept_hint: bool) -> ShResult<()> {
    let apply = |this: &mut Self| -> ShResult<()> {
      match motion {
        MotionKind::Char { end, .. } => {
          this.set_cursor(*end);
        }
        MotionKind::Line { start, .. } => {
          this.set_row(*start);
        }
        MotionKind::Block { .. } => unimplemented!(),
      }
      Ok(())
    };

    if accept_hint {
      self.with_hint(apply)
    } else {
      apply(self)
    }
  }
  pub(super) fn motion_mutation(
    &mut self,
    motion: &MotionKind,
    mut f: impl FnMut(&Grapheme) -> Grapheme,
  ) {
    match motion {
      MotionKind::Char {
        start,
        end,
        inclusive,
      } => {
        let (s, e) = ordered(start, end);
        if s.row == e.row {
          let range = if *inclusive {
            s.col..e.col + 1
          } else {
            s.col..e.col
          };
          for col in range {
            if col >= self.lines[s.row].len() {
              break;
            }
            self.lines[s.row][col] = f(&self.lines[s.row][col]);
          }
          return;
        }
        let end = if *inclusive { e.col + 1 } else { e.col };

        for col in s.col..self.lines[s.row].len() {
          self.lines[s.row][col] = f(&self.lines[s.row][col]);
        }
        for row in s.row + 1..e.row {
          for col in 0..self.lines[row].len() {
            self.lines[row][col] = f(&self.lines[row][col]);
          }
        }
        for col in 0..end {
          if col >= self.lines[e.row].len() {
            break;
          }
          self.lines[e.row][col] = f(&self.lines[e.row][col]);
        }
      }
      MotionKind::Line {
        start,
        end,
        inclusive,
      } => {
        let end = if *inclusive {
          *end
        } else {
          end.saturating_sub(1)
        };
        for row in *start..=end {
          let line = self.line_mut(row);
          for col in 0..line.len() {
            line[col] = f(&line[col]);
          }
        }
      }
      MotionKind::Block { .. } => unimplemented!(),
    }
  }
  fn search_char(&self, dir: Direction, dest: Dest, char: &Grapheme, count: usize) -> isize {
    let mut off: isize = 0;
    'outer: for it in 0..count {
      let pos = self.offset_cursor(0, off);
      match dir {
        Direction::Forward => {
          let slice = self.line_from_pos(pos);
          for (i, gr) in slice.iter().enumerate().skip(1) {
            let i = i as isize;
            if gr != char {
              continue;
            }
            match dest {
              Dest::On => {
                off += i;
                continue 'outer;
              }
              Dest::Before => {
                if it == count.saturating_sub(1) {
                  off += (i - 1).max(0);
                } else {
                  // there are more iterations to go
                  // if we land before, we will stop in the same
                  // place next time around
                  off += i;
                }
                continue 'outer;
              }
            }
          }
          return 0; // not found
        }
        Direction::Backward => {
          let slice = self.line_to_pos(pos);
          for (i, gr) in slice.iter().rev().enumerate() {
            let i = i as isize;
            if gr != char {
              continue;
            }
            match dest {
              Dest::On => {
                off -= i + 1;
                continue 'outer;
              }
              Dest::Before => {
                if it == count.saturating_sub(1) {
                  off -= i;
                } else {
                  // there are more iterations to go
                  // if we land before, we will stop in the same
                  // place next time around
                  off -= i + 1;
                }
                continue 'outer;
              }
            }
          }
          return 0; // not found
        }
      }
    }

    off
  }
  pub(super) fn eval_word_motion(
    &self,
    count: usize,
    to: To,
    word: Word,
    dir: Direction,
    ignore_trailing_ws: bool,
    mut inclusive: bool,
  ) -> MotionKind {
    let mut target = self.cursor.pos;

    for i in 0..count {
      let last = i == count - 1;
      let iws = ignore_trailing_ws && last; // only ignore on the last iteration
      match (to, dir) {
        (To::Start, Direction::Forward) => {
          // 'w' is a special snowflake motion so we need these two extra arguments
          // if we hit the ignore_trailing_ws path in the function,
          // inclusive is flipped to true.
          target = self
            .word_motion_w(word, target, iws, &mut inclusive)
            .unwrap_or_else(|| {
              // we set inclusive to true so that we catch the entire word
              // instead of ignoring the last character
              inclusive = true;
              Pos::MAX
            });
        }
        (To::End, Direction::Forward) => {
          inclusive = true;
          target = self.word_motion_e(word, target).unwrap_or(Pos::MAX);
        }
        (To::Start, Direction::Backward) => {
          target = self.word_motion_b(word, target).unwrap_or(Pos::MIN);
        }
        (To::End, Direction::Backward) => {
          inclusive = true;
          target = self.word_motion_ge(word, target).unwrap_or(Pos::MIN);
        }
      }
    }

    target.clamp_row(&self.lines);
    target.clamp_col(&self.lines[target.row].0, self.cursor.exclusive);

    MotionKind::Char {
      start: self.cursor.pos,
      end: target,
      inclusive,
    }
  }
  fn word_motion_w(
    &self,
    word: Word,
    start: Pos,
    ignore_trailing_ws: bool,
    inclusive: &mut bool,
  ) -> Option<Pos> {
    use CharClass as C;

    // get our iterator of char classes
    // we dont actually care what the chars are
    // just what they look like.
    // we are going to use .find() a lot to advance the iterator
    let mut classes = self.char_classes_forward_from(start).peekable();

    match word {
      Word::Big => {
        if let Some((_, C::Whitespace)) = classes.peek() {
          // we are on whitespace. advance to the next non-ws char class
          return classes.find(|(_, c)| !c.is_ws()).map(|(p, _)| p);
        }

        let last_non_ws = classes.find(|(_, c)| c.is_ws());
        if ignore_trailing_ws {
          return last_non_ws.map(|(p, _)| p);
        }
        classes.find(|(_, c)| !c.is_ws()).map(|(p, _)| p)
      }
      Word::Normal => {
        if let Some((_, C::Whitespace)) = classes.peek() {
          // we are on whitespace. advance to the next non-ws char class
          return classes.find(|(_, c)| !c.is_ws()).map(|(p, _)| p);
        }

        // go forward until we find some char class that isnt this one
        let mut last = classes.next()?;
        let first_c = last.1;
        while let Some((p, c)) = classes.next() {
          match c {
            C::Whitespace => {
              if ignore_trailing_ws {
                *inclusive = true;
                return Some(last.0);
              }
              break;
            }
            c if !c.is_other_class_or_ws(first_c) => {
              last = (p, c);
            }
            _ => return Some(p),
          }
        }

        // we found whitespace previously, look for the next non-whitespace char class
        classes.find(|(_, c)| !c.is_ws()).map(|(p, _)| p)
      }
    }
  }
  fn word_motion_b(&self, word: Word, start: Pos) -> Option<Pos> {
    use CharClass as C;
    // get our iterator again
    let mut classes = self.char_classes_backward_from(start).peekable();

    match word {
      Word::Big => {
        classes.next();
        // for 'b', we handle starting on whitespace differently than 'w'
        // we don't return immediately if find() returns Some() here.
        let first_non_ws = if let Some((_, C::Whitespace)) = classes.peek() {
          // we use find() to advance the iterator as usual
          // but we can also be clever and use the question mark
          // to return early if we don't find a word backwards
          classes.find(|(_, c)| !c.is_ws())?
        } else {
          classes.next()?
        };

        // ok now we are off that whitespace
        // now advance backwards until we find more whitespace, or next() is None

        let mut last = first_non_ws;
        while let Some((_, c)) = classes.peek() {
          if c.is_ws() {
            break;
          }
          last = classes.next()?;
        }
        Some(last.0)
      }
      Word::Normal => {
        classes.next();
        let first_non_ws = if let Some((_, C::Whitespace)) = classes.peek() {
          classes.find(|(_, c)| !c.is_ws())?
        } else {
          classes.next()?
        };

        // ok, off the whitespace
        // now advance until we find any different char class at all
        let mut last = first_non_ws;
        while let Some((_, c)) = classes.peek() {
          if c.is_other_class(last.1) {
            break;
          }
          last = classes.next()?;
        }

        Some(last.0)
      }
    }
  }
  fn word_motion_e(&self, word: Word, start: Pos) -> Option<Pos> {
    use CharClass as C;
    let mut classes = self.char_classes_forward_from(start).peekable();

    match word {
      Word::Big => {
        classes.next(); // unconditionally skip first position for 'e'
        let first_non_ws = if let Some((_, C::Whitespace)) = classes.peek() {
          classes.find(|(_, c)| !c.is_ws())?
        } else {
          classes.next()?
        };

        let mut last = first_non_ws;
        while let Some((_, c)) = classes.peek() {
          if c.is_ws() {
            return Some(last.0);
          }
          last = classes.next()?;
        }
        None
      }
      Word::Normal => {
        classes.next();
        let first_non_ws = if let Some((_, C::Whitespace)) = classes.peek() {
          classes.find(|(_, c)| !c.is_ws())?
        } else {
          classes.next()?
        };

        let mut last = first_non_ws;
        while let Some((_, c)) = classes.peek() {
          if c.is_other_class_or_ws(first_non_ws.1) {
            return Some(last.0);
          }
          last = classes.next()?;
        }
        None
      }
    }
  }
  fn word_motion_ge(&self, word: Word, start: Pos) -> Option<Pos> {
    use CharClass as C;
    let mut classes = self.char_classes_backward_from(start).peekable();

    match word {
      Word::Big => {
        classes.next(); // unconditionally skip first position for 'ge'
        if matches!(classes.peek(), Some((_, c)) if !c.is_ws()) {
          classes.find(|(_, c)| c.is_ws());
        }

        classes.find(|(_, c)| !c.is_ws()).map(|(p, _)| p)
      }
      Word::Normal => {
        classes.next();
        if let Some((_, C::Whitespace)) = classes.peek() {
          return classes.find(|(_, c)| !c.is_ws()).map(|(p, _)| p);
        }

        let cur_class = classes.peek()?.1;
        let bound = classes.find(|(_, c)| c.is_other_class(cur_class))?;

        if bound.1.is_ws() {
          classes.find(|(_, c)| !c.is_ws()).map(|(p, _)| p)
        } else {
          Some(bound.0)
        }
      }
    }
  }
  fn text_obj_paragraph(&mut self, from: Pos, bound: Bound) -> Option<MotionKind> {
    let is_blank = |i: usize| self.lines.get(i).is_some_and(|l| l.is_empty());
    let this_line = from.row;
    let kind = is_blank(this_line);
    let around = matches!(bound, Bound::Around);

    let mut lo = this_line;
    let mut hi = this_line;

    while lo > 0 && is_blank(lo - 1) == kind {
      lo -= 1;
    }
    while hi + 1 < self.lines.len() && is_blank(hi + 1) == kind {
      hi += 1;
    }

    if around {
      if hi + 1 < self.lines.len() {
        let opp = is_blank(hi + 1);
        while hi + 1 < self.lines.len() && is_blank(hi + 1) == opp {
          hi += 1;
        }
      } else if lo > 0 {
        let opp = is_blank(lo - 1);
        while lo > 0 && is_blank(lo - 1) == opp {
          lo -= 1;
        }
      }
    }

    Some(MotionKind::Line {
      start: lo,
      end: hi,
      inclusive: true,
    })
  }

  fn paragraph_motion(&self, dir: Direction) -> Option<MotionKind> {
    let is_blank = |i: usize| self.lines.get(i).is_some_and(|l| l.is_empty());
    let cur = self.row();
    let last = self.lines.len().saturating_sub(1);

    let target = match dir {
      // first blank line below the cursor, else the end of the buffer
      Direction::Forward => match ((cur + 1)..self.lines.len()).find(|&i| is_blank(i)) {
        Some(row) => Pos { row, col: 0 },
        None => Pos {
          row: last,
          col: self.lines.get(last).map_or(0, |l| l.len()),
        },
      },
      // last blank line above the cursor, else the top of the buffer
      Direction::Backward => Pos {
        row: (0..cur).rev().find(|&i| is_blank(i)).unwrap_or(0),
        col: 0,
      },
    };

    (target != self.cursor.pos).then_some(MotionKind::Char {
      start: self.cursor.pos,
      end: target,
      inclusive: false,
    })
  }

  fn dispatch_text_obj(&mut self, obj: TextObj) -> Option<MotionKind> {
    match obj {
      // text structures
      TextObj::Word(word, bound) => self.text_obj_word(self.cursor.pos, word, bound),
      TextObj::WholeParagraph(bound) => self.text_obj_paragraph(self.cursor.pos, bound),
      TextObj::Paragraph(dir) => self.paragraph_motion(dir),
      TextObj::Sentence(_) | TextObj::WholeSentence(_) => {
        log::warn!("{obj:?} text objects are not implemented yet");
        None
      }

      // quote stuff
      TextObj::DoubleQuote(bound) | TextObj::SingleQuote(bound) | TextObj::BacktickQuote(bound) => {
        self.text_obj_quote(obj, bound)
      }

      // delimited blocks
      TextObj::Paren(bound)
      | TextObj::Bracket(bound)
      | TextObj::Brace(bound)
      | TextObj::Angle(bound) => self.text_obj_delim(obj, bound),
    }
  }
  #[expect(clippy::too_many_lines)]
  pub(super) fn text_obj_word(
    &mut self,
    from: Pos,
    word: Word,
    bound: Bound,
  ) -> Option<MotionKind> {
    use CharClass as C;
    let mut fwd_classes = self.char_classes_forward_from(from);
    let first_class = fwd_classes.next()?;
    match first_class {
      (pos, C::Whitespace) => match bound {
        Bound::Inside => {
          let mut fwd_classes = self.char_classes_forward_from(pos).peekable();
          let mut bkwd_classes = self.char_classes_backward_from(pos).peekable();
          let mut first = (pos, C::Whitespace);
          let mut last = (pos, C::Whitespace);
          while let Some((_, c)) = bkwd_classes.peek() {
            if !c.is_ws() {
              break;
            }
            first = bkwd_classes.next()?;
          }

          while let Some((_, c)) = fwd_classes.peek() {
            if !c.is_ws() {
              break;
            }
            last = fwd_classes.next()?;
          }

          Some(MotionKind::Char {
            start: first.0,
            end: last.0,
            inclusive: true,
          })
        }
        Bound::Around => {
          let mut fwd_classes = self.char_classes_forward_from(pos).peekable();
          let mut bkwd_classes = self.char_classes_backward_from(pos).peekable();
          let mut first = (pos, C::Whitespace);
          let mut last = (pos, C::Whitespace);
          while let Some((_, cl)) = bkwd_classes.peek() {
            if !cl.is_ws() {
              break;
            }
            first = bkwd_classes.next()?;
          }

          while let Some((_, cl)) = fwd_classes.peek() {
            if !cl.is_ws() {
              break;
            }
            last = fwd_classes.next()?;
          }
          let word_class = fwd_classes.next()?.1;
          while let Some((_, cl)) = fwd_classes.peek() {
            match word {
              Word::Big => {
                if cl.is_ws() {
                  break;
                }
              }
              Word::Normal => {
                if cl.is_other_class_or_ws(word_class) {
                  break;
                }
              }
            }
            last = fwd_classes.next()?;
          }

          Some(MotionKind::Char {
            start: first.0,
            end: last.0,
            inclusive: true,
          })
        }
      },
      (pos, c) => {
        let break_cond = |cl: &C, c: &C| -> bool {
          match word {
            Word::Big => cl.is_ws(),
            Word::Normal => cl.is_other_class(*c),
          }
        };
        match bound {
          Bound::Inside => {
            let mut fwd_classes = self.char_classes_forward_from(pos).peekable();
            let mut bkwd_classes = self.char_classes_backward_from(pos).peekable();
            let mut first = (pos, c);
            let mut last = (pos, c);

            while let Some((_, cl)) = bkwd_classes.peek() {
              if break_cond(cl, &c) {
                break;
              }
              first = bkwd_classes.next()?;
            }

            while let Some((_, cl)) = fwd_classes.peek() {
              if break_cond(cl, &c) {
                break;
              }
              last = fwd_classes.next()?;
            }

            Some(MotionKind::Char {
              start: first.0,
              end: last.0,
              inclusive: true,
            })
          }
          Bound::Around => {
            let mut fwd_classes = self.char_classes_forward_from(pos).peekable();
            let mut bkwd_classes = self.char_classes_backward_from(pos).peekable();
            let mut first = (pos, c);
            let mut last = (pos, c);

            while let Some((_, cl)) = bkwd_classes.peek() {
              if break_cond(cl, &c) {
                break;
              }
              first = bkwd_classes.next()?;
            }

            while let Some((_, cl)) = fwd_classes.peek() {
              if break_cond(cl, &c) {
                break;
              }
              last = fwd_classes.next()?;
            }

            // Include trailing whitespace
            while let Some((_, cl)) = fwd_classes.peek() {
              if !cl.is_ws() {
                break;
              }
              last = fwd_classes.next()?;
            }

            Some(MotionKind::Char {
              start: first.0,
              end: last.0,
              inclusive: true,
            })
          }
        }
      }
    }
  }
  fn text_obj_quote(&mut self, obj: TextObj, bound: Bound) -> Option<MotionKind> {
    let q_ch = match obj {
      TextObj::DoubleQuote(_) => '"',
      TextObj::SingleQuote(_) => '\'',
      TextObj::BacktickQuote(_) => '`',
      _ => unreachable!(),
    };

    // Quote text objects operate within a single line. Pair the quotes on the
    // cursor's line left to right and pick the first pair whose closing quote
    // is at or after the cursor. This resolves correctly whether the cursor
    // sits before, inside, or on either quote of the pair, unlike a directional
    // scan that mistakes the closing quote for an opener when the cursor is
    // past it.
    let row = self.cursor.pos.row;
    let cur_col = self.cursor.pos.col;
    let (open, close) = self.lines[row]
      .0
      .iter()
      .enumerate()
      .filter_map(|(c, g)| (g.as_char() == Some(q_ch)).then_some(c))
      .collect::<Vec<_>>()
      .chunks_exact(2)
      .map(|pair| (pair[0], pair[1]))
      .find(|&(_, close)| close >= cur_col)?;

    match bound {
      Bound::Around => {
        // Around a quoted structure extends through any trailing whitespace
        // after the closing quote. When some follows, the object ends
        // exclusively at the first non-whitespace char. When none does (the
        // quote ends the line), it ends inclusively on the last char so the
        // closing quote itself is still captured.
        let start = Pos { row, col: open };
        let after_close = Pos {
          row,
          col: close + 1,
        };
        let mut classes = self.char_classes_forward_from(after_close);
        match classes.find(|(_, c)| !c.is_ws()) {
          Some((p, _)) => Some(MotionKind::Char {
            start,
            end: p,
            inclusive: false,
          }),
          None => Some(MotionKind::Char {
            start,
            end: Pos {
              row,
              col: self.lines[row].len().saturating_sub(1),
            },
            inclusive: true,
          }),
        }
      }
      Bound::Inside => {
        let start = Pos { row, col: open + 1 };
        let end = Pos { row, col: close };
        (start <= end).then_some(MotionKind::Char {
          start,
          end,
          inclusive: false,
        })
      }
    }
  }
  fn text_obj_delim(&mut self, obj: TextObj, bound: Bound) -> Option<MotionKind> {
    let (opener, closer) = match obj {
      TextObj::Paren(_) => ('(', ')'),
      TextObj::Bracket(_) => ('[', ']'),
      TextObj::Brace(_) => ('{', '}'),
      TextObj::Angle(_) => ('<', '>'),
      _ => unreachable!(),
    };
    let mut depth = 0;
    let start_pos = self
      .scan_backward(|g| {
        if g.as_char() == Some(closer) {
          depth += 1;
        }
        if g.as_char() == Some(opener) {
          if depth == 0 {
            return true;
          }
          depth -= 1;
        }
        false
      })
      .or_else(|| self.scan_forward(|g| g.as_char() == Some(opener)))?;

    depth = 0;
    let end_pos = self.scan_forward_from(start_pos, |g| {
      if g.as_char() == Some(opener) {
        depth += 1;
      }
      if g.as_char() == Some(closer) {
        depth -= 1;
      }
      depth == 0
    })?;

    match bound {
      Bound::Around => Some(MotionKind::Char {
        start: start_pos,
        end: end_pos,
        inclusive: true,
      }),
      Bound::Inside => {
        let mut start_pos = start_pos;
        start_pos.col += 1;
        (start_pos <= end_pos).then_some(MotionKind::Char {
          start: start_pos,
          end: end_pos,
          inclusive: false,
        })
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use crate::readline::LineBuf;
  use crate::readline::editcmd::LineAddr;
  use crate::tests::testutil::TestGuard;

  /// Build a multi-line `LineBuf` with the cursor placed on `cursor_row`.
  fn buf_at(content: &str, cursor_row: usize) -> LineBuf {
    let mut b = LineBuf::default();
    b.set_buffer(content);
    b.set_cursor(super::super::Pos {
      row: cursor_row,
      col: 0,
    });
    b
  }

  // ─── LineAddr::Number — 1-indexed, clamped to last line ─────────────

  #[test]
  fn resolve_number_within_range() {
    let _g = TestGuard::new();
    let b = buf_at("a\nb\nc\nd", 0);
    assert_eq!(b.resolve_line_addr(&LineAddr::Number(2)).unwrap(), Some(1));
  }

  #[test]
  fn resolve_number_clamps_past_end() {
    let _g = TestGuard::new();
    let b = buf_at("a\nb\nc", 0);
    // 99 → clamped to last line (index 2).
    assert_eq!(b.resolve_line_addr(&LineAddr::Number(99)).unwrap(), Some(2));
  }

  #[test]
  fn resolve_number_zero_saturates_to_first() {
    let _g = TestGuard::new();
    let b = buf_at("a\nb\nc", 0);
    // 0.saturating_sub(1) → 0, min last → 0.
    assert_eq!(b.resolve_line_addr(&LineAddr::Number(0)).unwrap(), Some(0));
  }

  // ─── LineAddr::Current / Last ────────────────────────────────────────

  #[test]
  fn resolve_current_returns_cursor_row() {
    let _g = TestGuard::new();
    let b = buf_at("a\nb\nc\nd", 2);
    assert_eq!(b.resolve_line_addr(&LineAddr::Current).unwrap(), Some(2));
  }

  #[test]
  fn resolve_last_returns_last_row_index() {
    let _g = TestGuard::new();
    let b = buf_at("a\nb\nc\nd", 0);
    assert_eq!(b.resolve_line_addr(&LineAddr::Last).unwrap(), Some(3));
  }

  // ─── LineAddr::Offset ────────────────────────────────────────────────

  #[test]
  fn resolve_offset_positive() {
    let _g = TestGuard::new();
    let b = buf_at("a\nb\nc\nd", 1);
    assert_eq!(b.resolve_line_addr(&LineAddr::Offset(2)).unwrap(), Some(3));
  }

  #[test]
  fn resolve_offset_negative() {
    let _g = TestGuard::new();
    let b = buf_at("a\nb\nc\nd", 3);
    assert_eq!(b.resolve_line_addr(&LineAddr::Offset(-2)).unwrap(), Some(1));
  }

  #[test]
  fn resolve_offset_negative_saturates_at_zero() {
    let _g = TestGuard::new();
    let b = buf_at("a\nb\nc", 1);
    // 1 + (-99) saturates to 0, not underflow.
    assert_eq!(
      b.resolve_line_addr(&LineAddr::Offset(-99)).unwrap(),
      Some(0)
    );
  }

  // ─── LineAddr::Pattern (forward) ─────────────────────────────────────

  #[test]
  fn resolve_pattern_finds_next_forward_match() {
    let _g = TestGuard::new();
    let b = buf_at("foo\nbar\nbaz\nfoo again", 0);
    let result = b
      .resolve_line_addr(&LineAddr::Pattern("baz".into()))
      .unwrap();
    assert_eq!(result, Some(2));
  }

  #[test]
  fn resolve_pattern_wraps_around() {
    let _g = TestGuard::new();
    // cursor on row 2, pattern matches row 0 → search wraps.
    let b = buf_at("target\nb\nc", 2);
    let result = b
      .resolve_line_addr(&LineAddr::Pattern("target".into()))
      .unwrap();
    assert_eq!(result, Some(0));
  }

  #[test]
  fn resolve_pattern_no_match_returns_none() {
    let _g = TestGuard::new();
    let b = buf_at("a\nb\nc", 0);
    let result = b
      .resolve_line_addr(&LineAddr::Pattern("xyz_no_match".into()))
      .unwrap();
    assert_eq!(result, None);
  }

  #[test]
  fn resolve_pattern_invalid_regex_returns_none() {
    let _g = TestGuard::new();
    let b = buf_at("a\nb\nc", 0);
    // Unclosed bracket — invalid regex. Function logs status_msg and returns Ok(None).
    let result = b.resolve_line_addr(&LineAddr::Pattern("[".into())).unwrap();
    assert_eq!(result, None);
  }

  // ─── LineAddr::PatternRev (backward) ─────────────────────────────────

  #[test]
  fn resolve_pattern_rev_finds_previous_match() {
    let _g = TestGuard::new();
    let b = buf_at("target\nb\nc\nd", 3);
    let result = b
      .resolve_line_addr(&LineAddr::PatternRev("target".into()))
      .unwrap();
    assert_eq!(result, Some(0));
  }

  #[test]
  fn resolve_pattern_rev_wraps_around() {
    let _g = TestGuard::new();
    // cursor on row 0, pattern matches row 2 → backward search wraps to end.
    let b = buf_at("a\nb\ntarget", 0);
    let result = b
      .resolve_line_addr(&LineAddr::PatternRev("target".into()))
      .unwrap();
    assert_eq!(result, Some(2));
  }

  // ─── LineAddr::Mark — anchors and unimplemented ──────────────────────

  #[test]
  fn resolve_mark_lt_without_selection_returns_none() {
    let _g = TestGuard::new();
    let b = buf_at("a\nb\nc", 0);
    let result = b.resolve_line_addr(&LineAddr::Mark('<')).unwrap();
    assert_eq!(result, None);
  }

  #[test]
  fn resolve_mark_gt_without_selection_returns_none() {
    let _g = TestGuard::new();
    let b = buf_at("a\nb\nc", 0);
    let result = b.resolve_line_addr(&LineAddr::Mark('>')).unwrap();
    assert_eq!(result, None);
  }

  #[test]
  fn resolve_mark_lt_with_char_selection_returns_anchor_row() {
    let _g = TestGuard::new();
    let mut b = buf_at("aaa\nbbb\nccc\nddd", 1);
    // Start char-select at current cursor (row 1), then move cursor to row 3.
    b.start_char_select();
    b.set_cursor(super::super::Pos { row: 3, col: 0 });
    let lt = b.resolve_line_addr(&LineAddr::Mark('<')).unwrap();
    let gt = b.resolve_line_addr(&LineAddr::Mark('>')).unwrap();
    // The lower-row endpoint is `<`, the upper is `>` (or vice-versa
    // depending on internal anchor/cursor ordering); just verify they
    // bracket the selection.
    let (a, c) = (lt.unwrap(), gt.unwrap());
    let (lo, hi) = if a < c { (a, c) } else { (c, a) };
    assert_eq!(lo, 1);
    assert_eq!(hi, 3);
  }

  #[test]
  fn resolve_mark_with_line_selection_returns_anchor_and_cursor_rows() {
    let _g = TestGuard::new();
    let mut b = buf_at("aaa\nbbb\nccc\nddd", 0);
    b.start_line_select();
    b.set_cursor(super::super::Pos { row: 2, col: 0 });
    let lt = b.resolve_line_addr(&LineAddr::Mark('<')).unwrap();
    let gt = b.resolve_line_addr(&LineAddr::Mark('>')).unwrap();
    let (a, c) = (lt.unwrap(), gt.unwrap());
    let (lo, hi) = if a < c { (a, c) } else { (c, a) };
    assert_eq!(lo, 0);
    assert_eq!(hi, 2);
  }

  #[test]
  fn resolve_mark_unimplemented_named_mark_returns_none() {
    let _g = TestGuard::new();
    let b = buf_at("a\nb\nc", 0);
    // Named marks ('a'-'z') aren't implemented; return None.
    let result = b.resolve_line_addr(&LineAddr::Mark('a')).unwrap();
    assert_eq!(result, None);
  }

  // ===================== motion_mutation =====================

  use super::super::types::Grapheme;
  use super::MotionKind;
  use super::Pos;

  fn upper_grapheme(g: &Grapheme) -> Grapheme {
    Grapheme::from(g.to_string().to_uppercase().as_str())
  }

  // ─── MotionKind::Char (single row) ──────────────────────────────

  #[test]
  fn motion_mutation_char_inclusive_single_row() {
    let _g = TestGuard::new();
    let mut b = buf_at("hello", 0);
    b.motion_mutation(
      &MotionKind::Char {
        start: Pos { row: 0, col: 0 },
        end: Pos { row: 0, col: 2 },
        inclusive: true,
      },
      upper_grapheme,
    );
    assert_eq!(b.to_string(), "HELlo");
  }

  #[test]
  fn motion_mutation_char_exclusive_single_row() {
    let _g = TestGuard::new();
    let mut b = buf_at("hello", 0);
    b.motion_mutation(
      &MotionKind::Char {
        start: Pos { row: 0, col: 0 },
        end: Pos { row: 0, col: 2 },
        inclusive: false,
      },
      upper_grapheme,
    );
    // Exclusive end: cols 0..2 → "HEllo".
    assert_eq!(b.to_string(), "HEllo");
  }

  #[test]
  fn motion_mutation_char_range_past_eol_stops_at_line_end() {
    let _g = TestGuard::new();
    let mut b = buf_at("abc", 0);
    // Range 0..10 inclusive — line is only 3 chars; the loop breaks.
    b.motion_mutation(
      &MotionKind::Char {
        start: Pos { row: 0, col: 0 },
        end: Pos { row: 0, col: 10 },
        inclusive: true,
      },
      upper_grapheme,
    );
    assert_eq!(b.to_string(), "ABC");
  }

  #[test]
  fn motion_mutation_char_ordered_swap() {
    // start > end gets ordered() to (end, start). Verify reverse range works.
    let _g = TestGuard::new();
    let mut b = buf_at("hello", 0);
    b.motion_mutation(
      &MotionKind::Char {
        start: Pos { row: 0, col: 3 },
        end: Pos { row: 0, col: 1 },
        inclusive: true,
      },
      upper_grapheme,
    );
    // ordered → start=col 1, end=col 3 inclusive → cols 1..=3.
    assert_eq!(b.to_string(), "hELLo");
  }

  // ─── MotionKind::Char (multi-row) ───────────────────────────────

  #[test]
  fn motion_mutation_char_multi_row_inclusive() {
    let _g = TestGuard::new();
    let mut b = buf_at("hello\nworld\nfoo", 0);
    b.motion_mutation(
      &MotionKind::Char {
        start: Pos { row: 0, col: 2 },
        end: Pos { row: 2, col: 1 },
        inclusive: true,
      },
      upper_grapheme,
    );
    // Row 0: from col 2 to end of line ("hello" → "heLLO")
    // Row 1: entire line ("world" → "WORLD")
    // Row 2: cols 0..=1 ("foo" → "FOo")
    assert_eq!(b.to_string(), "heLLO\nWORLD\nFOo");
  }

  #[test]
  fn motion_mutation_char_multi_row_exclusive_last_row() {
    let _g = TestGuard::new();
    let mut b = buf_at("hello\nworld", 0);
    b.motion_mutation(
      &MotionKind::Char {
        start: Pos { row: 0, col: 2 },
        end: Pos { row: 1, col: 2 },
        inclusive: false,
      },
      upper_grapheme,
    );
    // Row 0: cols 2..end ("hello" → "heLLO")
    // Row 1: cols 0..2 exclusive → cols 0..2 → "world" → "WOrld"
    assert_eq!(b.to_string(), "heLLO\nWOrld");
  }

  // ─── MotionKind::Line ──────────────────────────────────────────

  #[test]
  fn motion_mutation_line_inclusive() {
    let _g = TestGuard::new();
    let mut b = buf_at("a\nb\nc\nd", 0);
    b.motion_mutation(
      &MotionKind::Line {
        start: 1,
        end: 2,
        inclusive: true,
      },
      upper_grapheme,
    );
    assert_eq!(b.to_string(), "a\nB\nC\nd");
  }

  #[test]
  fn motion_mutation_line_exclusive() {
    let _g = TestGuard::new();
    let mut b = buf_at("a\nb\nc\nd", 0);
    // Exclusive end → end.saturating_sub(1) = 1. So row 1..=1 only.
    b.motion_mutation(
      &MotionKind::Line {
        start: 1,
        end: 2,
        inclusive: false,
      },
      upper_grapheme,
    );
    assert_eq!(b.to_string(), "a\nB\nc\nd");
  }

  #[test]
  fn motion_mutation_line_zero_end_doesnt_underflow() {
    // end=0, exclusive → end.saturating_sub(1) = 0; range 0..=0 still mutates row 0.
    let _g = TestGuard::new();
    let mut b = buf_at("abc\ndef", 0);
    b.motion_mutation(
      &MotionKind::Line {
        start: 0,
        end: 0,
        inclusive: false,
      },
      upper_grapheme,
    );
    assert_eq!(b.to_string(), "ABC\ndef");
  }
}
