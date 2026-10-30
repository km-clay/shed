use std::{cmp::Ordering, collections::VecDeque, fs, os::unix::fs::MetadataExt, path::PathBuf};

use bstr::ByteSlice;
use nix::{
  sys::stat::{self, SFlag},
  unistd::{self, AccessFlags},
};

use crate::{
  eval::lex::TkRule,
  expand::{
    Expander, Word, case, escape, glob,
    stream::{Marker, Quote, Unit},
    var,
  },
  match_loop, set_var, sherr,
  state::{
    Shed,
    vars::{ShStr, VarKind},
  },
  util::{
    error::{ShErr, ShResult},
    strops::{FromBytes, ParseBytes, QuoteState},
  },
};

use super::{
  NdRule, ParseStream,
  ast::{Ast, ExprId, NodeId, TkId},
  lex::{Span, Tk},
};

#[derive(Clone, Copy, Debug)]
pub(crate) enum TestExpr {
  Or { lhs: ExprId, rhs: ExprId },
  And { lhs: ExprId, rhs: ExprId },
  Not { expr: ExprId },
  Unary { op: UnaryOp, operand: TkId },
  Binary { lhs: TkId, op: BinaryOp, rhs: TkId },
  Truthy { operand: TkId },
}

#[derive(Debug, Copy, Clone)]
pub(crate) enum UnaryOp {
  Exists,                    // -e | -a
  Directory,                 // -d
  File,                      // -f
  Symlink,                   // -h or -L
  Readable,                  // -r
  Writable,                  // -w
  Executable,                // -x
  NonEmpty,                  // -s
  NamedPipe,                 // -p
  Socket,                    // -S
  BlockSpecial,              // -b
  CharSpecial,               // -c
  Sticky,                    // -k
  UIDOwner,                  // -O
  GIDOwner,                  // -G
  ModifiedSinceStatusChange, // -N
  SetUID,                    // -u
  SetGID,                    // -g
  Terminal,                  // -t
  NonNull,                   // -n
  Null,                      // -z
  OptSet,                    // -o

  // shed extensions
  IsInteger, // -i
  IsFloat,   // -F
}

impl FromBytes for UnaryOp {
  type Error = ShErr;
  fn from_bytes<B: AsRef<[u8]>>(bytes: B) -> Result<Self, Self::Error>
  where
    Self: Sized,
  {
    let s = bytes.as_ref();
    match s {
      b"-e" | b"-a" => Ok(Self::Exists),
      b"-d"         => Ok(Self::Directory),
      b"-f"         => Ok(Self::File),
      b"-h" | b"-L" => Ok(Self::Symlink),
      b"-r"         => Ok(Self::Readable),
      b"-w"         => Ok(Self::Writable),
      b"-x"         => Ok(Self::Executable),
      b"-s"         => Ok(Self::NonEmpty),
      b"-p"         => Ok(Self::NamedPipe),
      b"-S"         => Ok(Self::Socket),
      b"-b"         => Ok(Self::BlockSpecial),
      b"-c"         => Ok(Self::CharSpecial),
      b"-k"         => Ok(Self::Sticky),
      b"-o"         => Ok(Self::OptSet),
      b"-O"         => Ok(Self::UIDOwner),
      b"-G"         => Ok(Self::GIDOwner),
      b"-N"         => Ok(Self::ModifiedSinceStatusChange),
      b"-u"         => Ok(Self::SetUID),
      b"-g"         => Ok(Self::SetGID),
      b"-t"         => Ok(Self::Terminal),
      b"-n"         => Ok(Self::NonNull),
      b"-z"         => Ok(Self::Null),
      b"-i"         => Ok(Self::IsInteger),
      b"-F"         => Ok(Self::IsFloat),
      _             => Err(sherr!(SyntaxErr, "Invalid unary test operator '{}'", s.to_str_lossy())),
    }
  }
}

#[derive(Debug, Copy, Clone)]
pub(crate) enum FileOp {
  OlderThan, // -ot
  NewerThan, // -nt
  FileEq,    // -ef
}

#[derive(Debug, Copy, Clone)]
pub(crate) enum IntOp {
  Eq,  // -eq
  Neq, // -ne
  Gt,  // -gt
  Lt,  // -lt
  Ge,  // -ge
  Le,  // -le
}

