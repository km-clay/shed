//! The [`thru`](`Thru`) builtin
use std::{fs, io, sync::Arc};

use bstr::ByteSlice;

use crate::{
  builtin::{BuiltinArgs, opt::OptSpec},
  errln,
  eval::lex::Span,
  opt,
  procio::{self, OsSink, Sink},
  sherr, signal,
  state::vars::VarStr,
  util::{self, error::ShResult},
};

struct ThruOpts {
  count : bool,
  append: bool,
  tee   : Option<VarStr>,
  take  : Option<usize>,
  skip  : Option<usize>,
  from  : Option<u8>,
  until : Option<u8>,
}

/// Primitive command that simply reads bytes from stdin or files and writes to stdout or a variable.
///
/// Has several options for precisely controlling the flow of input and output,
/// effectively making this command a "valve" for pipelines.
pub(super) struct Thru;
impl super::Builtin for Thru {
  fn strict_opts(&self) -> bool {
    true
  }
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("count" | b'c'),
      opt!("append" | b'a'),
      opt!("tee" | b't', 1),
      opt!("limit" | b'L', 1),
      opt!("take" | b'T', 1),
      opt!("skip" | b'S', 1),
      opt!("from" | b'F', 1),
      opt!("until" | b'U', 1),
    ]
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    args.opt_value("limit").inspect(|_| {
      errln!("thru: warning: `-L`/`--limit` are deprecated, use `-T`/`--take` instead");
    });
    let mut status = 0;
    let mut set_status = |s: i32| {
      if status < s {
        status = s;
      }
    };
    let ThruOpts {
      count,
      append,
      tee,
      skip,
      mut take,
      mut from,
      mut until,
    } = Self::parse_opts(&args)?;

    let mut tee_file: Option<Arc<dyn Sink>> = tee
      .map(|dest| {
        if append {
          std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&dest)
        } else {
          std::fs::File::create(&dest)
        }
        .map(|f| Arc::new(OsSink::new(f.into())) as Arc<dyn Sink>)
        .inspect_err(|e| {
          errln!("thru: failed to open {dest} for writing: {e}");
        })
      })
      .transpose()
      .ok()
      .flatten();

    let mut sources: Vec<Option<(VarStr, Span)>> = args
      .arguments()
      .map(|(a, s)| (a.to_str_lossy() != "-").then(|| (a.clone(), s)))
      .collect();
    if sources.is_empty() {
      sources.push(None);
    }

    let mut byte_count = 0;
    let mut skip       = skip.unwrap_or(0);
    'sources: for src in sources {
      if take == Some(0) {
        break;
      }

      let reader: Arc<dyn Sink> = match &src {
        Some((path, p_span)) => match fs::File::open(path) {
          Ok(f) => Arc::new(OsSink::new(f.into())),
          Err(e) => {
            sherr!(ExecFail @ *p_span, "{e}").print_error();
            continue;
          }
        },
        None => match procio::stdin_sink() {
          Ok(s) => s,
          Err(e) => {
            sherr!(ExecFail @ args.cmd_span(), "{e}").print_error();
            continue;
          }
        },
      };
      let span = src.map(|(_, s)| s);
      let can_seek =
        (from.is_some() || until.is_some()) && reader.seek(io::SeekFrom::Current(0)).is_ok();

      let mut buf = procio::take_scratch();
      loop {
        let window = match take {
          Some(l) => skip.saturating_add(l),
          None    => buf.len(),
        };
        if window == 0 {
          break;
        }

        let cap = if (from.is_some() || until.is_some()) && !can_seek {
          if skip > 0 {
            // bulk read until we are done skipping
            skip.min(buf.len().min(window))
          } else {
            // now read one at a time until we hit the delimiter
            1
          }
        } else {
          buf.len().min(window)
        };
        if cap == 0 {
          break;
        }

        let n = match reader.read(&mut buf[..cap]) {
          Ok(0) => break,
          Ok(n) => n,
          Err(e) => match e.kind() {
            io::ErrorKind::WouldBlock => {
              set_status(6);
              break;
            }
            io::ErrorKind::Interrupted => {
              signal::check_signals()?;
              continue;
            }
            _ => {
              sherr!(IoErr(e.kind()), "error reading input: {e}")
                .option_promote(span)
                .print_error();
              break;
            }
          },
        };

        let chunk   = &buf[..n];
        let dropped = skip.min(n);
        skip -= dropped;
        let mut emit = &chunk[dropped..];
        if skip > 0 || emit.is_empty() {
          continue;
        }

        if let Some(delim) = from {
          if (can_seek && Self::rewind_past(&*reader, emit, delim)?.is_some())
            || emit.first() == Some(&delim)
          {
            from = None;
          }
          continue;
        }

        if let Some(delim) = until {
          if can_seek {
            if let Some(pos) = Self::rewind_past(&*reader, emit, delim)? {
              emit = &emit[..pos];
              procio::out_bytes(emit);
              if let Some(t) = tee_file.as_mut() {
                t.write_all(emit).ok();
              }
              byte_count += emit.len();
              until = None;
              take = None;
              break 'sources;
            }
          } else if emit.first() == Some(&delim) {
            until = None;
            take = None;
            break 'sources;
          }
        }

        if until.is_some() && emit.first() == until.as_ref() {
          until = None;
          take = None;
          break 'sources;
        }

        if let Some(l) = take {
          emit = &emit[..emit.len().min(l)];
        }

        procio::out_bytes(emit);

        if let Some(t) = tee_file.as_mut() {
          t.write_all(emit).ok();
        }

        byte_count += emit.len();
        if let Some(l) = take.as_mut() {
          *l -= emit.len();
        }
      }
    }

    if count {
      errln!("{byte_count}");
    }

    // thru exit statuses:
    // 1: we read 0 bytes (EOF)
    // 2: usage error/bad option
    // 3: -T is set, and we didn't take the number of bytes requested
    // 4: -F is set, and we never found the target byte
    // 5: -U is set, and we never found the target byte

    if byte_count == 0 {
      // note: this also includes the "-S is set, and we skipped everything" case
      set_status(1);
    }

    if byte_count > 0 && take.is_some_and(|t| t > 0) {
      // 2 is reserved for usage errors
      set_status(3);
    }

    if until.is_some() {
      set_status(4);
    }

    if from.is_some() {
      set_status(5);
    }

    util::with_status(status)
  }
}

