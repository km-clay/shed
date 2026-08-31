use std::{
  fs::OpenOptions,
  io::Write,
  path::PathBuf,
  sync::atomic::{AtomicUsize, Ordering},
};

use crate::{
  HashSet, autocmd, defer,
  eval::{execute, lex::TkFlags},
  motion,
  procio::{self, RedirSet, RedirSpec},
  sherr, shopt,
  state::vars::VarStr,
  state::{
    Shed, paths,
    vars::{VarFlags, VarKind},
  },
  status_msg, system_msg, try_var,
  util::{self, error::ShResult, guards, pos::Pos, strops},
  varstr, verb,
};
use itertools::Itertools;
use nix::libc::STDIN_FILENO;

use super::{
  Line, Lines, MotionKind,
  editcmd::{Anchor, Cmd, EditCmd, LineAddr, ReadSrc, StashArgs, StashListArg, Verb, WriteDest},
  editmode::{AddressRange, ExNdRule, ExNode, SubFlags},
  stash::{Stash, StashedCmd},
};

thread_local! {
  static EX_MODE_ENTRIES: AtomicUsize = const { AtomicUsize::new(1) };
}

fn get_entry_id() -> usize {
  EX_MODE_ENTRIES.with(|counter| counter.fetch_add(1, Ordering::SeqCst))
}

fn get_entry_name() -> VarStr {
  let id = get_entry_id();
  format!("ex_entry #{id}").into()
}

impl super::LineBuf {
  pub(super) fn dispatch_ex_node(&mut self, cmd: &EditCmd) -> ShResult<()> {
    let EditCmd {
      verb: Some(Cmd(_, Verb::ExCmd(node))),
      ..
    } = cmd
    else {
      return Ok(());
    };
    let ExNode {
      address,
      bang,
      kind,
    } = node;
    let address = address.clone();

    match kind {
      ExNdRule::Expand /*--------------------*/ => self.ex_expand(cmd, address, *bang),
      ExNdRule::Delete /*====================*/ => self.ex_delete(cmd, address),
      ExNdRule::Yank /*----------------------*/ => self.ex_yank(cmd, address),
      ExNdRule::Join /*======================*/ => self.ex_join(cmd, address),
      ExNdRule::Put(anchor) /*---------------*/ => self.ex_put(cmd, *anchor, address),
      ExNdRule::Edit(paths) /*===============*/ => Self::ex_edit(paths),
      ExNdRule::Write(write_dest) /*---------*/ => self.ex_write(write_dest),
      ExNdRule::RepeatSubstitute /*==========*/ => self.repeat_substitute(cmd),
      ExNdRule::RepeatGlobal /*--------------*/ => self.repeat_global(cmd),
      ExNdRule::Shell(sh_cmd) /*=============*/ => self.ex_shell_cmd(address.as_ref(), sh_cmd),
      ExNdRule::Stash(stash_args) /*---------*/ => self.ex_stash(stash_args),
      ExNdRule::Substitute { pat, repl, flags } => self.ex_substitute(cmd, pat, repl, *flags, address.as_ref()),
      ExNdRule::Global { pat, nested } /*----*/ => self.ex_global(cmd, *bang, pat, nested, address),
      ExNdRule::Move(dest) /*================*/ => self.ex_move(cmd, address, dest, true),
      ExNdRule::Transfer(dest) /*------------*/ => self.ex_move(cmd, address, dest, false),
      ExNdRule::Trim /*======================*/ => self.ex_trim(address, *bang),
      ExNdRule::Read(read_src) /*------------*/ => {
        self.ex_read(read_src);
        Ok(())
      }

      // Intercepted by the editor driver (ShedLine interactively, EditorCore
      // headlessly) before reaching here. Erroring rather than panicking keeps
      // a stray one from taking down the whole shell.
      ExNdRule::Normal { .. } | ExNdRule::WriteQuit | ExNdRule::Quit => {
        Err(crate::sherr!(ExecFail, "ex command is handled by the editor driver, not the buffer"))
      }
    }
  }

  fn ex_trim(&mut self, addr: Option<AddressRange>, bang: bool) -> ShResult<()> {
    let addr_range = addr.unwrap_or_else(AddressRange::all_lines);
    self.edit(|this| this.trim_range(addr_range.clone(), bang))
  }

  fn ex_move(
    &mut self,
    cmd: &EditCmd,
    source: Option<AddressRange>,
    dest: &AddressRange,
    delete: bool,
  ) -> ShResult<()> {
    // Resolve the source line range as a motion (defaults to the current line).
    let source = source.unwrap_or_default();
    let src_cmd = EditCmd {
      motion: Some(motion!(source.as_motion())),
      ..cmd.clone()
    };
    let Some(motion) = self.eval_motion(&src_cmd)? else {
      return Ok(());
    };
    let MotionKind::Line {
      start,
      end,
      inclusive,
    } = motion
    else {
      return Err(sherr!(ExecFail, "move/copy operate on whole lines"));
    };
    let src_start = start;
    let src_end = if inclusive {
      end
    } else {
      end.saturating_sub(1)
    };
    let AddressRange::Single(addr) = dest else {
      return Err(sherr!(
        ExecFail,
        "move/copy take a single destination address"
      ));
    };
    let insert_idx = match addr {
      LineAddr::Number(n) if *n == 0 => 0,
      other => {
        let Some(row) = self.resolve_line_addr(other)? else {
          return Err(sherr!(ExecFail, "could not resolve move/copy destination"));
        };
        row + 1
      }
    };

    // Relocating a range into itself is undefined.
    if delete && insert_idx > src_start && insert_idx <= src_end {
      return Err(sherr!(ExecFail, "cannot move a range of lines into itself"));
    }

    let content = if delete {
      self.delete_range(&motion)
    } else {
      self.yank_range(&motion)
    };
    let lines = content.0;
    let count = lines.len();
    if count == 0 {
      return Ok(());
    }

    let final_idx = if delete && insert_idx > src_end {
      insert_idx - count
    } else {
      insert_idx
    }
    .min(self.lines.len());

    for (i, line) in lines.into_iter().enumerate() {
      self.lines.insert(final_idx + i, line);
    }

    // Leave the cursor on the last relocated line, like vi.
    let last = (final_idx + count)
      .saturating_sub(1)
      .min(self.lines.len().saturating_sub(1));
    self.set_cursor(Pos { row: last, col: 0 });

    Ok(())
  }

