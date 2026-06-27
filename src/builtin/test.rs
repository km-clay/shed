use std::{collections::VecDeque, fs::metadata, os::fd::BorrowedFd, path::PathBuf, str::FromStr};

use crate::{
  eval::lex::Tk,
  procio,
  util::{ShResultExt, replace_posix_classes},
};

use super::{
  Shed,
  eval::{
    execute::prepare_argv_with,
    lex::{Span, TkVecUtils},
  },
  expand, sherr,
  state::{vars::VarFlags, vars::VarKind},
  util::{ShErr, ShResult, with_status},
};
use nix::{
  sys::stat::{self, SFlag},
  unistd::{AccessFlags, isatty},
};

#[derive(Debug, Clone)]
pub(crate) enum UnaryOp {
  Exists,                    // -e
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

  // shed extensions
  IsInteger, // -i
  IsFloat,   // -F
}

impl FromStr for UnaryOp {
  type Err = ShErr;
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "-e" => Ok(Self::Exists),
      "-d" => Ok(Self::Directory),
      "-f" => Ok(Self::File),
      "-h" | "-L" => Ok(Self::Symlink),
      "-r" => Ok(Self::Readable),
      "-w" => Ok(Self::Writable),
      "-x" => Ok(Self::Executable),
      "-s" => Ok(Self::NonEmpty),
      "-p" => Ok(Self::NamedPipe),
      "-S" => Ok(Self::Socket),
      "-b" => Ok(Self::BlockSpecial),
      "-c" => Ok(Self::CharSpecial),
      "-k" => Ok(Self::Sticky),
      "-O" => Ok(Self::UIDOwner),
      "-G" => Ok(Self::GIDOwner),
      "-N" => Ok(Self::ModifiedSinceStatusChange),
      "-u" => Ok(Self::SetUID),
      "-g" => Ok(Self::SetGID),
      "-t" => Ok(Self::Terminal),
      "-n" => Ok(Self::NonNull),
      "-z" => Ok(Self::Null),
      "-i" => Ok(Self::IsInteger),
      "-F" => Ok(Self::IsFloat),
      _ => Err(sherr!(SyntaxErr, "Invalid unary test operator '{s}'")),
    }
  }
}

#[derive(Debug, Clone)]
pub(crate) enum BinaryOp {
  StringEq,   // = ==
  StringNeq,  // !=
  IntEq,      // -eq
  IntNeq,     // -ne
  IntGt,      // -gt
  IntLt,      // -lt
  IntGe,      // -ge
  IntLe,      // -le
  RegexMatch, // =~
}

impl FromStr for BinaryOp {
  type Err = ShErr;
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "==" | "=" => Ok(Self::StringEq),
      "!=" => Ok(Self::StringNeq),
      "=~" => Ok(Self::RegexMatch),
      "-eq" => Ok(Self::IntEq),
      "-ne" => Ok(Self::IntNeq),
      "-gt" => Ok(Self::IntGt),
      "-lt" => Ok(Self::IntLt),
      "-ge" => Ok(Self::IntGe),
      "-le" => Ok(Self::IntLe),
      _ => Err(sherr!(SyntaxErr, "Invalid binary test operator '{s}'")),
    }
  }
}