#[derive(Debug, Copy, Clone)]
pub(crate) enum StringOp {
  Equal,     // =/==
  NotEqual,  // !=
  Match,     // =~
  Less,      // <
  Greater,   // >
  LessEq,    // <=
  GreaterEq, // >=
}

#[derive(Debug, Copy, Clone)]
pub(crate) enum BinaryOp {
  String(StringOp),
  Int(IntOp),
  File(FileOp),
}

impl FromBytes for BinaryOp {
  type Error = ShErr;
  fn from_bytes<B: AsRef<[u8]>>(bytes: B) -> Result<Self, Self::Error>
  where
    Self: Sized,
  {
    let s = bytes.as_ref();
    match s {
      b"==" | b"=" => Ok(Self::String(StringOp::Equal)),
      b"!="        => Ok(Self::String(StringOp::NotEqual)),
      b"=~"        => Ok(Self::String(StringOp::Match)),
      b">"         => Ok(Self::String(StringOp::Greater)),
      b"<"         => Ok(Self::String(StringOp::Less)),
      b">="        => Ok(Self::String(StringOp::GreaterEq)),
      b"<="        => Ok(Self::String(StringOp::LessEq)),
      b"-eq"       => Ok(Self::Int(IntOp::Eq)),
      b"-ne"       => Ok(Self::Int(IntOp::Neq)),
      b"-gt"       => Ok(Self::Int(IntOp::Gt)),
      b"-lt"       => Ok(Self::Int(IntOp::Lt)),
      b"-ge"       => Ok(Self::Int(IntOp::Ge)),
      b"-le"       => Ok(Self::Int(IntOp::Le)),
      b"-ot"       => Ok(Self::File(FileOp::OlderThan)),
      b"-nt"       => Ok(Self::File(FileOp::NewerThan)),
      b"-ef"       => Ok(Self::File(FileOp::FileEq)),
      _            => Err(sherr!(SyntaxErr, "Invalid binary test operator '{}'", s.to_str_lossy())),
    }
  }
}

pub(crate) fn eval_test_expr(ast: &Ast, expr: ExprId) -> ShResult<bool> {
  let res = match ast[expr] {
    TestExpr::Or { lhs, rhs }  => eval_test_expr(ast, lhs)? || eval_test_expr(ast, rhs)?,
    TestExpr::And { lhs, rhs } => eval_test_expr(ast, lhs)? && eval_test_expr(ast, rhs)?,
    TestExpr::Not { expr }     => !eval_test_expr(ast, expr)?,

    TestExpr::Unary { op, operand } => {
      let val = expand_operand(&ast[operand])?;
      eval_unary(&op, val)
    }
    TestExpr::Truthy { operand } => {
      let val = expand_operand(&ast[operand])?;
      !val.is_empty()
    }
    TestExpr::Binary { lhs, op, rhs } => {
      let lhs = expand_operand(&ast[lhs])?;
      let rhs = expand_rhs(&ast[rhs], op)?;
      eval_binary(&op, &lhs, &rhs, true)?
    }
  };

  Ok(res)
}

fn expand_operand(tk: &Tk) -> ShResult<Word> {
  let val = Expander::new(tk).no_glob().no_split().expand_no_split()?;

  Ok(Word::new(val).with_span(tk.span))
}

fn expand_rhs(tk: &Tk, op: BinaryOp) -> ShResult<Word> {
  let val = match op {
    BinaryOp::String(StringOp::Equal | StringOp::NotEqual) => {
      case::expand_case_pattern(Some(tk.span), tk.slice().as_bytes())?
    }
    BinaryOp::String(StringOp::Match) => {
      expand_regex_operand(Some(tk.span), tk.slice().as_bytes())?
    }
    _ => Expander::new(tk).no_glob().no_split().expand_no_split()?,
  };

  Ok(Word::new(val).with_span(tk.span))
}

