use std::{fs, io, sync::Arc};

use crate::{
  builtin::{BuiltinArgs, opt::OptSpec},
  errln, opt,
  procio::{self, OsSink, Sink},
  sherr, signal,
  state::vars::VarStr,
  util::{self, error::ShResult},
};

struct ThruOpts {
  count: bool,
  append: bool,
  report_eof: bool,
  tee: Option<VarStr>,
  take: Option<usize>,
  skip: Option<usize>,
  from: Option<u8>,
  until: Option<u8>,
}

/// Identity function that reads from stdin or files and writes to stdout, optionally teeing to a file and counting bytes.
///
/// Basically `cat` + `tee`, with no fork involved. Useful for keeping pipelines in-process if speed matters in a script.
pub(super) struct Thru;
impl super::Builtin for Thru {
  fn strict_opts(&self) -> bool {
    true
  }
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("count" | b'c'),
      opt!("append" | b'a'),
      opt!("report-eof" | b'E'),
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
    let opts = Self::parse_opts(&args)?;
    let ThruOpts {
      count,
      append,
      report_eof,
      tee,
      skip,
      mut take,
      mut from,
      mut until,
    } = opts;

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

    let mut sources: Vec<Option<VarStr>> = args
      .arguments()
      .map(|(a, _)| (a.to_str_lossy() != "-").then(|| a.clone()))
      .collect();
    if sources.is_empty() {
      // no source operands → read stdin
      sources.push(None);
    }

    let mut byte_count = 0;
    let mut skip = skip.unwrap_or(0);

    'sources: for src in sources {
      if take == Some(0) {
        break;
      }

      let reader: Arc<dyn Sink> = match &src {
        Some(path) => match fs::File::open(path) {
          Ok(f) => Arc::new(OsSink::new(f.into())),
          Err(e) => {
            errln!("thru: {path}: {e}");
            continue;
          }
        },
        None => match procio::stdin_sink() {
          Ok(s) => s,
          Err(e) => {
            errln!("thru: stdin: {e}");
            continue;
          }
        },
      };
      let path = src.unwrap_or_else(|| "stdin".into());

      let mut buf = [0u8; 16384];
      loop {
        let window = match take {
          Some(l) => skip.saturating_add(l),
          None => buf.len(),
        };
        let cap = if from.is_some() || until.is_some() {
          1
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
            io::ErrorKind::WouldBlock => break,
            io::ErrorKind::Interrupted => {
              signal::check_signals()?;
              continue;
            }
            _ => {
              errln!("thru: {path}: error reading input: {e}");
              break;
            }
          },
        };

        let chunk = &buf[..n];
        let dropped = skip.min(n);
        skip -= dropped;
        let mut emit = &chunk[dropped..];
        if skip > 0 || emit.is_empty() {
          continue;
        }

        if from.is_some() {
          if emit.first() == from.as_ref() {
            from = None;
          }
          continue;
        }
        if until.is_some() && emit.first() == until.as_ref() {
          until = None;
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
      errln!("thru: {byte_count} bytes");
    }

    let failed = (report_eof && byte_count == 0) || from.is_some() || until.is_some();

    let status = i32::from(failed);
    util::with_status(status)
  }
}

impl Thru {
  fn parse_opts(args: &BuiltinArgs) -> ShResult<ThruOpts> {
    let count = args.has_opt("count");
    let append = args.has_opt("append");
    let report_eof = args.has_opt("report-eof");
    let tee = args.opt_value("tee");
    let take = args
      .opt_value("take")
      .or_else(|| args.opt_value("limit"))
      .map(|s| {
        let span = args
          .opt_span("take")
          .or_else(|| args.opt_span("limit"))
          .unwrap();
        s.parse::<usize>()
          .ok_or_else(|| sherr!(InvalidOpt @ span, "invalid limit").with_code(2))
      })
      .transpose()?;
    let skip = args
      .opt_value("skip")
      .map(|s| {
        s.parse::<usize>().ok_or_else(|| {
          sherr!(InvalidOpt @ args.opt_span("skip").unwrap(), "invalid skip").with_code(2)
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
      report_eof,
      tee,
      take,
      skip,
      from,
      until,
    })
  }
}