impl Thru {
  fn parse_opts(args: &BuiltinArgs) -> ShResult<ThruOpts> {
    let count  = args.has_opt("count");
    let append = args.has_opt("append");
    let tee    = args.opt_value("tee");
    let take = args
      .opt_value("take")
      .or_else(|| args.opt_value("limit"))
      .map(|s| {
        let span = args
          .opt_span("take")
          .or_else(|| args.opt_span("limit"))
          .unwrap();
        s.parse::<usize>()
          .map_err(|v| sherr!(InvalidOpt @ span, "invalid limit: '{v}'").with_code(2))
      })
      .transpose()?;
    let skip = args
      .opt_value("skip")
      .map(|s| {
        s.parse::<usize>().map_err(|v| {
          sherr!(InvalidOpt @ args.opt_span("skip").unwrap(), "invalid skip: '{v}'").with_code(2)
        })
      })
      .transpose()?;
    let from: Option<u8> = args
      .opt_value("from")
      .map(|s| {
        if s.len() == 1 {
          Ok(s.as_bytes()[0])
        } else {
          Err(sherr!(InvalidOpt @ args.opt_span("from").unwrap(), "invalid from: must be a single byte").with_code(2))
        }
      })
      .transpose()?;
    let until: Option<u8> = args
      .opt_value("until")
      .map(|s| {
        if s.len() == 1 {
          Ok(s.as_bytes()[0])
        } else {
          Err(sherr!(InvalidOpt @ args.opt_span("until").unwrap(), "invalid until: must be a single byte").with_code(2))
        }
      })
      .transpose()?;

    Ok(ThruOpts {
      count,
      append,
      tee,
      take,
      skip,
      from,
      until,
    })
  }