  fn ex_global(
    &mut self,
    cmd: &EditCmd,
    negated: bool,
    pat: &str,
    nested: &ExNode,
    range: Option<AddressRange>,
  ) -> ShResult<()> {
    let range = range.unwrap_or_else(AddressRange::all_lines);
    let constraint = range.as_motion();
    let lines = self.get_matching_lines(&constraint, pat, negated)?;

    let nested_cmd = EditCmd {
      verb: Some(verb!(Verb::ExCmd(nested.clone()))),
      ..cmd.clone()
    };

    for line in lines.into_iter().rev() {
      self.set_cursor(Pos { row: line, col: 0 });
      self.dispatch_ex_node(&nested_cmd)?;
    }

    self.last_global = Some(cmd.clone());
    Ok(())
  }

  fn ex_substitute(
    &mut self,
    cmd: &EditCmd,
    old: &str,
    new: &str,
    flags: SubFlags,
    range: Option<&AddressRange>,
  ) -> ShResult<()> {
    let line_nums = self.lines_for_address(range)?;

    let re = match Shed::meta_mut(|m| m.get_regex(old)) {
      Ok(re) => re,
      Err(e) => {
        status_msg!("{e}");
        return Ok(());
      }
    };

    // TODO: implement flag logic
    let mut changes: Vec<(usize, Lines)> = vec![];
    let lines = self
      .lines
      .iter()
      .enumerate()
      .filter(|(i, _)| line_nums.contains(i));

    for (i, line) in lines {
      let s = line.to_string();
      let res = if flags.contains(SubFlags::GLOBAL) {
        re.replace_all(&s, new)
      } else {
        re.replace(&s, new)
      };
      let lines = Lines::to_lines(&res);
      changes.push((i, lines));
    }

    for (i, change) in changes.into_iter().rev() {
      self.lines.remove(i);
      for (j, new_line) in change.0.into_iter().enumerate() {
        self.lines.insert(i + j, new_line);
      }
    }

    self.last_substitute = Some(cmd.clone());

    Ok(())
  }
  #[expect(clippy::too_many_lines)]
  fn ex_stash(&mut self, args: &StashArgs) -> ShResult<()> {
    let Ok(stash) = Stash::new() else {
      status_msg!("Failed to access stash - database unreachable");
      return Ok(());
    };
    match args {
      StashArgs::Push(arg) => {
        if self.is_empty() {
          status_msg!("Buffer is empty, nothing to stash");
          return Ok(());
        }
        let name = arg.clone().filter(|a| !a.to_str_lossy().trim().is_empty());
        let buffer = self.to_string();
        let (s, e) = (self.row(), self.col());

        stash.push(name.as_ref(), &buffer, (s, e))?;
        self.clear_buffer();
        self.clear_hint();
        self.set_cursor(Pos::new(0, 0));
      }
      StashArgs::Pop(arg) => {
        let stack_len = stash.stack_len();
        let idx = arg
          .as_ref()
          .map(|a| a.to_str_lossy().parse::<usize>())
          .transpose()
          .ok()
          .flatten()
          .unwrap_or(stack_len.saturating_sub(1));

        let StashedCmd {
          name: _,
          buffer,
          cursor_pos,
        } = match stash.pop(idx) {
          Ok(ent) => {
            if let Some(ent) = ent {
              status_msg!("stash: Popped stash entry");
              ent
            } else {
              if stack_len == 0 {
                status_msg!("stash: Stash is empty, nothing to pop");
              } else {
                status_msg!("stash: No stash entry at index '{idx}'");
              }
              return Ok(());
            }
          }
          Err(e) => {
            status_msg!("stash: Failed to pop stash entry: {e}");
            return Ok(());
          }
        };

        self.set_buffer(&buffer.to_str_lossy());

        let cursor_pos = match self.parse_pos(&cursor_pos.to_str_lossy()) {
          Ok(pos) => pos,
          Err(e) => {
            status_msg!("Failed to parse cursor position from stash: {e}");
            Pos { row: 0, col: 0 }
          }
        };

        self.set_cursor(cursor_pos);
      }
      StashArgs::Drop(arg) => {
        let idx = arg
          .as_ref()
          .map(|a| a.to_str_lossy().parse::<usize>())
          .transpose()
          .ok()
          .flatten()
          .unwrap_or(0);
        let stack_len = stash.stack_len();

        match stash.pop(idx).ok().flatten() {
          Some(_) => {
            status_msg!("stash: Dropped stash entry");
          }
          None => {
            if stack_len == 0 {
              status_msg!("stash: Stash is empty, nothing to drop");
            } else {
              status_msg!("stash: No stash entry at index '{idx}'");
            }
          }
        }
      }
      StashArgs::Apply(arg) => {
        let stack_len = stash.stack_len();
        let name = arg
          .clone()
          .unwrap_or(varstr!("{}", stack_len.saturating_sub(1)));

        let Some(StashedCmd {
          name,
          buffer,
          cursor_pos,
        }) = stash.get(&name.to_str_lossy())?
        else {
          if let Ok(idx) = name.to_str_lossy().parse::<usize>() {
            if stack_len == 0 {
              status_msg!("stash: Stash is empty");
            } else {
              status_msg!("stash: No stash entry at index '{idx}'");
            }
          } else {
            status_msg!("stash: No stash entry named '{name}'");
          }
          return Ok(());
        };

        if let Some(name) = name {
          status_msg!("stash: Applied stash entry '{}'", name);
        }

        self.set_buffer(&buffer.to_str_lossy());

        let cursor_pos = match self.parse_pos(&cursor_pos.to_str_lossy()) {
          Ok(pos) => pos,
          Err(e) => {
            status_msg!("Failed to parse cursor position from stash: {e}");
            Pos { row: 0, col: 0 }
          }
        };

        self.set_cursor(cursor_pos);
      }
      StashArgs::Insert(arg) => {
        let stack_len = stash.stack_len();
        let name = arg
          .clone()
          .unwrap_or(varstr!("{}", stack_len.saturating_sub(1)));

        let Some(StashedCmd {
          name: _,
          buffer,
          cursor_pos,
        }) = stash.get(&name.to_str_lossy())?
        else {
          if let Ok(idx) = name.to_str_lossy().parse::<usize>() {
            if stack_len == 0 {
              status_msg!("stash: Stash is empty");
            } else {
              status_msg!("stash: No stash entry at index '{idx}'");
            }
          } else {
            status_msg!("stash: No stash entry named '{name}'");
          }
          return Ok(());
        };

        let lines = Lines::to_lines(&buffer.to_str_lossy());
        let num_lines = lines.len();
        let line_range = self.row()..self.row() + num_lines;

        self.insert_lines_at(self.cursor.pos, lines);

        let cursor_offset = match self.parse_pos(&cursor_pos.to_str_lossy()) {
          Ok(pos) => pos,
          Err(e) => {
            system_msg!("Failed to parse cursor position from stash: {e}");
            Pos { row: 0, col: 0 }
          }
        };
        self.cursor.pos = self.cursor.pos + cursor_offset;
        self.fix_cursor();
        if shopt!(line.auto_indent) {
          self.equalize_rows(line_range.collect());
        }
      }
      StashArgs::Swap(arg) => {
        let stack_len = stash.stack_len();
        let ident = arg
          .clone()
          .unwrap_or(varstr!("{}", stack_len.saturating_sub(1)));

        let Some(StashedCmd {
          name: ent_name,
          buffer: stashed_buf,
          cursor_pos: stashed_cursor,
        }) = stash.get(&ident.to_str_lossy())?
        else {
          if let Ok(idx) = ident.to_str_lossy().parse::<usize>() {
            if stack_len == 0 {
              status_msg!("stash: Stash is empty");
            } else {
              status_msg!("stash: No stash entry at index '{idx}'");
            }
          } else {
            status_msg!("stash: No stash entry named '{ident}'");
          }
          return Ok(());
        };

        let curr_buf = self.to_string();
        let curr_cursor = (self.row(), self.col());

        // Write the current buffer back into the stash slot.
        //
        // - Named entries: push(Some(name), ...) overwrites by name, so
        // repeated `stash swap <name>` is its own inverse.
        //
        // - Indexed entries: pop the row and push the current buffer as
        // a new unnamed entry at the top of the stack. Repeated
        // `stash swap <idx>` rotates the buffer through every entry
        // from `idx` up to the top, returning to the original state
        // after (stack_len - idx + 1) invocations.
        if let Some(name) = ent_name.clone() {
          stash.push(Some(&name), &curr_buf, curr_cursor)?;
        } else {
          let idx = ident
            .to_str_lossy()
            .parse::<usize>()
            .unwrap_or(stack_len.saturating_sub(1));
          stash.pop(idx)?;
          stash.push(None, &curr_buf, curr_cursor)?;
        }

        self.set_buffer(&stashed_buf.to_str_lossy());

        let cursor_pos = match self.parse_pos(&stashed_cursor.to_str_lossy()) {
          Ok(pos) => pos,
          Err(e) => {
            status_msg!("Failed to parse cursor position from stash: {e}");
            Pos { row: 0, col: 0 }
          }
        };
        self.set_cursor(cursor_pos);

        if let Some(name) = ent_name {
          status_msg!("stash: Swapped with '{}'", name);
        } else {
          status_msg!("stash: Swapped with stack entry");
        }
      }
      StashArgs::List(arg) => {
        let output = match arg {
          Some(StashListArg::Stack) => {
            stash.list(/*named_only:*/ false, /*stack_only:*/ true)
          }
          Some(StashListArg::Named) => {
            stash.list(/*named_only:*/ true, /*stack_only:*/ false)
          }
          None => stash.list(/*named_only:*/ false, /*stack_only:*/ false),
        };
        if output.to_str_lossy().trim().is_empty() {
          match arg {
            Some(StashListArg::Named) => {
              status_msg!("stash: No named stash entries");
            }
            Some(StashListArg::Stack) => {
              status_msg!("stash: Stack is empty");
            }
            None => {
              status_msg!("stash: No stash entries");
            }
          }
        } else {
          for line in output.to_str_lossy().lines() {
            system_msg!("{line}");
          }
        }
      }
    }

    Ok(())
  }

