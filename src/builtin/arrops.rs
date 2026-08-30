//! Contains builtins that operate on shell arrays.
//!
//! `shed`'s array variables are actually [`VecDeque`] under the hood,
//! so the goal of these builtins is to provide access to the deque's double-ended
//! interaction surface.
//!
//! `pop` - removes the last element of an array and prints it to stdout. ([`VecDeque::pop_back()`])
//! `fpop` - removes the first element of an array and prints it to stdout. ([`VecDeque::pop_front()`])
//! `push` - adds one or more elements to the end of an array. ([`VecDeque::push_back()`])
//! `fpush` - adds one or more elements to the front of an array. ([`VecDeque::push_front()`])
//! `rotate` - rotates the elements of an array left or right. ([`VecDeque::rotate_left()`] and [`VecDeque::rotate_right()`])

use std::collections::VecDeque;

use crate::procio::outln_bytes;
use crate::state::vars::VarStr;

use super::{
  BuiltinArgs, ShResult, Shed,
  opt::OptSpec,
  sherr,
  state::vars::{VarFlags, VarKind},
  util::ShResultExt,
  with_status,
};

/// Trait that provides common functionality for array operations like push and pop.
///
/// Each of the array operation builtins (`push`, `pop`, `fpush`, `fpop`) implement this trait,
/// because the only difference between each one is the direction of the operation (front or back)
/// and whether it is a push or pop.
trait ArrOp {
  /// Common options
  fn arr_opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::new("count").short(b'c').argc(1),
      OptSpec::new("variable").short(b'v').argc(1),
      OptSpec::new("reverse").short(b'r'),
    ]
  }
  /// Whether we are pushing or popping
  fn action(&self) -> Action;
  /// Which side of the array we are acting on
  fn direction(&self) -> End;
  /// Executes the array operation, dispatching to either `push` or `pop` based on the action.
  fn exec_arr_op(&self, args: super::BuiltinArgs) -> ShResult<()> {
    match self.action() {
      Action::Push => self.push(args),
      Action::Pop => self.pop(args),
    }
  }
  fn push(&self, args: BuiltinArgs) -> ShResult<()> {
    let end = self.direction();
    if args.no_arguments() {
      return Err(sherr!(ParseErr @ args.span(), "missing array name"));
    }
    let mut arguments = args.arguments();
    let name = arguments.next().unwrap().0;

    // each argument is pushed to the array
    Shed::vars_mut(|v| -> ShResult<()> {
      for (val, span) in arguments {
        match v.get_arr_mut(&name.to_str_lossy()).ok() {
          Some(arr) => match end {
            End::Front => arr.push_front(val.clone()),
            End::Back => arr.push_back(val.clone()),
          },
          None => {
            v.set_var(
              &name.to_str_lossy(),
              VarKind::arr([val.clone()]),
              VarFlags::empty(),
            )
            .promote_err(span.clone())?;
          }
        }
      }
      Ok(())
    })?;

    with_status(0)
  }
  fn pop(&self, args: BuiltinArgs) -> ShResult<()> {
    let end = self.direction();
    let mut popped = VecDeque::new();
    let mut count = 1;
    let mut var = None;

    for opt in args.options() {
      match opt.key() {
        "count" => {
          let c = opt.value()?;
          count = c
            .parse::<usize>()
            .map_err(|_| sherr!(ParseErr @ opt.span(), "invalid count: {c}"))?;
        }
        "variable" => var = opt.value().ok(),
        "reverse" => { /* no-op */ }
        _ => {
          return Err(sherr!(ParseErr @ opt.span().clone(), "invalid option: '{opt}'"));
        }
      }
    }

    if args.no_arguments() {
      return Err(sherr!(ParseErr @ args.span(), "missing array name"));
    }

    for (arg, _) in args.arguments() {
      for _ in 0..count {
        let pop = |arr: &mut VecDeque<VarStr>| match end {
          End::Front => arr.pop_front(),
          End::Back => arr.pop_back(),
        };
        let Some(popped_val) =
          Shed::vars_mut(|v| v.get_arr_mut(&arg.to_str_lossy()).ok().and_then(pop))
        else {
          return with_status(1);
        };
        popped.push_back(popped_val);
      }
    }

    if let Some(var) = var {
      if popped.len() == 1 {
        let val = popped.pop_back().unwrap();
        Shed::vars_mut(|v| v.set_var(var, VarKind::Str(val), VarFlags::empty()))?;
      } else {
        Shed::vars_mut(|v| v.set_var(var, VarKind::arr(popped), VarFlags::empty()))?;
      }
    } else {
      for val in popped {
        outln_bytes(val.as_bytes());
      }
    }

    with_status(0)
  }
}

#[derive(Clone, Copy)]
enum End {
  Front,
  Back,
}

#[derive(Clone, Copy)]
enum Action {
  Push,
  Pop,
}

