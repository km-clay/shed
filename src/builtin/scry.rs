use std::sync::Arc;

use bstr::ByteSlice;
use itertools::Itertools;

use crate::{
  errln,
  eval::{
    execute,
    lex::{LexFlags, LexStream, Span, Tk},
  },
  expand::escape,
  opt, out, outln,
  procio::{self, Sink, SinkIo},
  readline::{Candidate, CandidateStream, FuzzyBuilder, ScoredCandidate, fuzzy_match_score},
  state::source,
  try_var,
  util::{
    self,
    error::{ShResult, ShResultExt},
  },
};

use super::opt::{self, OptSpec, Parsed};

pub(super) struct Scry;
impl super::Builtin for Scry {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("read0" | b'0'),
      opt!("quote-in" | b'q'),
      opt!("quote-out" | b'Q'),
      opt!("list" | b'l'),
      opt!("sort" | b'S'),
      opt!("prompt" | b'p', 1),
      opt!("search" | b's', 1),
      OptSpec::new_short("no-newline", b'n'),
    ]
  }
  fn strict_opts(&self) -> bool {
    true
  }
  fn get_argv_and_opts(&self, cmd_span: Span, argv: &[Tk], _no_split: bool) -> ShResult<Parsed> {
    let opts = self.opts();
    let mut argv = argv.to_vec();

    // scry can be given default arguments via this SCRY_DEFAULT_OPTS variable,
    // based on the similar FZF_DEFAULT_OPTS with the fzf command. the extra
    // options are parsed as tokens from a separate source, and then spliced into
    // the arg vector at index 1, after the command name token.
    //
    // since they appear before the given args, the parser in execute() will naturally
    // overwrite any given user overrides since options are applied left-to-right

    let _src_handle = try_var!("SCRY_DEFAULT_OPTS").map(|default_opts| {
      let source =
        source::register_named_source("SCRY_DEFAULT_OPTS".as_bytes(), default_opts.as_bytes());

      let default_opts: Vec<Tk> = LexStream::new(&source, LexFlags::LEX_UNFINISHED)
        .filter_map(Result::ok)
        .collect();
      argv.splice(1..1, default_opts);
      source
    });

    let parsed = opt::parse_opts_with(&argv, &opts, self.strict_opts(), self.double_dash_operand())
      .promote_err(cmd_span)?;

    // `$_` is the last expanded word of the command line, options included; the
    // flat trace list preserves it in order.
    execute::record_last_arg(parsed.trace.last().cloned());
    Ok(parsed)
  }

  fn execute(&self, mut args: super::BuiltinArgs) -> ShResult<()> {
    let mut null_in/*----*/= false;
    let mut quote_in/*---*/= false;
    let mut quote_out/*--*/= false;
    let mut no_newline/*-*/= false;
    let mut list/*-------*/= false;
    let mut sort/*-------*/= false;
    let mut prompt/*-----*/= None;
    let mut query/*------*/= None;

    for opt in args.options() {
      match opt.key() {
        "read0"/*------*/=> null_in = true,
        "quote-in"/*---*/=> quote_in = true,
        "quote-out"/*--*/=> quote_out = true,
        "no-newline"/*-*/=> no_newline = true,
        "list"/*-------*/=> list = true,
        "sort"/*-------*/=> sort = true,
        "prompt"/*-----*/=> prompt = Some(opt.value()?.to_string()),
        "search"/*-----*/=> query = Some(opt.value()?.to_string()),
        _ => {}
      }
    }

    if !quote_in && let Some(stdin) = procio::stdin_sink().ok().filter(|s| !s.isatty()) {
      if list && !sort {
        return Self::stream_list(stdin, &mut args, query.as_deref(), null_in, quote_out);
      }
      if !list {
        return Self::stream_pick(
          stdin, &mut args, query, prompt, null_in, quote_out, no_newline,
        );
      }
    }

    let mut input = self
      .get_input(&mut args)
      .map(procio::bytes_to_string)
      .unwrap_or_default();

    if input.is_empty() && args.no_arguments() {
      match try_var!("SCRY_DEFAULT_CMD") {
        Some(cmd) if !cmd.trim().is_empty() => {
          input = procio::capture_command(cmd.as_bytes(), None, Some(&"scry".into()))?.to_string();
        }
        _ => return util::with_status(0),
      }
    }

    let mut entries = if quote_in {
      Self::split_input_quoted(&input)?
    } else if null_in {
      Self::split_input_null(&input)
    } else {
      input.lines().map(|s| (s.to_string(), 0)).collect()
    };

    for (arg, _) in args.arguments() {
      entries.push((arg.to_string(), 0));
    }

    if entries.is_empty() {
      errln!("scry: received no items to list");
      return util::with_status(2);
    }

    if list && !sort {
      return Self::print_list_unsorted(&entries, query.as_deref(), quote_out);
    }

    let mut selector = FuzzyBuilder::new().with_entries(entries).with_inline(false);

    if let Some(prompt) = prompt {
      selector = selector.with_placeholder(prompt);
    }
    if let Some(query) = query {
      selector = selector.with_query(query);
    }

    if list {
      return Self::print_candidates(selector, no_newline, quote_out);
    }

    Self::finish_pick(selector.pick()?, quote_out, no_newline)
  }
}