  fn repeat_global(&mut self, cmd: &EditCmd) -> ShResult<()> {
    let Some(saved) = self.last_global.clone() else {
      return Ok(());
    };
    let Some(merged) = merge_repeat_addr(cmd, &saved) else {
      return Ok(());
    };
    self.exec_cmd(&merged)
  }

  fn repeat_substitute(&mut self, cmd: &EditCmd) -> ShResult<()> {
    let Some(saved) = self.last_substitute.clone() else {
      return Ok(());
    };
    let Some(merged) = merge_repeat_addr(cmd, &saved) else {
      return Ok(());
    };
    self.exec_cmd(&merged)
  }

  fn ex_write(&mut self, dest: &WriteDest) -> ShResult<()> {
    match dest {
      WriteDest::FileAppend(path_buf) | WriteDest::File(path_buf) => {
        let Some(path_buf) = path_buf
          .clone()
          .or_else(|| self.open_file.as_ref().map(PathBuf::from).clone())
        else {
          status_msg!("expected file argument for write command");
          return Ok(());
        };
        let display_path = paths::display_path(&path_buf);

        let Ok(mut file) = (if matches!(dest, WriteDest::File(_)) {
          OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path_buf)
        } else {
          OpenOptions::new().create(true).append(true).open(&path_buf)
        }) else {
          system_msg!("Failed to open file {display_path}");
          return Ok(());
        };
        let mut joined = self.to_string();
        if !joined.ends_with('\n') {
          joined.push('\n');
        }

        let bytes = joined.as_bytes();

        #[expect(clippy::naive_bytecount)] // cold path, don't need an entire dependency for this
        let lines = bytes.iter().filter(|&&b| b == b'\n').count();

        let len = bytes.len() as u64;
        let mut size = String::new();

        strops::format_size(len, &mut size)?;

        if let Err(e) = file.write_all(bytes) {
          system_msg!("Failed to write to file {display_path}: {e}");
        }

        status_msg!("Wrote {lines} lines [{size}] to '{display_path}'");

        return Ok(());
      }
      WriteDest::Cmd(cmd) => {
        let buf: VarStr = self.to_string().into();
        let spec = RedirSpec::Buffer {
          fd: STDIN_FILENO,
          buf,
          flags: TkFlags::empty(),
        };

        let redirs = RedirSet::from(spec);
        let _guard = redirs.apply().or_fatal()?;

        autocmd!(PreCmd);
        {
          defer!(autocmd!(PostCmd));
          execute::exec_nonint(cmd.clone(), Some("ex write".into()))?;
        }
      }
    }
    Ok(())
  }

  fn ex_read(&mut self, src: &ReadSrc) {
    let contents = match src {
      ReadSrc::File(path_buf) => {
        let display_path = paths::display_path(path_buf);
        let contents = match std::fs::read_to_string(path_buf) {
          Ok(c) => c,
          Err(e) => {
            system_msg!("Failed to read '{display_path}': {e}");
            return;
          }
        };
        let line_count = contents.lines().count();
        let byte_count = contents.len();
        let mut size = String::new();
        strops::format_size(byte_count as u64, &mut size).ok();
        status_msg!("Read {line_count} lines [{size}] from '{display_path}'",);
        let realpath = paths::lex_normalize_path(path_buf);
        self.open_file = Some(realpath.to_string_lossy().into());
        contents
      }
      ReadSrc::Cmd(cmd) => {
        autocmd!(PreCmd);
        defer!(autocmd!(PostCmd));
        match procio::capture_command(cmd.as_bytes(), None, Some(&get_entry_name())) {
          Ok(out) => out,
          Err(e) => {
            e.print_error();
            return;
          }
        }
      }
    };

    let new_lines = Lines::to_lines(&contents);
    self.insert_lines_at(self.cursor.pos, new_lines);
    self.indent_cache = None;
  }

  fn ex_edit(paths: &[PathBuf]) -> ShResult<()> {
    if try_var!("EDITOR").is_none() {
      system_msg!("$EDITOR is unset. Aborting edit.");
      Ok(())
    } else {
      let args = paths.iter().map(|p| format!("{}", p.display())).join(" ");
      let input = format!("$EDITOR {args}");

      execute::exec_int(input.into(), Some(get_entry_name()))
    }
  }

  fn ex_shell_cmd(&mut self, addr: Option<&AddressRange>, sh_cmd: &str) -> ShResult<()> {
    let lines = if addr.is_some() {
      self.lines_for_address(addr)?
    } else {
      self.run_shell_cmd(sh_cmd, None)?;
      return Ok(());
    };

    if lines.is_empty() {
      self.run_shell_cmd(sh_cmd, None)?;
      return Ok(());
    }

    let (s, mut e) = util::ordered(*lines.first().unwrap(), *lines.last().unwrap());
    e = e.min(self.lines.len().saturating_sub(1));
    let lines = self.lines.drain(s..=e).collect::<Vec<_>>();
    if self.lines.is_empty() {
      self.lines.push(Line::default());
    }
    let input = format!("{}\n", Lines(lines).join());
    let output = self
      .run_shell_cmd(sh_cmd, Some(&input))?
      .unwrap_or_default();
    let trimmed = output.strip_suffix('\n').unwrap_or(&output);
    let new_lines = Lines::to_lines(trimmed);
    self.lines.0.splice(s..s, new_lines.0);

    Ok(())
  }

  fn run_shell_cmd(&mut self, sh_cmd: &str, stdin: Option<&str>) -> ShResult<Option<String>> {
    let mut vars = HashSet::default();
    vars.insert("BUFFER".into());
    vars.insert("CURSOR".into());
    vars.insert("ANCHOR".into());
    let _guard = guards::var_ctx_guard(vars);

    let mut buf: VarStr = self.to_string().into();
    let cursor_raw = self.cursor_to_flat();
    let mut cursor: VarStr = cursor_raw.to_string().into();
    let mut anchor = self.anchor_to_flat();

    Shed::vars_mut(|v| -> ShResult<()> {
      v.set_var("BUFFER", VarKind::string(buf.clone()), VarFlags::EXPORT)?;
      v.set_var("CURSOR", VarKind::string(cursor.clone()), VarFlags::EXPORT)?;
      if let Some(anchor) = anchor {
        v.set_var(
          "ANCHOR",
          VarKind::string(anchor.to_string().into()),
          VarFlags::EXPORT,
        )?;
      }
      Ok(())
    })?;

    autocmd!(PreCmd);
    let output = if let Some(stdin) = stdin {
      defer!(autocmd!(PostCmd));
      let _guard = Shed::term_mut(|t| t.yield_terminal(false));
      Some(procio::capture_command(
        sh_cmd.as_bytes(),
        Some(stdin.as_bytes()),
        Some(&get_entry_name()),
      )?)
    } else {
      defer!(autocmd!(PostCmd));
      let _guard = Shed::term_mut(|t| t.yield_terminal(false));
      execute::exec_int(sh_cmd.into(), Some(get_entry_name()))?;
      None
    };

    let mut new_anchor = None;

    let keys = Shed::vars_mut(|v| {
      buf = v.take_var("BUFFER");
      cursor = v.take_var("CURSOR");
      if anchor.is_some() {
        new_anchor = Some(v.take_var("ANCHOR"));
      }
      v.take_var("KEYS")
    });

    self.set_buffer(&buf.to_str_lossy());

    if let Some(new_anchor) = new_anchor {
      if let Ok(pos) = self.parse_pos(&new_anchor.to_str_lossy()) {
        anchor = Some(self.pos_to_flat(pos));
      } else {
        log::warn!("Invalid anchor position returned from shell command: '{new_anchor}'");
        anchor = None;
      }
    }

    if let Ok(pos) = self.parse_pos(&cursor.to_str_lossy()) {
      self.set_cursor(pos);
    } else {
      log::warn!("Invalid cursor position returned from shell command: '{cursor}'");
      self.set_cursor_from_flat(cursor_raw);
    }

    if let Some(anchor) = anchor
      && anchor != cursor_raw
      && self.select_mode.is_some()
    {
      self.set_anchor_from_flat(anchor);
    }
    if !keys.is_empty() {
      Shed::meta_mut(|m| m.set_pending_widget_keys(&keys.to_str_lossy()));
    }
    Ok(output)
  }

  fn ex_expand(&mut self, cmd: &EditCmd, range: Option<AddressRange>, bang: bool) -> ShResult<()> {
    let range = range.unwrap_or_else(AddressRange::all_lines); // expands entire buffer
    let verb = if bang { Verb::ExpandAll } else { Verb::Expand };

    self.replace_verb(cmd, verb, &range)
  }
  fn ex_delete(&mut self, cmd: &EditCmd, range: Option<AddressRange>) -> ShResult<()> {
    let range = range.unwrap_or_default();
    self.replace_verb(cmd, Verb::Delete, &range)
  }
  fn ex_yank(&mut self, cmd: &EditCmd, range: Option<AddressRange>) -> ShResult<()> {
    let range = range.unwrap_or_default();
    self.replace_verb(cmd, Verb::Yank, &range)
  }
  fn ex_put(&mut self, cmd: &EditCmd, anchor: Anchor, range: Option<AddressRange>) -> ShResult<()> {
    let range = range.unwrap_or_default();
    self.replace_verb(cmd, Verb::Put(anchor), &range)
  }
  fn ex_join(&mut self, cmd: &EditCmd, range: Option<AddressRange>) -> ShResult<()> {
    let range = range.unwrap_or_default();
    self.replace_verb(cmd, Verb::JoinLines, &range)
  }

  fn replace_verb(&mut self, cmd: &EditCmd, verb: Verb, range: &AddressRange) -> ShResult<()> {
    // TODO: this can probably be performed way before we get here
    let verb = Some(verb!(verb));
    let motion = Some(motion!(range.as_motion()));

    let new_cmd = EditCmd {
      verb,
      motion,
      ..cmd.clone()
    };
    self.exec_verb(&new_cmd)
  }

  /// Recursively determines the set of lines an `ExNode` operates on.
  ///
  /// - Leaf nodes (Normal, Delete, Substitute, etc.) use the node's address
  ///   (current row if None).
  /// - Global nodes resolve their pattern within the node's address scope.
  /// - Nested Globals INTERSECT: `:g/A/g/B/cmd` operates on lines matching A AND B.
  ///   Each layer of Global narrows the set.
  pub(crate) fn lines_for_ex_node(&self, node: &ExNode) -> ShResult<Vec<usize>> {
    match &node.kind {
      ExNdRule::Global { pat, nested } => {
        let range = node.address.clone().unwrap_or_else(AddressRange::all_lines);
        let constraint = range.as_motion();
        let outer = self.get_matching_lines(&constraint, pat, node.bang)?;

        // If the nested node also narrows the set (another Global), intersect.
        // Otherwise the outer match is the answer.
        if matches!(nested.kind, ExNdRule::Global { .. }) {
          let inner = self.lines_for_ex_node(nested)?;
          let outer_set: HashSet<usize> = outer.iter().copied().collect();
          Ok(
            inner
              .into_iter()
              .filter(|l| outer_set.contains(l))
              .collect(),
          )
        } else {
          Ok(outer)
        }
      }
      _ => self.lines_for_address(node.address.as_ref()),
    }
  }

  pub(crate) fn lines_for_address(&self, addr: Option<&AddressRange>) -> ShResult<Vec<usize>> {
    match addr {
      None => Ok(vec![self.row()]),
      Some(AddressRange::Single(a)) => {
        let line = self.resolve_line_addr(a)?.unwrap_or(self.row());
        Ok(vec![line])
      }
      Some(AddressRange::Range(s, e)) => {
        let s = self.resolve_line_addr(s)?.unwrap_or(self.row());
        let e = self.resolve_line_addr(e)?.unwrap_or(self.row());
        let (s, e) = util::ordered(s, e);
        Ok((s..=e).collect())
      }
    }
  }
}

