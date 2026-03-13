use std::collections::VecDeque;

use crate::{
  getopt::{Opt, OptSpec, get_opts_from_tokens},
  libsh::error::{ShErr, ShErrKind, ShResult, ShResultExt},
  parse::{NdRule, Node, execute::prepare_argv},
  prelude::*,
  procio::borrow_fd,
  state::{self, VarFlags, VarKind, write_vars},
};

fn arr_op_optspec() -> Vec<OptSpec> {
  vec![
    OptSpec {
      opt: Opt::Short('c'),
      takes_arg: true,
    },
    OptSpec {
      opt: Opt::Short('r'),
      takes_arg: false,
    },
    OptSpec {
      opt: Opt::Short('v'),
      takes_arg: true,
    },
  ]
}

pub struct ArrOpOpts {
  count: usize,
  reverse: bool,
  var: Option<String>,
}

impl Default for ArrOpOpts {
  fn default() -> Self {
    Self {
      count: 1,
      reverse: false,
      var: None,
    }
  }
}

#[derive(Clone, Copy)]
enum End {
  Front,
  Back,
}

fn arr_pop_inner(node: Node, end: End) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let (argv, opts) = get_opts_from_tokens(argv, &arr_op_optspec())?;
  let arr_op_opts = get_arr_op_opts(opts)?;
  let mut argv = prepare_argv(argv)?;
  if !argv.is_empty() {
    argv.remove(0);
  }
  let stdout = borrow_fd(STDOUT_FILENO);
  let mut status = 0;

  for (arg, _) in argv {
    for _ in 0..arr_op_opts.count {
      let pop = |arr: &mut std::collections::VecDeque<String>| match end {
        End::Front => arr.pop_front(),
        End::Back => arr.pop_back(),
      };
      let Some(popped) = write_vars(|v| v.get_arr_mut(&arg).ok().and_then(pop)) else {
        status = 1;
        break;
      };
      status = 0;

      if let Some(ref var) = arr_op_opts.var {
        write_vars(|v| v.set_var(var, VarKind::Str(popped), VarFlags::NONE))?;
      } else {
        write(stdout, popped.as_bytes())?;
        write(stdout, b"\n")?;
      }
    }
  }

  state::set_status(status);
  Ok(())
}

fn arr_push_inner(node: Node, end: End) -> ShResult<()> {
  let blame = node.get_span().clone();
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let (argv, opts) = get_opts_from_tokens(argv, &arr_op_optspec())?;
  let _arr_op_opts = get_arr_op_opts(opts)?;
  let mut argv = prepare_argv(argv)?;
  if !argv.is_empty() {
    argv.remove(0);
  }

  let mut argv = argv.into_iter();
  let Some((name, _)) = argv.next() else {
    return Err(ShErr::at(
      ShErrKind::ExecFail,
      blame,
      "push: missing array name".to_string(),
    ));
  };

  for (val, span) in argv {
    let push_val = val.clone();
    write_vars(|v| {
      if let Ok(arr) = v.get_arr_mut(&name) {
        match end {
          End::Front => arr.push_front(push_val),
          End::Back => arr.push_back(push_val),
        }
        Ok(())
      } else {
        v.set_var(
          &name,
          VarKind::Arr(VecDeque::from([push_val])),
          VarFlags::NONE,
        )
      }
    })
    .blame(span)?;
  }

  state::set_status(0);
  Ok(())
}

pub fn arr_pop(node: Node) -> ShResult<()> {
  arr_pop_inner(node, End::Back)
}

pub fn arr_fpop(node: Node) -> ShResult<()> {
  arr_pop_inner(node, End::Front)
}

pub fn arr_push(node: Node) -> ShResult<()> {
  arr_push_inner(node, End::Back)
}

pub fn arr_fpush(node: Node) -> ShResult<()> {
  arr_push_inner(node, End::Front)
}

pub fn arr_rotate(node: Node) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let (argv, opts) = get_opts_from_tokens(argv, &arr_op_optspec())?;
  let arr_op_opts = get_arr_op_opts(opts)?;
  let mut argv = prepare_argv(argv)?;
  if !argv.is_empty() {
    argv.remove(0);
  }

  for (arg, _) in argv {
    write_vars(|v| -> ShResult<()> {
      let arr = v.get_arr_mut(&arg)?;
      if arr_op_opts.reverse {
        arr.rotate_right(arr_op_opts.count.min(arr.len()));
      } else {
        arr.rotate_left(arr_op_opts.count.min(arr.len()));
      }
      Ok(())
    })?;
  }

  state::set_status(0);
  Ok(())
}

pub fn get_arr_op_opts(opts: Vec<Opt>) -> ShResult<ArrOpOpts> {
  let mut arr_op_opts = ArrOpOpts::default();
  for opt in opts {
    match opt {
      Opt::ShortWithArg('c', count) => {
        arr_op_opts.count = count
          .parse::<usize>()
          .map_err(|_| ShErr::simple(ShErrKind::ParseErr, format!("invalid count: {}", count)))?;
      }
      Opt::Short('c') => {
        return Err(ShErr::simple(
          ShErrKind::ParseErr,
          "missing count for -c".to_string(),
        ));
      }
      Opt::Short('r') => {
        arr_op_opts.reverse = true;
      }
      Opt::ShortWithArg('v', var) => {
        arr_op_opts.var = Some(var);
      }
      Opt::Short('v') => {
        return Err(ShErr::simple(
          ShErrKind::ParseErr,
          "missing variable name for -v".to_string(),
        ));
      }
      _ => {
        return Err(ShErr::simple(
          ShErrKind::ParseErr,
          format!("invalid option: {}", opt),
        ));
      }
    }
  }
  Ok(arr_op_opts)
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
    read_vars(|v| v.get_arr_elems(name)).unwrap()
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
    let result = test_input("push");
    assert!(result.is_err());
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