fn expand_regex_operand(span: Option<Span>, bytes: &[u8]) -> ShResult<ShStr> {
  const RE_META: &[u8] = br".*+?()[]{}^$|\";

  let     unescaped = escape::unescape_str(bytes);
  let     expanded  = var::expand_raw(span, &mut unescaped.cursor())?;
  let mut result    = vec![];
  let mut qt_state  = QuoteState::default();
  let mut cursor    = expanded.cursor();

  match_loop!(cursor.next() => unit, {
    Unit::Mark(Marker::Quote(q)) => match q {
      Quote::Single => {
        qt_state.toggle_double();
      },
      Quote::Double => {
        qt_state.toggle_single();
      },
    }
    Unit::Byte(b) => {
      if qt_state.in_quote() && RE_META.contains(&b) {
        result.push(b'\\')
      }
      result.push(b);
    }
    Unit::Mark(_) => {}
  });

  Ok(ShStr::from(result))
}

/// Evaluate a single unary test (`-OP OPERAND`).
pub(crate) fn eval_unary(op: &UnaryOp, operand: Word) -> bool {
  let operand = operand.content();

  match op {
    UnaryOp::NonNull    => !operand.is_empty(),
    UnaryOp::Null       => operand.is_empty(),
    UnaryOp::IsInteger  => operand.parse_bytes::<i64>().is_ok(),
    UnaryOp::Exists     => PathBuf::from(operand).exists(),
    UnaryOp::Directory  => PathBuf::from(operand).metadata().is_ok_and(|m| m.is_dir()),
    UnaryOp::File       => PathBuf::from(operand).metadata().is_ok_and(|m| m.is_file()),
    UnaryOp::Symlink    => fs::symlink_metadata(operand).is_ok_and(|m| m.file_type().is_symlink()),
    UnaryOp::Readable   => unistd::access(&operand, AccessFlags::R_OK).is_ok(),
    UnaryOp::Writable   => unistd::access(&operand, AccessFlags::W_OK).is_ok(),
    UnaryOp::Executable => unistd::access(&operand, AccessFlags::X_OK).is_ok(),
    UnaryOp::NonEmpty   => fs::metadata(operand).is_ok_and(|m| m.len() > 0),
    UnaryOp::Sticky     => stat::stat(&operand).is_ok_and(|s| s.st_mode & nix::libc::S_ISVTX != 0),
    UnaryOp::UIDOwner => stat::stat(&operand).is_ok_and(|s| s.st_uid == unistd::geteuid().as_raw()),
    UnaryOp::GIDOwner => stat::stat(&operand).is_ok_and(|s| s.st_gid == unistd::getegid().as_raw()),
    UnaryOp::SetUID   => stat::stat(&operand).is_ok_and(|s| s.st_mode & nix::libc::S_ISUID != 0),
    UnaryOp::SetGID   => stat::stat(&operand).is_ok_and(|s| s.st_mode & nix::libc::S_ISGID != 0),
    UnaryOp::NamedPipe => stat::stat(&operand)
      .is_ok_and(|s| SFlag::from_bits_truncate(s.st_mode).contains(SFlag::S_IFIFO)),
    UnaryOp::Socket => stat::stat(&operand)
      .is_ok_and(|s| SFlag::from_bits_truncate(s.st_mode).contains(SFlag::S_IFSOCK)),
    UnaryOp::BlockSpecial => stat::stat(&operand)
      .is_ok_and(|s| SFlag::from_bits_truncate(s.st_mode).contains(SFlag::S_IFBLK)),
    UnaryOp::CharSpecial => stat::stat(&operand)
      .is_ok_and(|s| SFlag::from_bits_truncate(s.st_mode).contains(SFlag::S_IFCHR)),
    UnaryOp::ModifiedSinceStatusChange => {
      stat::stat(&operand).is_ok_and(|s| s.st_mtime > s.st_ctime)
    }
    UnaryOp::OptSet => Shed::shopts(|o| {
      o.get_raw(&format!("set.{operand}"))
        .ok()
        .flatten()
        .or_else(|| o.get_raw(&operand.to_str_lossy()).ok().flatten())
        .and_then(|v| v.parse::<bool>().ok())
        .unwrap_or(false)
    }),
    UnaryOp::Terminal => match operand.parse_bytes::<i32>() {
      Ok(fd) => Shed::sinks(|s| s.get(fd).is_some_and(|s| s.isatty())),
      Err(_) => false,
    },
    UnaryOp::IsFloat => operand
      .parse::<f64>()
      .ok()
      .as_ref()
      .is_some_and(|n| n.is_finite()),
  }
}