  /// The seeking fast path for delim scanning
  ///
  /// Works on stuff like files, but not pipes since those cannot be lseek'd
  fn rewind_past(sink: &dyn Sink, chunk: &[u8], delim: u8) -> io::Result<Option<usize>> {
    match chunk.find_byte(delim) {
      Some(pos) => {
        let leftover = chunk.len() - (pos + 1);
        if leftover > 0 {
          sink.seek(io::SeekFrom::Current(-(leftover as i64)))?;
        }
        Ok(Some(pos))
      }
      None => Ok(None),
    }
  }
}

#[cfg(test)]
mod tests {
  use crate::state::Shed;
  use crate::tests::testutil::{TestGuard, test_input};

  fn status_of(cmd: &str) -> i32 {
    let _g = TestGuard::new();
    test_input(cmd).unwrap();
    Shed::get_status()
  }

  #[test]
  fn take_full_is_zero() {
    assert_eq!(status_of(r"printf '0123' | thru -T 4 >/dev/null"), 0);
  }

  #[test]
  fn take_partial_is_short_take() {
    assert_eq!(status_of(r"printf '01' | thru -T 4 >/dev/null"), 3);
  }

  #[test]
  fn take_zero_bytes_is_clean_eof_not_short_take() {
    // A -T read that gets nothing is a clean boundary (1), not a truncation (3);
    // this is what lets `while thru -T n` terminate cleanly.
    assert_eq!(status_of(r"printf '' | thru -T 4 >/dev/null"), 1);
  }

  #[test]
  fn eof_is_one() {
    assert_eq!(status_of(r"printf '' | thru >/dev/null"), 1);
  }

  #[test]
  fn plain_read_with_data_is_zero() {
    assert_eq!(status_of(r"printf 'abc' | thru >/dev/null"), 0);
  }

  #[test]
  fn until_found_is_zero() {
    assert_eq!(
      status_of("printf 'ab\\000cd' | thru --until $'\\0' >/dev/null"),
      0
    );
  }

  #[test]
  fn skip_then_until_seekable() {
    let dir  = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("lines.txt");
    std::fs::write(&path, "aaaa\nbbbb\ncccc\n").unwrap();
    let g = TestGuard::new();

    test_input(format!("thru -S 6 -U $'\\n' < {}", path.display())).unwrap();
    let status = Shed::get_status();

    assert_eq!(g.read_output(), "bbb");
    assert_eq!(status, 0);
  }

  #[test]
  fn skip_then_until_pipe() {
    let g = TestGuard::new();

    test_input("printf 'aaaa\\nbbbb\\ncccc\\n' | thru -S 6 -U $'\\n'").unwrap();
    let status = Shed::get_status();

    assert_eq!(g.read_output(), "bbb");
    assert_eq!(status, 0);
  }

  #[test]
  fn skip_then_from_seekable() {
    let dir  = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("lines.txt");
    std::fs::write(&path, "X\nYYbbbb\ncccc\n").unwrap();
    let g = TestGuard::new();

    test_input(format!("thru -S 2 -F $'\\n' < {}", path.display())).unwrap();
    let status = Shed::get_status();

    assert_eq!(g.read_output(), "cccc\n");
    assert_eq!(status, 0);
  }

  #[test]
  fn until_not_found_is_four() {
    assert_eq!(
      status_of("printf 'abcd' | thru --until $'\\0' >/dev/null"),
      4
    );
  }

  #[test]
  fn from_found_is_zero() {
    assert_eq!(
      status_of("printf 'ab\\000cd' | thru --from $'\\0' >/dev/null"),
      0
    );
  }

  #[test]
  fn from_not_found_is_five() {
    assert_eq!(
      status_of("printf 'abcd' | thru --from $'\\0' >/dev/null"),
      5
    );
  }

  #[test]
  fn until_outranks_short_take() {
    // Overlap: partial -T read (would be 3) with an unmet --until (4). The
    // higher code wins.
    assert_eq!(
      status_of("printf '01' | thru -T 4 --until $'\\0' >/dev/null"),
      4
    );
  }
}