pub(super) struct FrontPop;
impl ArrOp for FrontPop {
  fn action(&self) -> Action {
    Action::Pop
  }
  fn direction(&self) -> End {
    End::Front
  }
}
impl super::Builtin for FrontPop {
  fn opts(&self) -> Vec<OptSpec> {
    self.arr_opts()
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    self.exec_arr_op(args)
  }
}

pub(super) struct Pop;
impl ArrOp for Pop {
  fn action(&self) -> Action {
    Action::Pop
  }
  fn direction(&self) -> End {
    End::Back
  }
}
impl super::Builtin for Pop {
  fn opts(&self) -> Vec<OptSpec> {
    self.arr_opts()
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    self.exec_arr_op(args)
  }
}

pub(super) struct FrontPush;
impl ArrOp for FrontPush {
  fn action(&self) -> Action {
    Action::Push
  }
  fn direction(&self) -> End {
    End::Front
  }
}
impl super::Builtin for FrontPush {
  fn opts(&self) -> Vec<OptSpec> {
    self.arr_opts()
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    self.exec_arr_op(args)
  }
}

pub(super) struct Push;
impl ArrOp for Push {
  fn action(&self) -> Action {
    Action::Push
  }
  fn direction(&self) -> End {
    End::Back
  }
}
impl super::Builtin for Push {
  fn opts(&self) -> Vec<OptSpec> {
    self.arr_opts()
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    self.exec_arr_op(args)
  }
}

pub(super) struct Rotate;
impl super::Builtin for Rotate {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::new("reverse").short(b'r'),
      OptSpec::new("count").short(b'c').argc(1),
    ]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let mut reverse = false;
    let mut count = 1;
    for opt in args.options() {
      match opt.key() {
        "reverse" => reverse = true,
        "count" => {
          let c = opt.value()?;
          count = c
            .parse::<usize>()
            .map_err(|_| sherr!(ParseErr @ opt.span(), "invalid count: {c}"))?;
        }
        _ => {
          return Err(sherr!(ParseErr @ args.span(), "invalid option: '{opt}'"));
        }
      }
    }

    Shed::vars_mut(|v| -> ShResult<()> {
      // each arg is a variable name of an array to rotate
      for (arg, _) in args.arguments() {
        let arr = v
          .get_arr_mut(&arg.to_str_lossy())
          .promote_err(args.span())?;
        if reverse {
          arr.rotate_right(count.min(arr.len()));
        } else {
          arr.rotate_left(count.min(arr.len()));
        }
      }
      Ok(())
    })?;

    with_status(0)
  }
}

#[cfg(test)]
mod tests {
  use crate::{
    state::{
      self, Shed,
      vars::{VarFlags, VarKind, VarStr},
    },
    tests::testutil::{TestGuard, test_input},
    var,
  };
  use std::collections::VecDeque;

  fn set_arr(name: &str, elems: &[&str]) {
    let arr = elems
      .iter()
      .map(ToString::to_string)
      .collect::<VecDeque<_>>();
    Shed::vars_mut(|v| {
      v.set_var(
        name,
        VarKind::arr(arr.into_iter().map(Into::into)),
        VarFlags::empty(),
      )
    })
    .unwrap();
  }

  fn get_arr(name: &str) -> Vec<VarStr> {
    Shed::vars(|v| v.try_get_arr_elems(name)).unwrap()
  }

  // ===================== push =====================

  #[test]
  fn push_to_existing_array() {
    let _guard = TestGuard::new();
    set_arr("arr", &["a", "b"]);

    test_input("push arr c").unwrap();
    assert_eq!(get_arr("arr"), vec!["a", "b", "c"]);
  }

  #[test]
  fn push_creates_array() {
    let _guard = TestGuard::new();

    test_input("push newarr hello").unwrap();
    assert_eq!(get_arr("newarr"), vec!["hello"]);
  }

  #[test]
  fn push_multiple_values() {
    let _guard = TestGuard::new();
    set_arr("arr", &["a"]);

    test_input("push arr b c d").unwrap();
    assert_eq!(get_arr("arr"), vec!["a", "b", "c", "d"]);
  }

  #[test]
  fn push_no_array_name() {
    let _guard = TestGuard::new();
    test_input("push").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  // ===================== fpush =====================

  #[test]
  fn fpush_to_existing_array() {
    let _guard = TestGuard::new();
    set_arr("arr", &["b", "c"]);

    test_input("fpush arr a").unwrap();
    assert_eq!(get_arr("arr"), vec!["a", "b", "c"]);
  }

  #[test]
  fn fpush_multiple_values() {
    let _guard = TestGuard::new();
    set_arr("arr", &["c"]);

    test_input("fpush arr a b").unwrap();
    // Each value is pushed to the front in order: c -> a,c -> b,a,c
    assert_eq!(get_arr("arr"), vec!["b", "a", "c"]);
  }

  #[test]
  fn fpush_creates_array() {
    let _guard = TestGuard::new();

    test_input("fpush newarr x").unwrap();
    assert_eq!(get_arr("newarr"), vec!["x"]);
  }

  // ===================== pop =====================

  #[test]
  fn pop_removes_last() {
    let guard = TestGuard::new();
    set_arr("arr", &["a", "b", "c"]);

    test_input("pop arr").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "c\n");
    assert_eq!(get_arr("arr"), vec!["a", "b"]);
  }