/// Evaluate a single binary test (`LHS OP RHS`).
pub(crate) fn eval_binary(op: &BinaryOp, lhs: &Word, rhs: &Word, extended: bool) -> ShResult<bool> {
  match op {
    BinaryOp::String(str_op) => match str_op {
      StringOp::Less | StringOp::Greater | StringOp::LessEq | StringOp::GreaterEq => {
        let eq_check = matches!(str_op, StringOp::LessEq | StringOp::GreaterEq);
        let ge_check = matches!(str_op, StringOp::Greater | StringOp::GreaterEq);
        let ordering = lhs.cmp(rhs);

        match ordering {
          Ordering::Less    => Ok(!ge_check),
          Ordering::Equal   => Ok(eq_check),
          Ordering::Greater => Ok(ge_check),
        }
      }
      StringOp::Equal | StringOp::NotEqual => {
        let polarity = matches!(str_op, StringOp::Equal);
        let is_eq = if extended {
          let pat = Shed::meta_mut(|m| m.get_glob(rhs.as_bytes()));
          pat.is_match(lhs.as_bytes())
        } else {
          lhs == rhs
        };

        Ok(is_eq == polarity)
      }
      StringOp::Match => {
        let cleaned = glob::replace_posix_classes(&rhs.to_str_lossy());
        let re = Shed::meta_mut(|m| m.get_regex(&cleaned))
          .map_err(|e| sherr!(SyntaxErr ? rhs.span(), "Invalid regex: {e}"))?;

        if let Some(caps) = re.captures(&lhs.to_str_lossy()) {
          let groups: VecDeque<ShStr> = caps
            .iter()
            .map(|m| m.map(|mat| ShStr::from(mat.as_str())).unwrap_or_default())
            .collect();
          set_var!(b"SHED_REMATCH", VarKind::arr(groups); LOCAL)?;
          Ok(true)
        } else {
          Shed::vars_mut(|v| v.unset_var(b"SHED_REMATCH")).ok();
          Ok(false)
        }
      }
    },
    BinaryOp::Int(int_op) => {
      let lhs_i = lhs
        .to_str_lossy()
        .trim()
        .parse::<i64>()
        .map_err(|_| sherr!(SyntaxErr ? lhs.span(), "test: integer expected, got '{lhs}'"))?;
      let rhs_i = rhs
        .to_str_lossy()
        .trim()
        .parse::<i64>()
        .map_err(|_| sherr!(SyntaxErr ? rhs.span(), "test: integer expected, got '{rhs}'"))?;

      Ok(match int_op {
        IntOp::Eq  => lhs_i == rhs_i,
        IntOp::Neq => lhs_i != rhs_i,
        IntOp::Gt  => lhs_i > rhs_i,
        IntOp::Lt  => lhs_i < rhs_i,
        IntOp::Ge  => lhs_i >= rhs_i,
        IntOp::Le  => lhs_i <= rhs_i,
      })
    }
    BinaryOp::File(file_op) => {
      let mtime = |v: &ShStr| {
        PathBuf::from(v.clone())
          .metadata()
          .and_then(|m| m.modified())
          .ok()
      };

      Ok(match file_op {
        FileOp::NewerThan => mtime(lhs) > mtime(rhs),
        FileOp::OlderThan => mtime(lhs) < mtime(rhs),
        FileOp::FileEq => {
          let left_file  = PathBuf::from(lhs.content());
          let left_data  = left_file.metadata();
          let right_file = PathBuf::from(rhs.content());
          let right_data = right_file.metadata();

          match (left_data, right_data) {
            (Ok(l), Ok(r)) => l.ino() == r.ino() && l.dev() == r.dev(),
            _              => false,
          }
        }
      })
    }
  }
}

