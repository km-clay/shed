use std::{fs::metadata, path::PathBuf, str::FromStr};

use nix::{
  sys::stat::{self, SFlag},
  unistd::AccessFlags,
};
use regex::Regex;

use crate::{
  libsh::error::{ShErr, ShResult},
  parse::{ConjunctOp, NdRule, Node, TEST_UNARY_OPS, TestCase},
  sherr,
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
      _ => Err(sherr!(SyntaxErr, "Invalid test operator")),
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
      _ => Err(sherr!(SyntaxErr, "Invalid test operator '{s}'")),
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
  log::trace!("test cases: {:#?}", cases);

  for case in cases {
    let result = match case {
      TestCase::Unary {
        operator,
        operand,
        conjunct,
      } => {
        let operand = operand.expand_no_glob()?.get_words().join(" ");
        conjunct_op = conjunct;
        let TestOp::Unary(op) = TestOp::from_str(operator.as_str())? else {
          return Err(sherr!(
            SyntaxErr @ err_span,
            "Invalid unary operator",
          ));
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
        let lhs = lhs.expand_no_glob()?.get_words().join(" ");
        let rhs = rhs.expand_no_glob()?.get_words().join(" ");
        conjunct_op = conjunct;
        let test_op = operator.as_str().parse::<TestOp>()?;
        match test_op {
          TestOp::Unary(_) => {
            return Err(sherr!(
              SyntaxErr @ err_span,
              "Expected a binary operator in this test call; found a unary operator",
            ));
          }
          TestOp::StringEq => {
            let pattern = crate::expand::glob_to_regex(rhs.trim(), true);
            pattern.is_match(lhs.trim())
          }
          TestOp::StringNeq => {
            let pattern = crate::expand::glob_to_regex(rhs.trim(), true);
            !pattern.is_match(lhs.trim())
          }
          TestOp::IntNeq
          | TestOp::IntGt
          | TestOp::IntLt
          | TestOp::IntGe
          | TestOp::IntLe
          | TestOp::IntEq => {
            let err = sherr!(
              SyntaxErr @ err_span.clone(),
              "Expected an integer with '{operator}' operator"
            );
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

    last_result = result;
    if let Some(op) = conjunct_op {
      match op {
        ConjunctOp::And if !last_result => break,
        ConjunctOp::Or if last_result => break,
        _ => {}
      }
    }
  }
  Ok(last_result)
}

#[cfg(test)]
mod tests {
  use crate::state;
  use crate::testutil::{TestGuard, test_input};
  use std::fs;
  use tempfile::{NamedTempFile, TempDir};

  // ===================== Unary: file tests =====================

  #[test]
  fn test_exists_true() {
    let _g = TestGuard::new();
    let file = NamedTempFile::new().unwrap();
    test_input(format!("[[ -e {} ]]", file.path().display())).unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn test_exists_false() {
    let _g = TestGuard::new();
    test_input("[[ -e /tmp/__no_such_file_test_rs__ ]]").unwrap();
    assert_ne!(state::get_status(), 0);
  }

  #[test]
  fn test_is_directory() {
    let _g = TestGuard::new();
    let dir = TempDir::new().unwrap();
    test_input(format!("[[ -d {} ]]", dir.path().display())).unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn test_is_directory_false() {
    let _g = TestGuard::new();
    let file = NamedTempFile::new().unwrap();
    test_input(format!("[[ -d {} ]]", file.path().display())).unwrap();
    assert_ne!(state::get_status(), 0);
  }

  #[test]
  fn test_is_file() {
    let _g = TestGuard::new();
    let file = NamedTempFile::new().unwrap();
    test_input(format!("[[ -f {} ]]", file.path().display())).unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn test_is_file_false() {
    let _g = TestGuard::new();
    let dir = TempDir::new().unwrap();
    test_input(format!("[[ -f {} ]]", dir.path().display())).unwrap();
    assert_ne!(state::get_status(), 0);
  }

  #[test]
  fn test_readable() {
    let _g = TestGuard::new();
    let file = NamedTempFile::new().unwrap();
    test_input(format!("[[ -r {} ]]", file.path().display())).unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn test_writable() {
    let _g = TestGuard::new();
    let file = NamedTempFile::new().unwrap();
    test_input(format!("[[ -w {} ]]", file.path().display())).unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn test_non_empty_file() {
    let _g = TestGuard::new();
    let file = NamedTempFile::new().unwrap();
    fs::write(file.path(), "content").unwrap();
    test_input(format!("[[ -s {} ]]", file.path().display())).unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn test_empty_file() {
    let _g = TestGuard::new();
    let file = NamedTempFile::new().unwrap();
    test_input(format!("[[ -s {} ]]", file.path().display())).unwrap();
    assert_ne!(state::get_status(), 0);
  }

  // ===================== Unary: string tests =====================

  #[test]
  fn test_non_null_true() {
    let _g = TestGuard::new();
    test_input("[[ -n hello ]]").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn test_non_null_empty() {
    let _g = TestGuard::new();
    test_input("[[ -n '' ]]").unwrap();
    assert_ne!(state::get_status(), 0);
  }

  #[test]
  fn test_null_true() {
    let _g = TestGuard::new();
    test_input("[[ -z '' ]]").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn test_null_false() {
    let _g = TestGuard::new();
    test_input("[[ -z hello ]]").unwrap();
    assert_ne!(state::get_status(), 0);
  }

  // ===================== Binary: string comparison =====================

  #[test]
  fn test_string_eq() {
    let _g = TestGuard::new();
    test_input("[[ hello == hello ]]").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn test_string_eq_false() {
    let _g = TestGuard::new();
    test_input("[[ hello == world ]]").unwrap();
    assert_ne!(state::get_status(), 0);
  }

  #[test]
  fn test_string_neq() {
    let _g = TestGuard::new();
    test_input("[[ hello != world ]]").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn test_string_neq_false() {
    let _g = TestGuard::new();
    test_input("[[ hello != hello ]]").unwrap();
    assert_ne!(state::get_status(), 0);
  }

  #[test]
  fn test_string_glob_match() {
    let _g = TestGuard::new();
    test_input("[[ hello == hel* ]]").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn test_string_glob_no_match() {
    let _g = TestGuard::new();
    test_input("[[ hello == wor* ]]").unwrap();
    assert_ne!(state::get_status(), 0);
  }

  // ===================== Binary: integer comparison =====================

  #[test]
  fn test_int_eq() {
    let _g = TestGuard::new();
    test_input("[[ 42 -eq 42 ]]").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn test_int_eq_false() {
    let _g = TestGuard::new();
    test_input("[[ 42 -eq 43 ]]").unwrap();
    assert_ne!(state::get_status(), 0);
  }

  #[test]
  fn test_int_ne() {
    let _g = TestGuard::new();
    test_input("[[ 1 -ne 2 ]]").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn test_int_gt() {
    let _g = TestGuard::new();
    test_input("[[ 10 -gt 5 ]]").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn test_int_gt_false() {
    let _g = TestGuard::new();
    test_input("[[ 5 -gt 10 ]]").unwrap();
    assert_ne!(state::get_status(), 0);
  }

  #[test]
  fn test_int_lt() {
    let _g = TestGuard::new();
    test_input("[[ 5 -lt 10 ]]").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn test_int_ge() {
    let _g = TestGuard::new();
    test_input("[[ 10 -ge 10 ]]").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn test_int_le() {
    let _g = TestGuard::new();
    test_input("[[ 5 -le 5 ]]").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn test_int_negative() {
    let _g = TestGuard::new();
    test_input("[[ -5 -lt 0 ]]").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn test_int_non_integer_errors() {
    let _g = TestGuard::new();
    let result = test_input("[[ abc -eq 1 ]]");
    assert!(result.is_err());
  }

  // ===================== Binary: regex match =====================

  #[test]
  fn test_regex_match() {
    let _g = TestGuard::new();
    test_input("[[ hello123 =~ ^hello[0-9]+$ ]]").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn test_regex_no_match() {
    let _g = TestGuard::new();
    test_input("[[ goodbye =~ ^hello ]]").unwrap();
    assert_ne!(state::get_status(), 0);
  }

  // ===================== Conjuncts =====================

  #[test]
  fn test_and_both_true() {
    let _g = TestGuard::new();
    test_input("[[ -n hello && -n world ]]").unwrap();
    assert_eq!(state::get_status(), 0);
  }

	#[test]
	fn no_glob_expansion() {
		let _g = TestGuard::new();
		test_input("[[ 'hello*' == hello* ]]").unwrap();
		assert_eq!(state::get_status(), 0);
	}

  #[test]
  fn test_and_first_false() {
    let _g = TestGuard::new();
    test_input("[[ -z hello && -n world ]]").unwrap();
    assert_ne!(state::get_status(), 0);
  }

  #[test]
  fn test_or_first_true() {
    let _g = TestGuard::new();
    test_input("[[ -n hello || -z hello ]]").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn test_or_both_false() {
    let _g = TestGuard::new();
    test_input("[[ -z hello || -z world ]]").unwrap();
    assert_ne!(state::get_status(), 0);
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
    use super::TestOp;
    use std::str::FromStr;
    for op in ["==", "!=", "=~", "-eq", "-ne", "-gt", "-lt", "-ge", "-le"] {
      assert!(TestOp::from_str(op).is_ok(), "failed to parse {op}");
    }
  }

  #[test]
  fn parse_invalid_binary_op() {
    use super::TestOp;
    use std::str::FromStr;
    assert!(TestOp::from_str("~=").is_err());
  }

}