/// Evaluate a single unary test (`-OP OPERAND`).
fn eval_unary(op: &UnaryOp, operand: &str) -> bool {
  match op {
    UnaryOp::Exists => PathBuf::from(operand).exists(),
    UnaryOp::Directory => PathBuf::from(operand).metadata().is_ok_and(|m| m.is_dir()),
    UnaryOp::File => PathBuf::from(operand).metadata().is_ok_and(|m| m.is_file()),
    UnaryOp::Symlink => {
      std::fs::symlink_metadata(operand).is_ok_and(|m| m.file_type().is_symlink())
    }
    UnaryOp::Readable => nix::unistd::access(operand, AccessFlags::R_OK).is_ok(),
    UnaryOp::Writable => nix::unistd::access(operand, AccessFlags::W_OK).is_ok(),
    UnaryOp::Executable => nix::unistd::access(operand, AccessFlags::X_OK).is_ok(),
    UnaryOp::NonEmpty => metadata(operand).is_ok_and(|m| m.len() > 0),
    UnaryOp::NamedPipe => stat::stat(operand)
      .is_ok_and(|s| SFlag::from_bits_truncate(s.st_mode).contains(SFlag::S_IFIFO)),
    UnaryOp::Socket => stat::stat(operand)
      .is_ok_and(|s| SFlag::from_bits_truncate(s.st_mode).contains(SFlag::S_IFSOCK)),
    UnaryOp::BlockSpecial => stat::stat(operand)
      .is_ok_and(|s| SFlag::from_bits_truncate(s.st_mode).contains(SFlag::S_IFBLK)),
    UnaryOp::CharSpecial => stat::stat(operand)
      .is_ok_and(|s| SFlag::from_bits_truncate(s.st_mode).contains(SFlag::S_IFCHR)),
    UnaryOp::Sticky => stat::stat(operand).is_ok_and(|s| s.st_mode & nix::libc::S_ISVTX != 0),
    UnaryOp::UIDOwner => {
      stat::stat(operand).is_ok_and(|s| s.st_uid == nix::unistd::geteuid().as_raw())
    }
    UnaryOp::GIDOwner => {
      stat::stat(operand).is_ok_and(|s| s.st_gid == nix::unistd::getegid().as_raw())
    }
    UnaryOp::ModifiedSinceStatusChange => {
      stat::stat(operand).is_ok_and(|s| s.st_mtime > s.st_ctime)
    }
    UnaryOp::SetUID => stat::stat(operand).is_ok_and(|s| s.st_mode & nix::libc::S_ISUID != 0),
    UnaryOp::SetGID => stat::stat(operand).is_ok_and(|s| s.st_mode & nix::libc::S_ISGID != 0),
    UnaryOp::Terminal => match operand.parse::<i32>() {
      Ok(fd) => match fd {
        1 => {
          !procio::has_out_sink() && isatty(unsafe { BorrowedFd::borrow_raw(fd) }).unwrap_or(false)
        }
        0 => {
          !procio::has_in_sink() && isatty(unsafe { BorrowedFd::borrow_raw(fd) }).unwrap_or(false)
        }
        _ => isatty(unsafe { BorrowedFd::borrow_raw(fd) }).unwrap_or(false),
      },
      Err(_) => false,
    },
    UnaryOp::NonNull => !operand.is_empty(),
    UnaryOp::Null => operand.is_empty(),
    UnaryOp::IsInteger => operand.parse::<i64>().is_ok(),
    UnaryOp::IsFloat => operand
      .parse::<f64>()
      .ok()
      .filter(|n| n.is_finite())
      .is_some(),
  }
}

/// Evaluate a single binary test (`LHS OP RHS`).
fn eval_binary(op: &BinaryOp, lhs: &(String, Span), rhs: &(String, Span)) -> ShResult<bool> {
  match op {
    BinaryOp::StringEq => {
      let pattern = expand::glob_to_regex(rhs.0.trim(), true);
      Ok(pattern.is_match(lhs.0.trim()))
    }
    BinaryOp::StringNeq => {
      let pattern = expand::glob_to_regex(rhs.0.trim(), true);
      Ok(!pattern.is_match(lhs.0.trim()))
    }
    BinaryOp::IntEq
    | BinaryOp::IntNeq
    | BinaryOp::IntGt
    | BinaryOp::IntLt
    | BinaryOp::IntGe
    | BinaryOp::IntLe => {
      let lhs_i = lhs.0.trim().parse::<i64>().map_err(
        |_| sherr!(SyntaxErr @ lhs.1.clone(), "test: integer expected, got '{}'", &lhs.0),
      )?;
      let rhs_i = rhs.0.trim().parse::<i64>().map_err(
        |_| sherr!(SyntaxErr @ rhs.1.clone(), "test: integer expected, got '{}'", &rhs.0),
      )?;
      Ok(match op {
        BinaryOp::IntEq => lhs_i == rhs_i,
        BinaryOp::IntNeq => lhs_i != rhs_i,
        BinaryOp::IntGt => lhs_i > rhs_i,
        BinaryOp::IntLt => lhs_i < rhs_i,
        BinaryOp::IntGe => lhs_i >= rhs_i,
        BinaryOp::IntLe => lhs_i <= rhs_i,
        _ => unreachable!(),
      })
    }
    BinaryOp::RegexMatch => {
      let cleaned = replace_posix_classes(&rhs.0);
      let re = Shed::meta_mut(|m| m.get_regex(&cleaned))
        .map_err(|e| sherr!(SyntaxErr @ rhs.1.clone(), "Invalid regex: {e}"))?;
      if let Some(caps) = re.captures(&lhs.0) {
        let groups: VecDeque<String> = caps
          .iter()
          .map(|m| m.map(|mat| mat.as_str().to_string()).unwrap_or_default())
          .collect();
        Shed::vars_mut(|v| v.set_var("SHED_REMATCH", VarKind::arr(groups), VarFlags::LOCAL))?;
        Ok(true)
      } else {
        Shed::vars_mut(|v| v.unset_var("SHED_REMATCH")).ok();
        Ok(false)
      }
    }
  }
}