impl ParseStream {
  pub(super) fn parse_dbracket(&mut self) -> ShResult<Option<NodeId>> {
    if !self.check_keyword(b"[[") {
      return Ok(None);
    }

    let mut span: Option<Span> = None;
    let mut redirs             = vec![];

    self.expect_tk_with(&mut span);

    let expr = self.parse_test_or(&mut span)?;

    if !self.check_keyword(b"]]") {
      bail!(self, span, "Expected ']]' to close this test expression");
    }
    self.expect_tk_with(&mut span);

    self.parse_redir(&mut redirs, &mut span)?;

    let span   = self.tree.alloc(span.unwrap_or_default());
    let redirs = (!redirs.is_empty()).then(|| self.tree.alloc_redirs(redirs));
    let node   = node!(self, span, NdRule::TestNode { expr }, redirs);

    Ok(Some(self.tree.alloc(node)))
  }

  fn parse_test_or(&mut self, span: &mut Option<Span>) -> ShResult<ExprId> {
    let mut lhs = self.parse_test_and(span)?;
    while *self.next_tk_class() == TkRule::Or {
      self.expect_tk_with(span);
      self.catch_linebreak(span);

      let rhs = self.parse_test_and(span)?;
      lhs = self.tree.alloc(TestExpr::Or { lhs, rhs });
    }
    Ok(lhs)
  }

  fn parse_test_and(&mut self, span: &mut Option<Span>) -> ShResult<ExprId> {
    let mut lhs = self.parse_test_expr(span)?;
    while *self.next_tk_class() == TkRule::And {
      self.expect_tk_with(span);
      self.catch_linebreak(span);

      let rhs = self.parse_test_expr(span)?;
      lhs = self.tree.alloc(TestExpr::And { lhs, rhs });
    }
    Ok(lhs)
  }

  fn parse_test_expr(&mut self, span: &mut Option<Span>) -> ShResult<ExprId> {
    stacker::maybe_grow(64 * 1024, 1024 * 1024, || {
      let Some(tk) = self.peek_tk().cloned() else {
        bail!(self, *span, "Unexpected end of input in test expression");
      };

      match tk.slice().as_bytes() {
        b"!" => {
          self.expect_tk_with(span);
          let expr = self.parse_test_expr(span)?;
          Ok(self.tree.alloc(TestExpr::Not { expr }))
        }
        b"(" => {
          self.expect_tk_with(span);
          let expr = self.parse_test_or(span)?;
          if !self.peek_is(b")") {
            bail!(self, *span, "Expected ')' to close this test expression");
          }
          self.expect_tk_with(span);
          Ok(expr)
        }
        b")" => {
          bail!(self, *span, "Empty parentheses in test expression");
        }
        b"]]" => {
          bail!(self, *span, "Unexpected ']]' in test expression");
        }
        _ => self.parse_test_leaf(span),
      }
    })
  }

  fn peek_is_operand(&self) -> bool {
    self.peek_tk().is_some_and(|tk| {
      tk.class == TkRule::Str
        && !matches!(tk.slice().as_bytes(), b"]]" | b"(" | b")")
        && tk.slice().parse_bytes::<BinaryOp>().is_err()
    })
  }

  fn peek_is_rhs_operand(&self) -> bool {
    self.peek_tk().is_some_and(|tk| {
      tk.class == TkRule::Str && !matches!(tk.slice().as_bytes(), b"]]" | b"(" | b")")
    })
  }

  fn parse_test_leaf(&mut self, span: &mut Option<Span>) -> ShResult<ExprId> {
    let l_tk = self.expect_tk_with(span);

    if let Ok(op) = l_tk.slice().parse_bytes::<UnaryOp>()
      && self.peek_is_operand()
    {
      let operand = self.expect_tk_with(span);
      let operand = self.tree.alloc(operand);
      return Ok(self.tree.alloc(TestExpr::Unary { op, operand }));
    }

    let lhs = self.tree.alloc(l_tk);

    let maybe_operator = self
      .peek_tk()
      .and_then(|t| t.slice().parse_bytes::<BinaryOp>().ok());

    let Some(op) = maybe_operator else {
      return Ok(self.tree.alloc(TestExpr::Truthy { operand: lhs }));
    };

    let op_tk = self.expect_tk_with(span);

    if !self.peek_is_rhs_operand() {
      bail!(
        self,
        *span,
        "Expected right-hand side operand after operator '{}'",
        op_tk.slice()
      );
    }

    let r_tk = self.expect_tk_with(span);

    let rhs  = self.tree.alloc(r_tk);

    Ok(self.tree.alloc(TestExpr::Binary { lhs, op, rhs }))
  }
}
