use std::{fmt::Write, io::Write as IoWrite, sync::atomic::AtomicBool};

use tempfile::NamedTempFile;

use crate::{
  match_loop, out, parse::{
    NdRule, Node,
    execute::{Dispatcher, exec_input},
    lex::{Span, Tk},
  }, prelude::*, readline::{
    history::{HistEntry, History},
    linebuf::ordered,
  }, sherr, shopt::xtrace_print, state::{self}, util::error::{ShResult, ShResultExt}
};

/// POSIX specifies that an invocation of `fc` that edits and re-executes a command shall not itself be committed to command history
/// This flag is checked in main and gates history writing.
pub static NO_HIST_SAVE: AtomicBool = AtomicBool::new(false);

bitflags! {
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub struct FixCmdFlags: u32 {
  }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RangeArg {
  Number(i32),
  Prefix(String),
}

impl Default for RangeArg {
  fn default() -> Self {
    Self::Number(-1)
  }
}

#[derive(Debug, Default)]
pub struct FixCmdOpts {
  editor: Option<String>,
  replace: Option<(String, String)>,
  first: Option<RangeArg>,
  last: Option<RangeArg>,
  list: bool,
  no_numbers: bool,
  reverse: bool,
  no_editor: bool,
}

pub fn parse_fc_args(args: Vec<Tk>) -> ShResult<(Vec<(String, Span)>, FixCmdOpts)> {
  let mut args = args.into_iter();
  args.next(); // skip "fc" command itself

  let mut words: Vec<(String, Span)> = vec![];
  let mut opts = FixCmdOpts::default();
  for tk in args {
    let span = tk.span.clone();
    let expanded = tk.expand()?;
    for word in expanded.get_words() {
      words.push((word, span.clone()));
    }
  }

  xtrace_print(&words);

  let mut words_iter = words.into_iter().peekable();
  let mut non_opts = vec![];

  while let Some((word, span)) = words_iter.next() {
    if word == "--" {
      non_opts.push((word, span));
      non_opts.extend(words_iter);
      break;
    }

    if let Ok(num) = word.parse::<i32>()
      && num != 0
    {
      if opts.first.is_none() {
        opts.first = Some(RangeArg::Number(num));
      } else if opts.last.is_none() {
        opts.last = Some(RangeArg::Number(num));
      } else {
        non_opts.push((word, span));
      }
      continue;
    }

    if opts.no_editor {
      let mut old = String::new();
      let mut new = String::new();
      let mut chars = word.chars();
      match_loop!(chars.next() => ch, {
        '\\' => {
          old.push(ch);
          if let Some(next_ch) = chars.next() {
            old.push(next_ch);
          }
        }
        '=' => {
          new = chars.collect();
          break;
        }
        _ => old.push(ch),
      });

      if !new.is_empty() {
        if opts.replace.is_none() {
          opts.replace = Some((old, new));
        } else {
          non_opts.push((word, span));
        }
        continue;
      }
    }

    match word.as_str() {
      "-r" => opts.reverse = true,
      "-n" => opts.no_numbers = true,
      "-s" => opts.no_editor = true,
      "-l" => opts.list = true,
      "-e" => {
        let Some((word, _)) = words_iter.next() else {
          return Err(sherr!(ParseErr @ span, "Option -e requires an argument"));
        };
        opts.editor = Some(word);
      }
      _ => {
        if opts.first.is_none() {
          opts.first = Some(RangeArg::Prefix(word));
        } else if opts.last.is_none() {
          opts.last = Some(RangeArg::Prefix(word));
        } else {
          non_opts.push((word, span));
        }
      }
    }
  }

  Ok((non_opts, opts))
}

pub(super) struct FixCmd;
impl super::Builtin for FixCmd {
  fn execute(&self, _args: super::BuiltinArgs) -> ShResult<()> {
    unreachable!("fixcmd is a special snowflake command that needs really special handling");
  }
  fn run_builtin(
    &self,
    node: Node,
    _dispatcher: &mut Dispatcher
  ) -> ShResult<()> {
    let span = node.get_span();
    let NdRule::Command {
      assignments: _,
      argv,
    } = node.class
    else {
      unreachable!()
    };

    let (_argv, opts) = parse_fc_args(argv).promote_err(span.clone())?;

    let conn = state::get_db_conn()
      .ok_or_else(|| sherr!(InternalErr, "database not available"))
      .promote_err(span.clone())?;
    let hist = History::new(conn, "shed_history").promote_err(span.clone())?;
    if opts.list {
      fc_list(hist, opts).promote_err(span)?;
    } else if opts.no_editor {
      fc_reexec(hist, opts).promote_err(span)?;
    } else {
      fc_edit(hist, opts).promote_err(span)?;
    }

    state::set_status(0);
    Ok(())
  }
}

fn fc_edit(hist: History, opts: FixCmdOpts) -> ShResult<()> {
  let editor = if let Some(editor) = opts.editor {
    editor
  } else if let Ok(editor) = env::var("FCEDIT") {
    editor
  } else if let Ok(editor) = env::var("EDITOR") {
    editor
  } else {
    return Err(sherr!(ExecFail, "No editor specified for fc command"));
  };
  let first = opts.first.unwrap_or_default();
  let last = opts.last.unwrap_or(first.clone());

  let entries = get_entry_range(&hist, Some(first), Some(last), opts.reverse)?;
  let mut should_push;

  NO_HIST_SAVE.store(true, std::sync::atomic::Ordering::SeqCst);
  for (_, entry) in entries {
    let old_cmd = entry.command;
    let mut new_cmd = String::new();

    let mut tmp = NamedTempFile::new()?;
    tmp.write_all(old_cmd.as_bytes())?;
    tmp.flush()?;

    let editor_cmd = format!("{editor} {}", tmp.path().display());

    exec_input(editor_cmd, None, Some("fc edit".into()))?;

    tmp.rewind()?;
    tmp.read_to_string(&mut new_cmd)?;
    new_cmd = new_cmd.trim().to_string();

    should_push = new_cmd != old_cmd;

    exec_input(new_cmd.clone(), None, Some("fc re-exec".into()))?;

    if should_push {
      hist.push(new_cmd)?;
    }
  }

  Ok(())
}

fn fc_reexec(hist: History, opts: FixCmdOpts) -> ShResult<()> {
  let first = opts.first.unwrap_or_default();
  let last = opts.last.unwrap_or(first.clone());
  let entries = get_entry_range(&hist, Some(first), Some(last), opts.reverse)?;

  NO_HIST_SAVE.store(true, std::sync::atomic::Ordering::SeqCst);
  for (_, entry) in entries {
    let mut command = entry.command;
    let mut should_push = false;
    if let Some((old, new)) = &opts.replace {
      let new_cmd = command.replace(old, new);
      if new_cmd != command {
        command = new_cmd;
        should_push = true;
      }
    }

    exec_input(command.clone(), None, Some("fc re-exec".into()))?;
    if should_push {
      hist.push(command)?;
    }
  }

  Ok(())
}

fn fc_list(hist: History, opts: FixCmdOpts) -> ShResult<()> {
  let first = if let Some(first) = opts.first {
    first
  } else {
    RangeArg::Number(-16)
  };
  let last = opts.last.clone().unwrap_or_default();

  let entries = get_entry_range(&hist, Some(first), Some(last), opts.reverse)?;

  let mut buf = String::new();
  for (id, entry) in entries {
    let cmd = entry.command;
    if !opts.no_numbers {
      write!(buf, "{}\t", id).unwrap();
    }
    buf.push_str(&cmd);
    buf.push('\n');
  }

  out!("{}", buf)?;

  Ok(())
}

fn get_entry_range(
  hist: &History,
  first: Option<RangeArg>,
  last: Option<RangeArg>,
  reverse: bool,
) -> ShResult<Vec<(i64, HistEntry)>> {
  let last_id = hist.last_id();

  let resolve = |arg: &RangeArg| -> ShResult<i64> {
    match arg {
      RangeArg::Number(n) if *n < 0 => Ok(last_id + 1 + (*n - 1) as i64),
      RangeArg::Number(n) => Ok(*n as i64),
      RangeArg::Prefix(p) => Ok(
        hist
          .query_by_prefix(p)?
          .map(|(id, _)| id)
          .unwrap_or(last_id),
      ),
    }
  };

  let first_id = resolve(&first.unwrap_or(RangeArg::Number(last_id as i32)))?;
  let last_id = resolve(&last.unwrap_or(RangeArg::Number(first_id as i32)))?;

  let (lo, hi) = ordered(first_id, last_id);

  let mut entries = hist.query_range(lo, hi)?;
  if reverse || first_id > last_id {
    entries.reverse();
  }
  Ok(entries)
}