/// Recursive Descent Parser for test arguments.
/// The grammar looks like:
///
///   `parse_or`   ::= `parse_and` (('-o' | '||') `parse_and`)*
///   `parse_and`  ::= `parse_not` (('-a' | '&&') `parse_not`)*
///   `parse_not`  ::= '!' `parse_not` | `parse_primary`
///   `parse_primary` ::= '(' `parse_or` ')' | `leaf_dispatch`
///
/// Leaf dispatch is arity-based per POSIX:
///   1 arg  → implicit -n on the argument
///   2 args → unary op + operand   (e.g. `-f foo`)
///   3 args → lhs op rhs           (e.g. `a -eq b`)
struct ArgvParser<'a> {
  argv: &'a [(String, Span)],
  pos: usize,
}

const STOP_TOKENS: &[&str] = &["-a", "-o", "&&", "||", ")", "!"];

impl<'a> ArgvParser<'a> {
  fn new(argv: &'a [(String, Span)]) -> Self {
    Self { argv, pos: 0 }
  }

  fn peek(&self) -> Option<&str> {
    self.argv.get(self.pos).map(|s| s.0.as_str())
  }

  fn advance(&mut self) {
    self.pos += 1;
  }

  fn parse_or(&mut self, eval: bool) -> ShResult<bool> {
    let mut left = self.parse_and(eval)?;
    while matches!(self.peek(), Some("-o" | "||")) {
      self.advance();
      let right = self.parse_and(eval && !left)?;
      left = left || right;
    }
    Ok(left)
  }

  fn parse_and(&mut self, eval: bool) -> ShResult<bool> {
    let mut left = self.parse_not(eval)?;
    while matches!(self.peek(), Some("-a" | "&&")) {
      self.advance();
      let right = self.parse_not(eval && left)?;
      left = left && right;
    }
    Ok(left)
  }

  fn parse_not(&mut self, eval: bool) -> ShResult<bool> {
    if self.peek() == Some("!") {
      self.advance();
      Ok(!self.parse_not(eval)?)
    } else {
      self.parse_primary(eval)
    }
  }

  fn parse_primary(&mut self, eval: bool) -> ShResult<bool> {
    if self.peek() == Some("(") {
      self.advance();
      let inner = self.parse_or(eval)?;
      if self.peek() != Some(")") {
        return Err(sherr!(SyntaxErr, "test: expected ')' to close group"));
      }
      self.advance();
      return Ok(inner);
    }

    let start = self.pos;
    while let Some(tok) = self.peek() {
      if STOP_TOKENS.contains(&tok) {
        break;
      }
      self.advance();
    }
    let leaf = &self.argv[start..self.pos];
    if eval { eval_leaf(leaf) } else { Ok(false) }
  }
}