  #[test]
  fn pop_with_count() {
    let guard = TestGuard::new();
    set_arr("arr", &["a", "b", "c", "d"]);

    test_input("pop -c 2 arr").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "d\nc\n");
    assert_eq!(get_arr("arr"), vec!["a", "b"]);
  }

  #[test]
  fn pop_into_variable() {
    let _guard = TestGuard::new();
    set_arr("arr", &["x", "y", "z"]);

    test_input("pop -v result arr").unwrap();
    let val = var!("result");
    assert_eq!(val, "z");
    assert_eq!(get_arr("arr"), vec!["x", "y"]);
  }

  #[test]
  fn pop_empty_array_fails() {
    let _guard = TestGuard::new();
    set_arr("arr", &[]);

    test_input("pop arr").unwrap();
    assert_eq!(state::Shed::get_status(), 1);
  }

  #[test]
  fn pop_nonexistent_array() {
    let _guard = TestGuard::new();

    test_input("pop nosucharray").unwrap();
    assert_eq!(state::Shed::get_status(), 1);
  }

  // ===================== fpop =====================

  #[test]
  fn fpop_removes_first() {
    let guard = TestGuard::new();
    set_arr("arr", &["a", "b", "c"]);

    test_input("fpop arr").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "a\n");
    assert_eq!(get_arr("arr"), vec!["b", "c"]);
  }

  #[test]
  fn fpop_with_count() {
    let guard = TestGuard::new();
    set_arr("arr", &["a", "b", "c", "d"]);

    test_input("fpop -c 2 arr").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "a\nb\n");
    assert_eq!(get_arr("arr"), vec!["c", "d"]);
  }

  #[test]
  fn fpop_into_variable() {
    let _guard = TestGuard::new();
    set_arr("arr", &["first", "second"]);

    test_input("fpop -v result arr").unwrap();
    let val = var!("result");
    assert_eq!(val, "first");
    assert_eq!(get_arr("arr"), vec!["second"]);
  }

  // ===================== rotate =====================

  #[test]
  fn rotate_left_default() {
    let _guard = TestGuard::new();
    set_arr("arr", &["a", "b", "c", "d"]);

    test_input("rotate arr").unwrap();
    assert_eq!(get_arr("arr"), vec!["b", "c", "d", "a"]);
  }

  #[test]
  fn rotate_left_with_count() {
    let _guard = TestGuard::new();
    set_arr("arr", &["a", "b", "c", "d"]);

    test_input("rotate -c 2 arr").unwrap();
    assert_eq!(get_arr("arr"), vec!["c", "d", "a", "b"]);
  }

  #[test]
  fn rotate_right() {
    let _guard = TestGuard::new();
    set_arr("arr", &["a", "b", "c", "d"]);

    test_input("rotate -r arr").unwrap();
    assert_eq!(get_arr("arr"), vec!["d", "a", "b", "c"]);
  }

  #[test]
  fn rotate_right_with_count() {
    let _guard = TestGuard::new();
    set_arr("arr", &["a", "b", "c", "d"]);

    test_input("rotate -r -c 2 arr").unwrap();
    assert_eq!(get_arr("arr"), vec!["c", "d", "a", "b"]);
  }

  #[test]
  fn rotate_count_exceeds_len() {
    let _guard = TestGuard::new();
    set_arr("arr", &["a", "b"]);

    // count clamped to arr.len(), so rotate by 2 on len=2 is a no-op
    test_input("rotate -c 5 arr").unwrap();
    assert_eq!(get_arr("arr"), vec!["a", "b"]);
  }

  #[test]
  fn rotate_single_element() {
    let _guard = TestGuard::new();
    set_arr("arr", &["only"]);

    test_input("rotate arr").unwrap();
    assert_eq!(get_arr("arr"), vec!["only"]);
  }

  // ===================== combined ops =====================

  #[test]
  fn push_then_pop_roundtrip() {
    let guard = TestGuard::new();
    set_arr("arr", &["a"]);

    test_input("push arr b").unwrap();
    test_input("pop arr").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "b\n");
    assert_eq!(get_arr("arr"), vec!["a"]);
  }

  #[test]
  fn fpush_then_fpop_roundtrip() {
    let guard = TestGuard::new();
    set_arr("arr", &["a"]);

    test_input("fpush arr z").unwrap();
    test_input("fpop arr").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "z\n");
    assert_eq!(get_arr("arr"), vec!["a"]);
  }

  #[test]
  fn pop_until_empty() {
    let _guard = TestGuard::new();
    set_arr("arr", &["x", "y"]);

    test_input("pop arr").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
    test_input("pop arr").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
    test_input("pop arr").unwrap();
    assert_eq!(state::Shed::get_status(), 1);
  }
}
