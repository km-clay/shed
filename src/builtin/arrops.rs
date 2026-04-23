use std::collections::VecDeque;

use crate::{
  builtin::BuiltinArgs,
  getopt::{Opt, OptSpec},
  sherr,
  state::{VarFlags, VarKind, write_vars},
  util::{
    error::{ShResult, ShResultExt},
    with_status, write_ln_out,
  },
};

trait ArrOp {
  fn arr_opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::single_arg('c'),
      OptSpec::single_arg('v'),
      OptSpec::flag('r'),
    ]
  }
  fn action(&self) -> Action;
  fn direction(&self) -> End;
  fn exec_arr_op(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let action = self.action();

    match action {
      Action::Push => self.push(args),
      Action::Pop => self.pop(args),
    }
  }
  fn push(&self, args: BuiltinArgs) -> ShResult<()> {
    let end = self.direction();
    if args.argv.is_empty() {
      return Err(sherr!(ParseErr @ args.span(), "missing array name"));
    }
    let mut argv = args.argv.into_iter();

    let name = argv.next().unwrap().0;

    for (val, span) in argv {
      write_vars(|v| {
        if let Ok(arr) = v.get_arr_mut(&name) {
          match end {
            End::Front => arr.push_front(val),
            End::Back => arr.push_back(val),
          }
          Ok(())
        } else {
          v.set_var(&name, VarKind::Arr(VecDeque::from([val])), VarFlags::NONE)
        }
      })
      .blame(span)?;
    }

    with_status(0)
  }
  fn pop(&self, args: BuiltinArgs) -> ShResult<()> {
    let end = self.direction();
    let mut popped = VecDeque::new();
    let mut count = 1;
    let mut var = None;

    for opt in &args.opts {
      match opt {
        Opt::ShortWithArg('c', c) => {
          count = c
            .parse::<usize>()
            .map_err(|_| sherr!(ParseErr @ args.span(), "invalid count: {}", c))?;
        }
        Opt::ShortWithArg('v', v) => {
          var = Some(v);
        }
        Opt::Short('r') => { /* no-op */ }
        _ => {
          return Err(sherr!(ParseErr @ args.span(), "invalid option: '{opt}'"));
        }
      }
    }

    if args.argv.is_empty() {
      return Err(sherr!(ParseErr @ args.span(), "missing array name"));
    }

    for (arg, _) in args.argv {
      for _ in 0..count {
        let pop = |arr: &mut VecDeque<String>| match end {
          End::Front => arr.pop_front(),
          End::Back => arr.pop_back(),
        };
        let Some(popped_val) = write_vars(|v| v.get_arr_mut(&arg).ok().and_then(pop)) else {
          return with_status(1);
        };
        popped.push_back(popped_val);
      }
    }

    if let Some(var) = var {
      if popped.len() == 1 {
        let val = popped.pop_back().unwrap();
        write_vars(|v| v.set_var(var, VarKind::Str(val), VarFlags::NONE))?;
      } else {
        write_vars(|v| v.set_var(var, VarKind::Arr(popped), VarFlags::NONE))?;
      }
    } else {
      for val in popped {
        write_ln_out(val)?;
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
    vec![OptSpec::flag('r'), OptSpec::single_arg('c')]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let mut reverse = false;
    let mut count = 1;
    for opt in &args.opts {
      match opt {
        Opt::Short('r') => reverse = true,
        Opt::ShortWithArg('c', c) => {
          count = c
            .parse::<usize>()
            .map_err(|_| sherr!(ParseErr @ args.span(), "invalid count: {}", c))?;
        }
        _ => {
          return Err(sherr!(ParseErr @ args.span(), "invalid option: '{opt}'"));
        }
      }
    }

    for (arg, _) in &args.argv {
      write_vars(|v| -> ShResult<()> {
        let arr = v.get_arr_mut(arg).promote_err(args.span())?;
        if reverse {
          arr.rotate_right(count.min(arr.len()));
        } else {
          arr.rotate_left(count.min(arr.len()));
        }
        Ok(())
      })?;
    }

    with_status(0)
  }
}

#[cfg(test)]
mod tests {
  use crate::state::{self, VarFlags, VarKind, read_vars, write_vars};
  use crate::testutil::{TestGuard, test_input};
  use std::collections::VecDeque;

  fn set_arr(name: &str, elems: &[&str]) {
    let arr = VecDeque::from_iter(elems.iter().map(|s| s.to_string()));
    write_vars(|v| v.set_var(name, VarKind::Arr(arr), VarFlags::NONE)).unwrap();
  }

  fn get_arr(name: &str) -> Vec<String> {
    read_vars(|v| v.try_get_arr_elems(name)).unwrap()
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
    assert_ne!(state::get_status(), 0);
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
    let val = read_vars(|v| v.get_var("result"));
    assert_eq!(val, "z");
    assert_eq!(get_arr("arr"), vec!["x", "y"]);
  }

  #[test]
  fn pop_empty_array_fails() {
    let _guard = TestGuard::new();
    set_arr("arr", &[]);

    test_input("pop arr").unwrap();
    assert_eq!(state::get_status(), 1);
  }

  #[test]
  fn pop_nonexistent_array() {
    let _guard = TestGuard::new();

    test_input("pop nosucharray").unwrap();
    assert_eq!(state::get_status(), 1);
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
    let val = read_vars(|v| v.get_var("result"));
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
    assert_eq!(state::get_status(), 0);
    test_input("pop arr").unwrap();
    assert_eq!(state::get_status(), 0);
    test_input("pop arr").unwrap();
    assert_eq!(state::get_status(), 1);
  }
}