/// POSIX arity dispatch on a leaf (no `!`, `(`, `)`, or conjuncts).
fn eval_leaf(leaf: &[(String, Span)]) -> ShResult<bool> {
  if leaf.is_empty() {
    return Ok(false);
  }
  let start_span = leaf.first().unwrap().1.clone();
  let end_span = leaf.last().unwrap().1.clone();
  let major_span = start_span.merge_with(&end_span).unwrap_or(end_span);

  match leaf.len() {
    1 => {
      // Arity-1: implicit `-n`, true if the lone argument is non-empty.
      Ok(!leaf[0].0.is_empty())
    }
    2 => {
      // Arity-2: `-OP OPERAND`.
      let op: UnaryOp = leaf[0].0.parse()?;
      Ok(eval_unary(&op, &leaf[1].0))
    }
    3 => {
      // Arity-3: `LHS OP RHS`.
      let op: BinaryOp = leaf[1].0.parse()?;
      eval_binary(&op, &leaf[0], &leaf[2])
    }
    _ => Err(sherr!(
      SyntaxErr @ major_span,
      "test: too many arguments for a single expression ({})",
      leaf.len()
    )),
  }
}

pub(super) struct Test;
impl super::Builtin for Test {
  /// Custom override so we can pair the opener (`[`/`[[`/`test`) against its
  /// required closer (`]`/`]]`/none) while argv[0] is still present, then
  /// hand the operands-only argv to `execute`.
  fn get_argv_and_opts(
    &self,
    cmd_span: Span,
    argv: &[Tk],
    no_split: bool,
  ) -> ShResult<(super::ArgVector, Vec<super::Opt>)> {
    let span = argv.get_span().unwrap();
    let mut argv = prepare_argv_with(argv, no_split).promote_err(cmd_span)?;
    let opener = argv
      .first()
      .map(|(s, _)| s.as_str())
      .unwrap_or_default()
      .to_string();
    let want_close: Option<&str> = match opener.as_str() {
      "[" => Some("]"),
      "[[" => Some("]]"),
      _ => None,
    };
    if let Some(close) = want_close {
      match argv.last() {
        Some((last, _)) if last == close => {
          argv.pop();
        }
        _ => {
          return Err(sherr!(
            SyntaxErr @ span,
            "{opener}: missing matching `{close}` to close the expression"
          ));
        }
      }
    }
    if !argv.is_empty() {
      argv.remove(0);
    }
    Ok((argv, vec![]))
  }

  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let span = args.span();
    let result = ArgvParser::new(&args.argv)
      .parse_or(true)
      .map_err(|e| e.try_blame(span));

    match result {
      Err(e) => {
        Shed::set_status(2);
        Err(e)
      }
      Ok(res) => with_status(i32::from(!res)),
    }
  }
}

#[cfg(test)]
mod tests {
  use crate::state;
  use crate::tests::testutil::{TestGuard, test_input};
  use std::fs;
  use tempfile::{NamedTempFile, TempDir};

  // ===================== Unary: file tests =====================