/// Build a repeat command that uses the saved verb's *kind* (pat/repl/flags
/// or pattern/nested) but the NEW cmd's address. Without this, `:%s` after a
/// `:s/foo/bar/` would replay the old substitute on the OLD scope (current
/// line) instead of the new `%` scope.
fn merge_repeat_addr(new_cmd: &EditCmd, saved: &EditCmd) -> Option<EditCmd> {
  // Pull the saved kind
  let Some(Cmd(_, Verb::ExCmd(saved_node))) = saved.verb.as_ref() else {
    return None;
  };
  // Pull the new address (if any)
  let new_address = match new_cmd.verb.as_ref() {
    Some(Cmd(_, Verb::ExCmd(node))) => node.address.clone(),
    _ => None,
  };

  let new_node = ExNode {
    address: new_address,
    bang: saved_node.bang,
    kind: saved_node.kind.clone(),
  };

  Some(EditCmd {
    verb: Some(verb!(Verb::ExCmd(new_node))),
    ..new_cmd.clone()
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::readline::LineBuf;
  use crate::readline::stash::Stash;
  use crate::tests::testutil::TestGuard;

  fn make_buf(content: &str) -> LineBuf {
    let mut buf = LineBuf::default();
    buf.set_buffer(content);
    buf
  }

  // ─── Push ────────────────────────────────────────────────────────────

  #[test]
  fn stash_push_clears_buffer_and_persists_to_stack() {
    let _g = TestGuard::new();
    let mut buf = make_buf("echo hello");
    buf.ex_stash(&StashArgs::Push(None)).unwrap();
    assert_eq!(buf.to_string(), "", "push should clear the buffer");

    let stash = Stash::new().unwrap();
    assert_eq!(stash.stack_len(), 1, "stack should have one entry");
  }

  #[test]
  fn stash_push_named_does_not_increase_stack_count() {
    let _g = TestGuard::new();
    let mut buf = make_buf("named cmd");
    buf
      .ex_stash(&StashArgs::Push(Some("my_name".into())))
      .unwrap();
    let stash = Stash::new().unwrap();
    // Named entries don't count toward stack_len (only unnamed do).
    assert_eq!(stash.stack_len(), 0);
    // But the named entry should be retrievable.
    let entry = stash.get("my_name").unwrap();
    assert!(entry.is_some());
    assert_eq!(entry.unwrap().buffer, "named cmd");
  }

  #[test]
  fn stash_push_on_empty_buffer_is_noop() {
    let _g = TestGuard::new();
    let mut buf = LineBuf::default();
    buf.ex_stash(&StashArgs::Push(None)).unwrap();
    let stash = Stash::new().unwrap();
    assert_eq!(stash.stack_len(), 0, "empty push should not persist");
  }

  // ─── Pop ─────────────────────────────────────────────────────────────

  #[test]
  fn stash_pop_restores_buffer_and_removes_from_stack() {
    let _g = TestGuard::new();
    let mut buf = make_buf("first cmd");
    buf.ex_stash(&StashArgs::Push(None)).unwrap();
    assert_eq!(buf.to_string(), "");

    buf.ex_stash(&StashArgs::Pop(None)).unwrap();
    assert_eq!(buf.to_string(), "first cmd");
    let stash = Stash::new().unwrap();
    assert_eq!(stash.stack_len(), 0);
  }

  #[test]
  fn stash_pop_empty_is_noop() {
    let _g = TestGuard::new();
    let mut buf = make_buf("starting content");
    buf.ex_stash(&StashArgs::Pop(None)).unwrap();
    // Buffer untouched when there's nothing to pop.
    assert_eq!(buf.to_string(), "starting content");
  }

  #[test]
  fn stash_pop_by_index() {
    let _g = TestGuard::new();
    let mut buf = make_buf("entry one");
    buf.ex_stash(&StashArgs::Push(None)).unwrap();
    buf.set_buffer("entry two");
    buf.ex_stash(&StashArgs::Push(None)).unwrap();
    // stack now: [entry one, entry two] (idx 0 oldest, idx 1 newest)

    buf.ex_stash(&StashArgs::Pop(Some("0".into()))).unwrap();
    assert_eq!(buf.to_string(), "entry one");
    let stash = Stash::new().unwrap();
    assert_eq!(stash.stack_len(), 1, "only one left after popping idx 0");
  }

  // ─── Drop ────────────────────────────────────────────────────────────

  #[test]
  fn stash_drop_removes_entry_without_touching_buffer() {
    let _g = TestGuard::new();
    let mut buf = make_buf("to be stashed");
    buf.ex_stash(&StashArgs::Push(None)).unwrap();
    buf.set_buffer("current work");

    buf.ex_stash(&StashArgs::Drop(Some("0".into()))).unwrap();
    assert_eq!(
      buf.to_string(),
      "current work",
      "drop should not modify the buffer"
    );
    let stash = Stash::new().unwrap();
    assert_eq!(stash.stack_len(), 0);
  }

  // ─── Apply ───────────────────────────────────────────────────────────

  #[test]
  fn stash_apply_named_replaces_buffer_but_keeps_entry() {
    let _g = TestGuard::new();
    let mut buf = make_buf("saved cmd");
    buf.ex_stash(&StashArgs::Push(Some("snap".into()))).unwrap();
    buf.set_buffer("now editing");

    buf
      .ex_stash(&StashArgs::Apply(Some("snap".into())))
      .unwrap();
    assert_eq!(buf.to_string(), "saved cmd");

    // Apply should NOT remove the named entry.
    let stash = Stash::new().unwrap();
    assert!(stash.get("snap").unwrap().is_some());
  }

  #[test]
  fn stash_apply_unknown_name_is_noop() {
    let _g = TestGuard::new();
    let mut buf = make_buf("current");
    buf
      .ex_stash(&StashArgs::Apply(Some("nope_doesnt_exist".into())))
      .unwrap();
    assert_eq!(buf.to_string(), "current");
  }

  // ─── Insert ──────────────────────────────────────────────────────────

  #[test]
  fn stash_insert_pastes_into_existing_buffer() {
    let _g = TestGuard::new();
    let mut buf = make_buf("stash content");
    buf
      .ex_stash(&StashArgs::Push(Some("piece".into())))
      .unwrap();

    buf.set_buffer("hello world");
    // Cursor defaults to (0,0); insert at start.
    buf
      .ex_stash(&StashArgs::Insert(Some("piece".into())))
      .unwrap();
    // The stashed content gets pasted; exact concatenation depends on
    // insert_lines_at semantics, but the result must contain both.
    let result = buf.to_string();
    assert!(result.contains("stash content"), "got: {result:?}");
    assert!(result.contains("hello world"), "got: {result:?}");
  }

  // ─── List ────────────────────────────────────────────────────────────

  #[test]
  fn stash_list_does_not_panic_on_empty_or_populated_stash() {
    let _g = TestGuard::new();
    let mut buf = make_buf("");
    // Empty case — should post a "no entries" status_msg, not panic.
    buf.ex_stash(&StashArgs::List(None)).unwrap();
    buf
      .ex_stash(&StashArgs::List(Some(StashListArg::Stack)))
      .unwrap();
    buf
      .ex_stash(&StashArgs::List(Some(StashListArg::Named)))
      .unwrap();

    // Populate and list again.
    buf.set_buffer("on the stack");
    buf.ex_stash(&StashArgs::Push(None)).unwrap();
    buf.set_buffer("by name");
    buf
      .ex_stash(&StashArgs::Push(Some("alpha".into())))
      .unwrap();

    buf.ex_stash(&StashArgs::List(None)).unwrap();
    buf
      .ex_stash(&StashArgs::List(Some(StashListArg::Stack)))
      .unwrap();
    buf
      .ex_stash(&StashArgs::List(Some(StashListArg::Named)))
      .unwrap();
  }

  // ===================== run_shell_cmd =====================

  mod run_shell_cmd_tests {
    use super::*;
    use crate::tests::testutil::has_cmd;

    // ─── stdin path: capture_command ────────────────────────────────

    #[test]
    fn with_stdin_captures_command_output() {
      if !has_cmd("cat") {
        return;
      }
      let _g = TestGuard::new();
      let mut buf = make_buf("");
      let out = buf
        .run_shell_cmd("cat", Some("hello-from-stdin\n"))
        .unwrap()
        .unwrap();
      assert!(out.contains("hello-from-stdin"), "got: {out:?}");
    }

    #[test]
    fn with_empty_stdin_runs_command() {
      // `echo` is a shell builtin in shed, so no external binary is
      // required here — no `has_cmd` guard needed.
      let _g = TestGuard::new();
      let mut buf = make_buf("");
      let out = buf
        .run_shell_cmd("echo captured", Some(""))
        .unwrap()
        .unwrap();
      assert!(out.contains("captured"), "got: {out:?}");
    }

    // ─── no-stdin path: exec_int ───────────────────────────────────
    //
    // Note: the no-stdin path runs via `exec_int` (interactive mode).
    // External commands (`true`, `:`) would fork and trigger
    // `wait_fg → attach → tcsetpgrp`, which fails with ENOTTY in the
    // test harness (the test process isn't in the pty's session).
    // We restrict no-stdin tests to assignment commands which run
    // in-process without forking.

    #[test]
    fn without_stdin_returns_none() {
      let _g = TestGuard::new();
      let mut buf = make_buf("original");
      let result = buf.run_shell_cmd("BUFFER=stays", None).unwrap();
      assert_eq!(result, None);
    }

    // ─── BUFFER var modifications round-trip into LineBuf ──────────

    #[test]
    fn buffer_var_set_in_shell_cmd_updates_linebuf() {
      let _g = TestGuard::new();
      let mut buf = make_buf("original");
      // The command runs in the current shell process; assigning
      // BUFFER updates the shell var, which the function reads back.
      buf.run_shell_cmd("BUFFER=replaced", None).unwrap();
      assert_eq!(buf.to_string(), "replaced");
    }

    // ─── CURSOR var modifications round-trip ───────────────────────

    #[test]
    fn cursor_var_set_to_valid_pos_moves_cursor() {
      let _g = TestGuard::new();
      let mut buf = make_buf("abcdef");
      buf.set_cursor_from_flat(0);
      buf.run_shell_cmd("CURSOR=3", None).unwrap();
      assert_eq!(buf.cursor_to_flat(), 3);
    }

    #[test]
    fn cursor_var_invalid_falls_back_to_original_position() {
      let _g = TestGuard::new();
      let mut buf = make_buf("abcdef");
      buf.set_cursor_from_flat(2);
      // 'garbage' isn't a valid flat index or row:col → falls back.
      buf.run_shell_cmd("CURSOR=garbage", None).unwrap();
      assert_eq!(buf.cursor_to_flat(), 2);
    }

    // ─── BUFFER + CURSOR together — typical widget pattern ────────

    #[test]
    fn buffer_and_cursor_replacement_together() {
      let _g = TestGuard::new();
      let mut buf = make_buf("original");
      buf.run_shell_cmd("BUFFER=replaced;CURSOR=4", None).unwrap();
      assert_eq!(buf.to_string(), "replaced");
      assert_eq!(buf.cursor_to_flat(), 4);
    }

    // ─── Stdin path doesn't get to mutate shell-side BUFFER ───────

    #[test]
    fn stdin_path_does_not_round_trip_buffer_via_child() {
      // With stdin, capture_command forks. The child can read BUFFER
      // (it's exported) but its writes to BUFFER stay in the child's
      // env and never reach the parent's shell vars. So the parent
      // restores the original buffer regardless of what the child did.
      let _g = TestGuard::new();
      let mut buf = make_buf("stays_the_same");
      buf
        .run_shell_cmd("BUFFER=should_not_leak", Some(""))
        .unwrap();
      assert_eq!(buf.to_string(), "stays_the_same");
    }
  }

  // ===================== ex_write =====================

  mod ex_write_tests {
    use super::*;
    use crate::readline::editcmd::WriteDest;

    #[test]
    fn write_to_file_creates_and_truncates() {
      let _g = TestGuard::new();
      let dir = tempfile::TempDir::new().unwrap();
      let path = dir.path().join("out.txt");
      // Pre-existing content should be truncated.
      std::fs::write(&path, "OLD_CONTENT_TO_BE_OVERWRITTEN").unwrap();
      let mut buf = make_buf("new buffer content");
      buf.ex_write(&WriteDest::File(Some(path.clone()))).unwrap();
      let content = std::fs::read_to_string(&path).unwrap();
      assert_eq!(content, "new buffer content\n");
    }

    #[test]
    fn write_to_file_creates_when_missing() {
      let _g = TestGuard::new();
      let dir = tempfile::TempDir::new().unwrap();
      let path = dir.path().join("newfile.txt");
      let mut buf = make_buf("hello");
      buf.ex_write(&WriteDest::File(Some(path.clone()))).unwrap();
      assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello\n");
    }

    #[test]
    fn write_to_file_append_does_not_truncate() {
      let _g = TestGuard::new();
      let dir = tempfile::TempDir::new().unwrap();
      let path = dir.path().join("append.txt");
      std::fs::write(&path, "first\n").unwrap();
      let mut buf = make_buf("second");
      buf
        .ex_write(&WriteDest::FileAppend(Some(path.clone())))
        .unwrap();
      let content = std::fs::read_to_string(&path).unwrap();
      assert_eq!(content, "first\nsecond\n");
    }

    #[test]
    fn write_to_file_in_unwritable_dir_is_silent_ok() {
      // Pinning current behavior: ex_write swallows open errors and
      // returns Ok (the user sees a system_msg about it).
      let _g = TestGuard::new();
      let mut buf = make_buf("anything");
      let bad_path = std::path::PathBuf::from("/this/does/not/exist/xyz/out.txt");
      let res = buf.ex_write(&WriteDest::File(Some(bad_path)));
      assert!(res.is_ok());
    }

    // Note: `WriteDest::Cmd` pipes the buffer to a shell command via
    // exec_nonint. Forking shell commands in the test harness can hit
    // tcsetpgrp ENOTTY issues; we skip direct testing of that arm.
  }

  // ===================== ex_read =====================

  mod ex_read_tests {
    use super::*;
    use crate::readline::editcmd::ReadSrc;

    #[test]
    fn read_file_inserts_contents_into_buffer() {
      let _g = TestGuard::new();
      let dir = tempfile::TempDir::new().unwrap();
      let path = dir.path().join("in.txt");
      std::fs::write(&path, "line one\nline two\n").unwrap();
      let mut buf = make_buf("");
      buf.ex_read(&ReadSrc::File(path));
      let joined = buf.to_string();
      assert!(joined.contains("line one"), "got: {joined:?}");
      assert!(joined.contains("line two"), "got: {joined:?}");
    }

    #[test]
    fn read_missing_file_is_silent_ok() {
      // Non-existent paths are not errors — `system_msg!` informs the
      // user and the function returns Ok with the buffer untouched.
      let _g = TestGuard::new();
      let mut buf = make_buf("original");
      let bad_path = std::path::PathBuf::from("/this/does/not/exist/zzz.txt");
      buf.ex_read(&ReadSrc::File(bad_path));
      assert_eq!(buf.to_string(), "original");
    }

    #[test]
    fn read_directory_path_is_silent_ok() {
      // path.is_file() returns false for a directory → same not-a-file
      // branch as a missing path.
      let _g = TestGuard::new();
      let dir = tempfile::TempDir::new().unwrap();
      let mut buf = make_buf("untouched");
      buf.ex_read(&ReadSrc::File(dir.path().to_path_buf()));
      assert_eq!(buf.to_string(), "untouched");
    }

    #[test]
    fn read_file_clears_indent_cache() {
      // ex_read sets self.indent_cache = None as part of the successful
      // path. We pin this by reading something in and checking the
      // cache afterward.
      let _g = TestGuard::new();
      let dir = tempfile::TempDir::new().unwrap();
      let path = dir.path().join("c.txt");
      std::fs::write(&path, "x").unwrap();
      let mut buf = make_buf("");
      buf.ex_read(&ReadSrc::File(path));
      assert!(buf.indent_cache.is_none());
    }

    // Note: `ReadSrc::Cmd` runs capture_command which forks. Like
    // `WriteDest::Cmd` above, that arm is skipped due to fork/tty
    // issues in the test harness.
  }

  // ===================== ex_shell_cmd =====================

  mod ex_shell_cmd_tests {
    use super::*;
    use crate::readline::editcmd::LineAddr;

    /// No motion attached → `ex_shell_cmd` falls through to
    /// `run_shell_cmd(_, None)` and the buffer is mutated only by
    /// side effects the shell command itself causes.
    #[test]
    fn no_motion_runs_command_without_replacing_lines() {
      let _g = TestGuard::new();
      let mut buf = make_buf("first\nsecond\nthird");
      // Assigning BUFFER inside the no-stdin path updates the linebuf
      // via run_shell_cmd's read-back. Use it to confirm the command
      // ran rather than the line-range path being taken.
      buf.ex_shell_cmd(None, "BUFFER=after_no_motion").unwrap();
      assert_eq!(buf.to_string(), "after_no_motion");
    }

    /// A single-line address (`:2!cmd`) filters exactly that line through
    /// the command and splices the result back in place, leaving the
    /// surrounding lines untouched. Uses a *transforming* filter (`tr`) with
    /// an exact-equality assertion so an identity filter can't mask a
    /// never-drained target line (the shape of the earlier regression).
    #[test]
    fn single_addr_filters_only_that_line() {
      use crate::tests::testutil::has_cmd;
      if !has_cmd("tr") {
        return;
      }
      let _g = TestGuard::new();
      let mut buf = make_buf("alpha\nbeta\ngamma");
      buf
        .ex_shell_cmd(
          Some(&AddressRange::Single(LineAddr::Number(2))),
          "tr a-z A-Z",
        )
        .unwrap();
      assert_eq!(buf.to_string(), "alpha\nBETA\ngamma");
    }

    /// A range address (`:1,2!cmd`) filters the whole inclusive span.
    #[test]
    fn range_addr_filters_whole_span() {
      use crate::tests::testutil::has_cmd;
      if !has_cmd("tr") {
        return;
      }
      let _g = TestGuard::new();
      let mut buf = make_buf("alpha\nbeta\ngamma");
      buf
        .ex_shell_cmd(
          Some(&AddressRange::Range(
            LineAddr::Number(1),
            LineAddr::Number(2),
          )),
          "tr a-z A-Z",
        )
        .unwrap();
      assert_eq!(buf.to_string(), "ALPHA\nBETA\ngamma");
    }
  }
}
