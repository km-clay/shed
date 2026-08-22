use crate::{
  ShResult, Shed,
  builtin::opt::OptSpec,
  expand, match_loop, opt,
  procio::{out_bytes, outln_bytes},
  state::vars::{VarFlags, VarKind, VarStr},
  util::with_status,
  varstr,
};

/// Join byte slices with `sep` (no trailing separator).
fn join_bytes(parts: &[Vec<u8>], sep: &[u8]) -> Vec<u8> {
  let mut out = Vec::new();
  for (i, part) in parts.iter().enumerate() {
    if i > 0 {
      out.extend_from_slice(sep);
    }
    out.extend_from_slice(part);
  }
  out
}

pub(super) struct Quote;
impl super::Builtin for Quote {
  fn opts(&self) -> Vec<OptSpec> {
    vec![opt!("var" | b'v', 1)]
  }
  fn execute(&self, mut args: super::BuiltinArgs) -> ShResult<()> {
    if let Some(stdin) = self.get_input(&mut args) {
      outln_bytes(&expand::shell_quote_bytes(&stdin));
      return with_status(0);
    }

    let mut parts: Vec<Vec<u8>> = args
      .arguments()
      .map(|(s, _)| expand::shell_quote_bytes(s.as_bytes()))
      .collect();

    for opt in args.options() {
      if opt.key() == "var" {
        let var = opt.value()?;
        if let Some(quoted) = quote_var(var) {
          parts.push(quoted);
        }
      }
    }

    outln_bytes(&join_bytes(&parts, b" "));
    with_status(0)
  }
}

fn quote_var(name: &str) -> Option<Vec<u8>> {
  let var = Shed::vars(|v| v.try_get_var_meta(name))?;
  match var.kind() {
    VarKind::Str(var_str) => Some(expand::shell_quote_bytes(var_str.as_bytes())),
    VarKind::Int(int) => {
      let mut buf = itoa::Buffer::new();
      Some(expand::shell_quote_bytes(buf.format(*int).as_bytes()))
    }
    VarKind::Arr(var_strs) => Some(join_bytes(
      &var_strs
        .iter()
        .map(|v| expand::shell_quote_bytes(v.as_bytes()))
        .collect::<Vec<_>>(),
      b" ",
    )),
    VarKind::AssocArr(items) => Some(join_bytes(
      &items
        .iter()
        .map(|(k, v)| {
          let mut entry = expand::shell_quote_bytes(k.as_bytes());
          entry.push(b' ');
          entry.extend_from_slice(&expand::shell_quote_bytes(v.as_bytes()));
          entry
        })
        .collect::<Vec<_>>(),
      b"\n",
    )),
    VarKind::Magic(magic_var) => {
      let resolved = magic_var()?;
      Some(expand::shell_quote_bytes(resolved.as_bytes()))
    }
    VarKind::Unset => None,
  }
}

enum UnquoteTarget {
  Array(VarStr),
  Var(VarStr),
}

pub(super) struct Unquote;
impl super::Builtin for Unquote {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::new_short("null", b'0'),
      opt!("array" | b'a', 1),
      opt!("var" | b'v', 1),
      opt!("sep" | b's', 1),
    ]
  }
  fn execute(&self, mut args: super::BuiltinArgs) -> ShResult<()> {
    log::debug!("entered unquote execute()");
    let (arg_vec, opts) = args.take_argv();

    let input: VarStr = if arg_vec.is_empty() {
      self.get_input(&mut args)
    } else {
      None
    }
    .map_or_else(|| super::join_raw_args(arg_vec).0, VarStr::from);

    let mut target = None;
    let mut delim = "\n".into();

    for opt in opts {
      match opt.key() {
        "array" => {
          target = Some(UnquoteTarget::Array(opt.value()?.into()));
        }
        "var" => {
          target = Some(UnquoteTarget::Var(opt.value()?.into()));
        }
        "sep" => {
          let val = opt.value()?;
          delim = varstr!("{val}");
        }
        _ => {}
      }
    }

    let mut fields = unquote_raw(input.as_bytes())?.into_iter();

    match target {
      None => {
        if let Some(first) = fields.next() {
          out_bytes(&first);
          for field in fields {
            out_bytes(delim.as_bytes());
            out_bytes(&field);
          }
          if delim == "\n" {
            out_bytes(b"\n");
          }
        }
      }
      Some(UnquoteTarget::Array(name)) => {
        let var = VarKind::arr(fields.map(VarStr::from));
        Shed::vars_mut(|v| v.set_var(&name.to_str_lossy(), var, VarFlags::empty()))?;
      }
      Some(UnquoteTarget::Var(name)) => {
        let collected: Vec<Vec<u8>> = fields.collect();
        let var = VarKind::string(VarStr::from(join_bytes(&collected, b" ")));
        Shed::vars_mut(|v| v.set_var(&name.to_str_lossy(), var, VarFlags::empty()))?;
      }
    }

    with_status(0)
  }
}

pub(crate) fn unquote_raw(s: &[u8]) -> ShResult<Vec<Vec<u8>>> {
  let mut fields: Vec<Vec<u8>> = vec![];
  let mut field: Vec<u8> = Vec::new();
  let mut bytes = s.iter().copied().peekable();
  let mut started = false;

  match_loop!(bytes.next() => ch, {
    b'\\' => {
      started = true;
      if let Some(next) = bytes.next() {
        field.push(next);
      }
    }
    b'\'' => {
      started = true;
      match_loop!(bytes.next() => ch, {
        b'\'' => break,
        _ => field.push(ch),
      });
    }
    b'$' if bytes.peek() == Some(&b'\'') => {
      started = true;
      bytes.next();
      let mut raw: Vec<u8> = Vec::new();
      match_loop!(bytes.next() => ch, {
        b'\'' => break,
        b'\\' => {
          raw.push(b'\\');
          if let Some(next) = bytes.next() {
            raw.push(next);
          }
        }
        _ => raw.push(ch),
      });

      // Byte-native: `$'...'` expands straight into the field, preserving any
      // raw bytes (e.g. `\377`) instead of laundering through `from_utf8_lossy`.
      field.extend_from_slice(&expand::expand_ansi_c(&raw));
    }
    _ if ch.is_ascii_whitespace() => {
      if started {
        fields.push(std::mem::take(&mut field));
        started = false;
      }
    }
    _ => {
      started = true;
      field.push(ch);
    }
  });

  if started {
    fields.push(field);
  }

  Ok(fields)
}

pub(crate) fn unquote_records(input: &[u8]) -> ShResult<Vec<Vec<Vec<u8>>>> {
  input
    .split(|&b| b == b'\n')
    .filter(|l| !l.trim_ascii().is_empty())
    .map(unquote_raw)
    .collect()
}