  #[test]
  fn test_exists_true() {
    let _g = TestGuard::new();
    let file = NamedTempFile::new().unwrap();
    test_input(format!("[[ -e {} ]]", file.path().display())).unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_exists_false() {
    let _g = TestGuard::new();
    test_input("[[ -e /tmp/__no_such_file_test_rs__ ]]").unwrap();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_is_directory() {
    let _g = TestGuard::new();
    let dir = TempDir::new().unwrap();
    test_input(format!("[[ -d {} ]]", dir.path().display())).unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_is_directory_false() {
    let _g = TestGuard::new();
    let file = NamedTempFile::new().unwrap();
    test_input(format!("[[ -d {} ]]", file.path().display())).unwrap();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_is_file() {
    let _g = TestGuard::new();
    let file = NamedTempFile::new().unwrap();
    test_input(format!("[[ -f {} ]]", file.path().display())).unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_is_file_false() {
    let _g = TestGuard::new();
    let dir = TempDir::new().unwrap();
    test_input(format!("[[ -f {} ]]", dir.path().display())).unwrap();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_readable() {
    let _g = TestGuard::new();
    let file = NamedTempFile::new().unwrap();
    test_input(format!("[[ -r {} ]]", file.path().display())).unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_writable() {
    let _g = TestGuard::new();
    let file = NamedTempFile::new().unwrap();
    test_input(format!("[[ -w {} ]]", file.path().display())).unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_non_empty_file() {
    let _g = TestGuard::new();
    let file = NamedTempFile::new().unwrap();
    fs::write(file.path(), "content").unwrap();
    test_input(format!("[[ -s {} ]]", file.path().display())).unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_empty_file() {
    let _g = TestGuard::new();
    let file = NamedTempFile::new().unwrap();
    test_input(format!("[[ -s {} ]]", file.path().display())).unwrap();
    assert_ne!(state::Shed::get_status(), 0);
  }

  // ===================== Unary: string tests =====================

  #[test]
  fn test_non_null_true() {
    let _g = TestGuard::new();
    test_input("[[ -n hello ]]").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_non_null_empty() {
    let _g = TestGuard::new();
    test_input("[[ -n '' ]]").unwrap();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_null_true() {
    let _g = TestGuard::new();
    test_input("[[ -z '' ]]").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_null_false() {
    let _g = TestGuard::new();
    test_input("[[ -z hello ]]").unwrap();
    assert_ne!(state::Shed::get_status(), 0);
  }

  // ===================== Binary: string comparison =====================

  #[test]
  fn test_string_eq() {
    let _g = TestGuard::new();
    test_input("[[ hello == hello ]]").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_string_eq_false() {
    let _g = TestGuard::new();
    test_input("[[ hello == world ]]").unwrap();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_string_neq() {
    let _g = TestGuard::new();
    test_input("[[ hello != world ]]").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_string_neq_false() {
    let _g = TestGuard::new();
    test_input("[[ hello != hello ]]").unwrap();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_string_glob_match() {
    let _g = TestGuard::new();
    test_input("[[ hello == hel* ]]").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_string_glob_no_match() {
    let _g = TestGuard::new();
    test_input("[[ hello == wor* ]]").unwrap();
    assert_ne!(state::Shed::get_status(), 0);
  }

  // ===================== Binary: integer comparison =====================

  #[test]
  fn test_int_eq() {
    let _g = TestGuard::new();
    test_input("[[ 42 -eq 42 ]]").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_int_eq_false() {
    let _g = TestGuard::new();
    test_input("[[ 42 -eq 43 ]]").unwrap();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_int_ne() {
    let _g = TestGuard::new();
    test_input("[[ 1 -ne 2 ]]").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_int_gt() {
    let _g = TestGuard::new();
    test_input("[[ 10 -gt 5 ]]").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_int_gt_false() {
    let _g = TestGuard::new();
    test_input("[[ 5 -gt 10 ]]").unwrap();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_int_lt() {
    let _g = TestGuard::new();
    test_input("[[ 5 -lt 10 ]]").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_int_ge() {
    let _g = TestGuard::new();
    test_input("[[ 10 -ge 10 ]]").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_int_le() {
    let _g = TestGuard::new();
    test_input("[[ 5 -le 5 ]]").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_int_negative() {
    let _g = TestGuard::new();
    test_input("[[ -5 -lt 0 ]]").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_int_non_integer_errors() {
    let _g = TestGuard::new();
    test_input("[[ abc -eq 1 ]]").unwrap();
    // Bash convention: a syntax error inside `[[ ]]` prints the diagnostic and
    // surfaces as a non-zero exit status rather than aborting the shell. We
    // match that — no propagation, just a failed status.
    assert_ne!(state::Shed::get_status(), 0);
  }

  // ===================== Binary: regex match =====================

  #[test]
  fn test_regex_match() {
    let _g = TestGuard::new();
    test_input("[[ hello123 =~ ^hello[0-9]+$ ]]").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_regex_no_match() {
    let _g = TestGuard::new();
    test_input("[[ goodbye =~ ^hello ]]").unwrap();
    assert_ne!(state::Shed::get_status(), 0);
  }

  // ===================== Conjuncts =====================

  #[test]
  fn test_and_both_true() {
    let _g = TestGuard::new();
    test_input("[[ -n hello && -n world ]]").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn no_glob_expansion() {
    let _g = TestGuard::new();
    test_input("[[ 'hello*' == hello* ]]").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_and_first_false() {
    let _g = TestGuard::new();
    test_input("[[ -z hello && -n world ]]").unwrap();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_or_first_true() {
    let _g = TestGuard::new();
    test_input("[[ -n hello || -z hello ]]").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_or_both_false() {
    let _g = TestGuard::new();
    test_input("[[ -z hello || -z world ]]").unwrap();
    assert_ne!(state::Shed::get_status(), 0);
  }

  // ===================== Pure: operator parsing =====================

  #[test]
  fn parse_unary_ops() {
    use super::UnaryOp;
    use std::str::FromStr;
    for op in [
      "-e", "-d", "-f", "-h", "-L", "-r", "-w", "-x", "-s", "-p", "-S", "-b", "-c", "-k", "-O",
      "-G", "-N", "-u", "-g", "-t", "-n", "-z",
    ] {
      assert!(UnaryOp::from_str(op).is_ok(), "failed to parse {op}");
    }
  }

  #[test]
  fn parse_invalid_unary_op() {
    use super::UnaryOp;
    use std::str::FromStr;
    assert!(UnaryOp::from_str("-Q").is_err());
  }

  #[test]
  fn parse_binary_ops() {
    use super::BinaryOp;
    use std::str::FromStr;
    for op in ["==", "!=", "=~", "-eq", "-ne", "-gt", "-lt", "-ge", "-le"] {
      assert!(BinaryOp::from_str(op).is_ok(), "failed to parse {op}");
    }
  }

  #[test]
  fn parse_invalid_binary_op() {
    use super::BinaryOp;
    use std::str::FromStr;
    assert!(BinaryOp::from_str("~=").is_err());
  }

  // ─── Symlink (-h / -L) ──────────────────────────────────────────────

  #[test]
  fn test_symlink_true() {
    let _g = TestGuard::new();
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("target");
    fs::write(&target, b"hi").unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    test_input(format!("[[ -h {} ]]", link.display())).unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_symlink_false_on_regular_file() {
    let _g = TestGuard::new();
    let file = NamedTempFile::new().unwrap();
    test_input(format!("[[ -h {} ]]", file.path().display())).unwrap();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_symlink_capital_l_alias() {
    let _g = TestGuard::new();
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("target");
    fs::write(&target, b"hi").unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    test_input(format!("[[ -L {} ]]", link.display())).unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  // ─── Executable (-x) ────────────────────────────────────────────────

  #[test]
  fn test_executable_true() {
    use std::os::unix::fs::PermissionsExt;
    let _g = TestGuard::new();
    let file = NamedTempFile::new().unwrap();
    let mut perms = fs::metadata(file.path()).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(file.path(), perms).unwrap();
    test_input(format!("[[ -x {} ]]", file.path().display())).unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_executable_false() {
    use std::os::unix::fs::PermissionsExt;
    let _g = TestGuard::new();
    let file = NamedTempFile::new().unwrap();
    let mut perms = fs::metadata(file.path()).unwrap().permissions();
    perms.set_mode(0o644);
    fs::set_permissions(file.path(), perms).unwrap();
    test_input(format!("[[ -x {} ]]", file.path().display())).unwrap();
    assert_ne!(state::Shed::get_status(), 0);
  }

  // ─── NamedPipe (-p) ─────────────────────────────────────────────────

  #[test]
  fn test_named_pipe_true() {
    let _g = TestGuard::new();
    let dir = TempDir::new().unwrap();
    let fifo = dir.path().join("myfifo");
    nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRWXU).unwrap();
    test_input(format!("[[ -p {} ]]", fifo.display())).unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_named_pipe_false_on_regular_file() {
    let _g = TestGuard::new();
    let file = NamedTempFile::new().unwrap();
    test_input(format!("[[ -p {} ]]", file.path().display())).unwrap();
    assert_ne!(state::Shed::get_status(), 0);
  }

  // ─── Socket (-S) ────────────────────────────────────────────────────

  #[test]
  fn test_socket_true() {
    let _g = TestGuard::new();
    let dir = TempDir::new().unwrap();
    let sock_path = dir.path().join("test.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&sock_path).unwrap();
    test_input(format!("[[ -S {} ]]", sock_path.display())).unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_socket_false_on_regular_file() {
    let _g = TestGuard::new();
    let file = NamedTempFile::new().unwrap();
    test_input(format!("[[ -S {} ]]", file.path().display())).unwrap();
    assert_ne!(state::Shed::get_status(), 0);
  }

  // ─── BlockSpecial (-b) / CharSpecial (-c) ───────────────────────────

  #[test]
  fn test_block_special_false_on_regular_file() {
    let _g = TestGuard::new();
    let file = NamedTempFile::new().unwrap();
    test_input(format!("[[ -b {} ]]", file.path().display())).unwrap();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_char_special_true_dev_null() {
    let _g = TestGuard::new();
    test_input("[[ -c /dev/null ]]").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_char_special_false_on_regular_file() {
    let _g = TestGuard::new();
    let file = NamedTempFile::new().unwrap();
    test_input(format!("[[ -c {} ]]", file.path().display())).unwrap();
    assert_ne!(state::Shed::get_status(), 0);
  }

  // ─── Sticky (-k) ────────────────────────────────────────────────────

  #[test]
  fn test_sticky_true_tmpdir() {
    let _g = TestGuard::new();
    test_input("[[ -k /tmp ]]").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_sticky_false_on_regular_file() {
    let _g = TestGuard::new();
    let file = NamedTempFile::new().unwrap();
    test_input(format!("[[ -k {} ]]", file.path().display())).unwrap();
    assert_ne!(state::Shed::get_status(), 0);
  }

  // ─── UIDOwner (-O) / GIDOwner (-G) ──────────────────────────────────

  #[test]
  fn test_uid_owner_true_on_self_created_file() {
    let _g = TestGuard::new();
    let file = NamedTempFile::new().unwrap();
    test_input(format!("[[ -O {} ]]", file.path().display())).unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_gid_owner_true_on_self_created_file() {
    let _g = TestGuard::new();
    let file = NamedTempFile::new().unwrap();
    test_input(format!("[[ -G {} ]]", file.path().display())).unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_uid_owner_false_on_nonexistent_path() {
    let _g = TestGuard::new();
    test_input("[[ -O /__nope_xyz_123 ]]").unwrap();
    assert_ne!(state::Shed::get_status(), 0);
  }

  // ─── SetUID (-u) / SetGID (-g) ──────────────────────────────────────

  #[test]
  fn test_setuid_false_on_regular_file() {
    let _g = TestGuard::new();
    let file = NamedTempFile::new().unwrap();
    test_input(format!("[[ -u {} ]]", file.path().display())).unwrap();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_setgid_false_on_regular_file() {
    let _g = TestGuard::new();
    let file = NamedTempFile::new().unwrap();
    test_input(format!("[[ -g {} ]]", file.path().display())).unwrap();
    assert_ne!(state::Shed::get_status(), 0);
  }

  // ─── ModifiedSinceStatusChange (-N) ─────────────────────────────────

  #[test]
  fn test_modified_since_ctime_false_on_fresh_file() {
    let _g = TestGuard::new();
    let file = NamedTempFile::new().unwrap();
    test_input(format!("[[ -N {} ]]", file.path().display())).unwrap();
    assert_ne!(state::Shed::get_status(), 0);
  }

  // ─── Terminal (-t) ──────────────────────────────────────────────────

  #[test]
  fn test_terminal_false_on_pipe_stdin() {
    let _g = TestGuard::new();
    test_input("[[ -t 0 ]]").unwrap();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_terminal_true_on_pty_stdout() {
    let _g = TestGuard::new();
    test_input("[[ -t 1 ]]").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_terminal_non_numeric_operand_is_false() {
    let _g = TestGuard::new();
    test_input("[[ -t notanumber ]]").unwrap();
    assert_ne!(state::Shed::get_status(), 0);
  }

  // ─── Conjunction short-circuit ──────────────────────────────────────

  #[test]
  fn test_and_short_circuits_on_first_false() {
    let _g = TestGuard::new();
    test_input("[[ -z nonempty && abc -eq 5 ]]").unwrap();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_or_short_circuits_on_first_true() {
    let _g = TestGuard::new();
    test_input("[[ -n nonempty || abc -eq 5 ]]").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  // ─── Misc errors ────────────────────────────────────────────────────

  #[test]
  fn test_regex_invalid_pattern_errors() {
    let _g = TestGuard::new();
    test_input(r#"[[ abc =~ "[" ]]"#).unwrap();
    // Same as test_int_non_integer_errors: an invalid regex inside `[[ ]]`
    // is reported as a failed status, not as an error that escapes the shell.
    assert_ne!(state::Shed::get_status(), 0);
  }
}
