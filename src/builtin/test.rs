use std::{fs::metadata, path::PathBuf, str::FromStr};

use nix::{
  sys::stat::{self, SFlag},
  unistd::AccessFlags,
};
use regex::Regex;

use crate::{
  libsh::error::{ShErr, ShErrKind, ShResult},
  parse::{ConjunctOp, NdRule, Node, TEST_UNARY_OPS, TestCase},
  prelude::*,
};

#[derive(Debug, Clone)]
pub enum UnaryOp {
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
}

impl FromStr for UnaryOp {
  type Err = ShErr;
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "-e" => Ok(Self::Exists),
      "-d" => Ok(Self::Directory),
      "-f" => Ok(Self::File),
      "-h" | "-L" => Ok(Self::Symlink), // -h or -L
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
      _ => Err(ShErr::Simple {
        kind: ShErrKind::SyntaxErr,
        msg: "Invalid test operator".into(),
        notes: vec![],
      }),
    }
  }
}

#[derive(Debug, Clone)]
pub enum TestOp {
  Unary(UnaryOp),
  StringEq,   // ==
  StringNeq,  // !=
  IntEq,      // -eq
  IntNeq,     // -ne
  IntGt,      // -gt
  IntLt,      // -lt
  IntGe,      // -ge
  IntLe,      // -le
  RegexMatch, // =~
}

impl FromStr for TestOp {
  type Err = ShErr;
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "==" => Ok(Self::StringEq),
      "!=" => Ok(Self::StringNeq),
      "=~" => Ok(Self::RegexMatch),
      "-eq" => Ok(Self::IntEq),
      "-ne" => Ok(Self::IntNeq),
      "-gt" => Ok(Self::IntGt),
      "-lt" => Ok(Self::IntLt),
      "-ge" => Ok(Self::IntGe),
      "-le" => Ok(Self::IntLe),
      _ if TEST_UNARY_OPS.contains(&s) => Ok(Self::Unary(s.parse::<UnaryOp>()?)),
      _ => Err(ShErr::Simple {
        kind: ShErrKind::SyntaxErr,
        msg: "Invalid test operator".into(),
        notes: vec![],
      }),
    }
  }
}

fn replace_posix_classes(pat: &str) -> String {
  pat
    .replace("[[:alnum:]]", r"[A-Za-z0-9]")
    .replace("[[:alpha:]]", r"[A-Za-z]")
    .replace("[[:blank:]]", r"[ \t]")
    .replace("[[:cntrl:]]", r"[\x00-\x1F\x7F]")
    .replace("[[:digit:]]", r"[0-9]")
    .replace("[[:graph:]]", r"[!-~]")
    .replace("[[:lower:]]", r"[a-z]")
    .replace("[[:print:]]", r"[\x20-\x7E]")
    .replace("[[:space:]]", r"[ \t\r\n\x0B\x0C]") // vertical tab (\x0B), form feed (\x0C)
    .replace("[[:upper:]]", r"[A-Z]")
    .replace("[[:xdigit:]]", r"[0-9A-Fa-f]")
}