impl Scry {
  fn split_input_quoted(input: &str) -> ShResult<Vec<(String, i32)>> {
    Ok(
      super::quote::unquote_records(input.as_bytes())?
        .into_iter()
        // scry's fuzzy picker is String-based, so the (byte-native) records are
        // lossy-decoded here at the UI boundary.
        .map(|r| (String::from_utf8_lossy(&r.join(&b' ')).into_owned(), 0))
        .collect(),
    )
  }

  fn spawn_stdin_stream(stdin: Arc<dyn Sink>, null_in: bool) -> ShResult<CandidateStream> {
    CandidateStream::spawn(move |sink| {
      use std::io::{BufRead, BufReader};

      let delim = if null_in { b'\0' } else { b'\n' };
      let mut reader = BufReader::new(SinkIo(stdin));
      let mut buf = Vec::new();
      loop {
        buf.clear();
        match reader.read_until(delim, &mut buf) {
          Ok(0) => break,
          Ok(_) => {
            if buf.last() == Some(&delim) {
              buf.pop();
            }
            let item = String::from_utf8_lossy(&buf).into_owned();
            if !sink.send(vec![Candidate::from(item.as_str())]) {
              break;
            }
          }
          Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
            if crate::signal::sigint_pending() {
              break;
            }
          }
          Err(_) => break,
        }
      }
    })
  }

  fn finish_pick(picked: Option<String>, quote_out: bool, no_newline: bool) -> ShResult<()> {
    match picked {
      Some(item) => {
        if quote_out && let Ok(out) = procio::stdout_sink() {
          escape::shell_quote_fmt(&item, &mut SinkIo(out)).ok();
        } else if no_newline {
          out!("{item}");
        } else {
          outln!("{item}");
        }
        util::with_status(0)
      }
      None => util::with_status(1),
    }
  }

  fn emit_item(item: &str, quote_out: bool) {
    if quote_out {
      if let Ok(out) = procio::stdout_sink() {
        escape::shell_quote_fmt(item, &mut SinkIo(out)).ok();
      }
      outln!();
    } else {
      outln!("{item}");
    }
  }

  fn matches(item: &str, query: &[char]) -> bool {
    fuzzy_match_score(item, query, false) > i32::MIN
  }

  fn print_list_unsorted(
    entries: &[(String, i32)],
    query: Option<&str>,
    quote_out: bool,
  ) -> ShResult<()> {
    let query: Vec<char> = query.unwrap_or_default().chars().collect();
    for (item, _) in entries {
      if Self::matches(item, &query) {
        Self::emit_item(item, quote_out);
      }
    }
    util::with_status(0)
  }

  fn stream_list(
    stdin: Arc<dyn Sink>,
    args: &mut super::BuiltinArgs,
    query: Option<&str>,
    null_in: bool,
    quote_out: bool,
  ) -> ShResult<()> {
    let query: Vec<char> = query.unwrap_or_default().chars().collect();

    for (arg, _) in args.arguments() {
      let arg = arg.to_string();
      if Self::matches(&arg, &query) {
        Self::emit_item(&arg, quote_out);
      }
    }

    let stream = Self::spawn_stdin_stream(stdin, null_in)?;
    while let Some(batch) = stream.recv() {
      for cand in batch {
        if Self::matches(cand.as_str(), &query) {
          Self::emit_item(cand.as_str(), quote_out);
        }
      }
    }
    util::with_status(0)
  }

  fn stream_pick(
    stdin: Arc<dyn Sink>,
    args: &mut super::BuiltinArgs,
    query: Option<String>,
    prompt: Option<String>,
    null_in: bool,
    quote_out: bool,
    no_newline: bool,
  ) -> ShResult<()> {
    let entries: Vec<(String, i32)> = args.arguments().map(|(a, _)| (a.to_string(), 0)).collect();
    let stream = Self::spawn_stdin_stream(stdin, null_in)?;

    let mut selector = FuzzyBuilder::new()
      .with_entries(entries)
      .with_inline(false)
      .with_stream(stream);
    if let Some(prompt) = prompt {
      selector = selector.with_placeholder(prompt);
    }
    if let Some(query) = query {
      selector = selector.with_query(query);
    }

    Self::finish_pick(selector.pick()?, quote_out, no_newline)
  }

  fn print_candidates(builder: FuzzyBuilder, no_newline: bool, quote_out: bool) -> ShResult<()> {
    let selector = builder.build();
    let candidates = selector.filtered();

    if quote_out {
      let Some(out) = procio::stdout_sink().ok() else {
        return util::with_status(1);
      };
      let mut first = true;

      for cand in candidates {
        if !first {
          outln!();
        }
        let content = cand.content();
        escape::shell_quote_fmt(content, &mut SinkIo(out.clone())).ok();
        first = false;
      }
      if !no_newline {
        outln!();
      }
    } else {
      let output = candidates.iter().map(ScoredCandidate::content).join("\n");

      if no_newline {
        out!("{output}");
      } else {
        outln!("{output}");
      }
    }

    util::with_status(0)
  }

  fn split_input_null(input: &str) -> Vec<(String, i32)> {
    input.split('\0').map(|s| (s.to_string(), 0)).collect()
  }
}