pub fn double_bracket_test(node: Node) -> ShResult<bool> {
  let err_span = node.get_span();
  let NdRule::Test { cases } = node.class else {
    unreachable!()
  };
  let mut last_result = false;
  let mut conjunct_op: Option<ConjunctOp>;

  for case in cases {
    let result = match case {
      TestCase::Unary {
        operator,
        operand,
        conjunct,
      } => {
        let operand = operand.expand()?.get_words().join(" ");
        conjunct_op = conjunct;
        let TestOp::Unary(op) = TestOp::from_str(operator.as_str())? else {
          return Err(ShErr::Full {
            kind: ShErrKind::SyntaxErr,
            msg: "Invalid unary operator".into(),
            notes: vec![],
            span: err_span,
          });
        };
        match op {
          UnaryOp::Exists => {
            let path = PathBuf::from(operand.as_str());
            path.exists()
          }
          UnaryOp::Directory => {
            let path = PathBuf::from(operand.as_str());
            if path.exists() {
              path.metadata().unwrap().is_dir()
            } else {
              false
            }
          }
          UnaryOp::File => {
            let path = PathBuf::from(operand.as_str());
            if path.exists() {
              path.metadata().unwrap().is_file()
            } else {
              false
            }
          }
          UnaryOp::Symlink => {
            let path = PathBuf::from(operand.as_str());
            if path.exists() {
              path.metadata().unwrap().file_type().is_symlink()
            } else {
              false
            }
          }
          UnaryOp::Readable => nix::unistd::access(operand.as_str(), AccessFlags::R_OK).is_ok(),
          UnaryOp::Writable => nix::unistd::access(operand.as_str(), AccessFlags::W_OK).is_ok(),
          UnaryOp::Executable => nix::unistd::access(operand.as_str(), AccessFlags::X_OK).is_ok(),
          UnaryOp::NonEmpty => match metadata(operand.as_str()) {
            Ok(meta) => meta.len() > 0,
            Err(_) => false,
          },
          UnaryOp::NamedPipe => match stat::stat(operand.as_str()) {
            Ok(stat) => SFlag::from_bits_truncate(stat.st_mode).contains(SFlag::S_IFIFO),
            Err(_) => false,
          },
          UnaryOp::Socket => match stat::stat(operand.as_str()) {
            Ok(stat) => SFlag::from_bits_truncate(stat.st_mode).contains(SFlag::S_IFSOCK),
            Err(_) => false,
          },
          UnaryOp::BlockSpecial => match stat::stat(operand.as_str()) {
            Ok(stat) => SFlag::from_bits_truncate(stat.st_mode).contains(SFlag::S_IFBLK),
            Err(_) => false,
          },
          UnaryOp::CharSpecial => match stat::stat(operand.as_str()) {
            Ok(stat) => SFlag::from_bits_truncate(stat.st_mode).contains(SFlag::S_IFCHR),
            Err(_) => false,
          },
          UnaryOp::Sticky => match stat::stat(operand.as_str()) {
            Ok(stat) => stat.st_mode & nix::libc::S_ISVTX != 0,
            Err(_) => false,
          },
          UnaryOp::UIDOwner => match stat::stat(operand.as_str()) {
            Ok(stat) => stat.st_uid == nix::unistd::geteuid().as_raw(),
            Err(_) => false,
          },

          UnaryOp::GIDOwner => match stat::stat(operand.as_str()) {
            Ok(stat) => stat.st_gid == nix::unistd::getegid().as_raw(),
            Err(_) => false,
          },

          UnaryOp::ModifiedSinceStatusChange => match stat::stat(operand.as_str()) {
            Ok(stat) => stat.st_mtime > stat.st_ctime,
            Err(_) => false,
          },

          UnaryOp::SetUID => match stat::stat(operand.as_str()) {
            Ok(stat) => stat.st_mode & nix::libc::S_ISUID != 0,
            Err(_) => false,
          },

          UnaryOp::SetGID => match stat::stat(operand.as_str()) {
            Ok(stat) => stat.st_mode & nix::libc::S_ISGID != 0,
            Err(_) => false,
          },

          UnaryOp::Terminal => match operand.as_str().parse::<nix::libc::c_int>() {
            Ok(fd) => unsafe { nix::libc::isatty(fd) == 1 },
            Err(_) => false,
          },
          UnaryOp::NonNull => !operand.is_empty(),
          UnaryOp::Null => operand.is_empty(),
        }
      }
      TestCase::Binary {
        lhs,
        operator,
        rhs,
        conjunct,
      } => {
        let lhs = lhs.expand()?.get_words().join(" ");
        let rhs = rhs.expand()?.get_words().join(" ");
        conjunct_op = conjunct;
        let test_op = operator.as_str().parse::<TestOp>()?;
        match test_op {
          TestOp::Unary(_) => {
            return Err(ShErr::Full {
              kind: ShErrKind::SyntaxErr,
              msg: "Expected a binary operator in this test call; found a unary operator".into(),
              notes: vec![],
              span: err_span,
            });
          }
          TestOp::StringEq => rhs.trim() == lhs.trim(),
          TestOp::StringNeq => rhs.trim() != lhs.trim(),
          TestOp::IntNeq
          | TestOp::IntGt
          | TestOp::IntLt
          | TestOp::IntGe
          | TestOp::IntLe
          | TestOp::IntEq => {
            let err = ShErr::Full {
              kind: ShErrKind::SyntaxErr,
              msg: format!("Expected an integer with '{}' operator", operator.as_str()),
              notes: vec![],
              span: err_span.clone(),
            };
            let Ok(lhs) = lhs.trim().parse::<i32>() else {
              return Err(err);
            };
            let Ok(rhs) = rhs.trim().parse::<i32>() else {
              return Err(err);
            };
            match test_op {
              TestOp::IntNeq => lhs != rhs,
              TestOp::IntGt => lhs > rhs,
              TestOp::IntLt => lhs < rhs,
              TestOp::IntGe => lhs >= rhs,
              TestOp::IntLe => lhs <= rhs,
              TestOp::IntEq => lhs == rhs,
              _ => unreachable!(),
            }
          }
          TestOp::RegexMatch => {
            // FIXME: Imagine doing all of this in every single iteration of a loop
            let cleaned = replace_posix_classes(&rhs);
            let regex = Regex::new(&cleaned).unwrap();
            regex.is_match(&lhs)
          }
        }
      }
    };

    if let Some(op) = conjunct_op {
      match op {
        ConjunctOp::And if !last_result => {
          last_result = result;
          break;
        }
        ConjunctOp::Or if last_result => {
          last_result = result;
          break;
        }
        _ => {}
      }
    } else {
      last_result = result;
    }
  }
  Ok(last_result)
}
