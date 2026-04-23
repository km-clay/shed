use bitflags::bitflags;
use nix::{libc::STDOUT_FILENO, unistd::write};

use crate::{
  getopt::{Opt, OptSpec},
  outln,
  parse::{NdRule, Node},
  procio::borrow_fd,
  readline::complete::{BashCompSpec, CompContext, CompSpec},
  sherr,
  state::{read_meta, write_meta},
  util::{error::ShResult, with_status, write_out},
};

bitflags! {
  #[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Hash)]
  pub struct CompFlags: u32 {
    const FILES   = 0b0000000001;
    const DIRS    = 0b0000000010;
    const CMDS    = 0b0000000100;
    const USERS   = 0b0000001000;
    const VARS    = 0b0000010000;
    const JOBS    = 0b0000100000;
    const ALIAS   = 0b0001000000;
    const SIGNALS = 0b0010000000;
    const PRINT   = 0b0100000000;
    const REMOVE  = 0b1000000000;
  }
  #[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Hash)]
  pub struct CompOptFlags: u32 {
    const DEFAULT  = 0b0000000001;
    const DIRNAMES = 0b0000000010;
    const SPACE    = 0b0000000100;
  }
}

#[derive(Default, Debug, Clone)]
pub struct CompOpts {
  pub func: Option<String>,
  pub wordlist: Option<Vec<String>>,
  pub action: Option<String>,
  pub flags: CompFlags,
  pub opt_flags: CompOptFlags,
}

pub(super) struct Complete;
impl super::Builtin for Complete {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::flag('j'),
      OptSpec::flag('p'),
      OptSpec::flag('r'),
      OptSpec::flag('f'),
      OptSpec::flag('d'),
      OptSpec::flag('c'),
      OptSpec::flag('u'),
      OptSpec::flag('v'),
      OptSpec::flag('a'),
      OptSpec::flag('S'),
      OptSpec::single_arg('o'),
      OptSpec::single_arg('F'),
      OptSpec::single_arg('W'),
      OptSpec::single_arg('A'),
    ]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let blame = args.span();
    let src = build_source(&args.opts, &args.argv);
    let comp_opts = get_comp_opts(args.opts)?;

    if comp_opts.flags.contains(CompFlags::PRINT) {
      if args.argv.is_empty() {
        read_meta(|m| -> ShResult<()> {
          let specs = m.comp_specs().values();
          for spec in specs {
            write_out(spec.source())?;
          }
          Ok(())
        })?;
      } else {
        read_meta(|m| -> ShResult<()> {
          for (cmd, _) in &args.argv {
            if let Some(spec) = m.comp_specs().get(cmd) {
              let stdout = borrow_fd(STDOUT_FILENO);
              write(stdout, spec.source().as_bytes())?;
            }
          }
          Ok(())
        })?;
      }

      return with_status(0);
    }

    if comp_opts.flags.contains(CompFlags::REMOVE) {
      write_meta(|m| {
        for (cmd, _) in &args.argv {
          m.remove_comp_spec(cmd);
        }
      });

      return with_status(0);
    }

    if args.argv.is_empty() {
      return Err(sherr!(
        ExecFail @ blame,
        "complete: no command specified",
      ));
    }

    let comp_spec = BashCompSpec::from_comp_opts(comp_opts).with_source(src);

    for (cmd, _) in args.argv {
      write_meta(|m| m.set_comp_spec(cmd, Box::new(comp_spec.clone())));
    }

    with_status(0)
  }
}

pub(super) struct CompGen;
impl super::Builtin for CompGen {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::flag('j'),
      OptSpec::flag('f'),
      OptSpec::flag('d'),
      OptSpec::flag('c'),
      OptSpec::flag('u'),
      OptSpec::flag('v'),
      OptSpec::flag('a'),
      OptSpec::flag('S'),
      OptSpec::single_arg('o'),
      OptSpec::single_arg('F'),
      OptSpec::single_arg('W'),
    ]
  }
  fn execute(&self, _args: super::BuiltinArgs) -> ShResult<()> {
    unreachable!("CompGen uses run_builtin directly")
  }
  fn run_builtin(&self, node: Node) -> ShResult<()> {
    use crate::getopt::get_opts_from_tokens_raw;

    let NdRule::Command {
      assignments: _,
      argv,
    } = node.class
    else {
      unreachable!()
    };
    let src = argv
      .iter()
      .map(|tk| tk.clone().expand().map(|tk| tk.get_words().join(" ")))
      .collect::<ShResult<Vec<String>>>()?
      .join(" ");

    let (argv, opts) = get_opts_from_tokens_raw(argv, &self.opts())?;

    let prefix = argv.into_iter().nth(1).unwrap_or_default();
    let comp_opts = get_comp_opts(opts)?;
    let comp_spec = BashCompSpec::from_comp_opts(comp_opts).with_source(src);

    let dummy_ctx = CompContext {
      words: vec![prefix.clone()],
      cword: 0,
      line: prefix.to_string(),
      cursor_pos: prefix.as_str().len(),
    };

    let results = comp_spec.complete(&dummy_ctx)?;

    for result in &results {
      outln!("{result}")?;
    }

    with_status(0)
  }
}

fn build_source(opts: &[Opt], argv: &[(String, crate::parse::lex::Span)]) -> String {
  let mut parts: Vec<String> = vec!["complete".into()];
  for opt in opts {
    match opt {
      Opt::Short(c) => parts.push(format!("-{c}")),
      Opt::Long(s) => parts.push(format!("--{s}")),
      Opt::ShortWithArg(c, a) => {
        parts.push(format!("-{c}"));
        parts.push(a.clone());
      }
      Opt::LongWithArg(s, a) => {
        parts.push(format!("--{s}"));
        parts.push(a.clone());
      }
      _ => {}
    }
  }
  for (s, _) in argv {
    parts.push(s.clone());
  }
  parts.join(" ")
}

pub fn get_comp_opts(opts: Vec<Opt>) -> ShResult<CompOpts> {
  let mut comp_opts = CompOpts::default();

  for opt in opts {
    match opt {
      Opt::ShortWithArg('F', func) => {
        comp_opts.func = Some(func);
      }
      Opt::ShortWithArg('W', wordlist) => {
        comp_opts.wordlist = Some(wordlist.split_whitespace().map(|s| s.to_string()).collect());
      }
      Opt::ShortWithArg('A', action) => {
        comp_opts.action = Some(action);
      }
      Opt::ShortWithArg('o', opt_flag) => match opt_flag.as_str() {
        "default" => comp_opts.opt_flags |= CompOptFlags::DEFAULT,
        "dirnames" => comp_opts.opt_flags |= CompOptFlags::DIRNAMES,
        "space" => comp_opts.opt_flags |= CompOptFlags::SPACE,
        "nospace" => comp_opts.opt_flags &= !CompOptFlags::SPACE,
        _ => {
          let span: crate::parse::lex::Span = Default::default();
          return Err(sherr!(
            InvalidOpt @ span,
            "complete: invalid option: {opt_flag}"
          ));
        }
      },

      Opt::Short('a') => comp_opts.flags |= CompFlags::ALIAS,
      Opt::Short('S') => comp_opts.flags |= CompFlags::SIGNALS,
      Opt::Short('r') => comp_opts.flags |= CompFlags::REMOVE,
      Opt::Short('j') => comp_opts.flags |= CompFlags::JOBS,
      Opt::Short('p') => comp_opts.flags |= CompFlags::PRINT,
      Opt::Short('f') => comp_opts.flags |= CompFlags::FILES,
      Opt::Short('d') => comp_opts.flags |= CompFlags::DIRS,
      Opt::Short('c') => comp_opts.flags |= CompFlags::CMDS,
      Opt::Short('u') => comp_opts.flags |= CompFlags::USERS,
      Opt::Short('v') => comp_opts.flags |= CompFlags::VARS,
      _ => unreachable!(),
    }
  }

  Ok(comp_opts)
}

#[cfg(test)]
mod tests {
  use crate::state::{self, VarFlags, VarKind, read_meta, write_vars};
  use crate::testutil::{TestGuard, test_input};
  use std::fs;
  use tempfile::TempDir;

  // ===================== complete: Registration =====================

  #[test]
  fn complete_register_wordlist() {
    let _g = TestGuard::new();
    test_input("complete -W 'foo bar baz' mycmd").unwrap();

    let spec = read_meta(|m| m.get_comp_spec("mycmd"));
    assert!(spec.is_some());
  }

  #[test]
  fn complete_register_files() {
    let _g = TestGuard::new();
    test_input("complete -f mycmd").unwrap();

    let spec = read_meta(|m| m.get_comp_spec("mycmd"));
    assert!(spec.is_some());
  }

  #[test]
  fn complete_register_dirs() {
    let _g = TestGuard::new();
    test_input("complete -d mycmd").unwrap();

    let spec = read_meta(|m| m.get_comp_spec("mycmd"));
    assert!(spec.is_some());
  }

  #[test]
  fn complete_register_multiple_commands() {
    let _g = TestGuard::new();
    test_input("complete -W 'x y' cmd1 cmd2").unwrap();

    assert!(read_meta(|m| m.get_comp_spec("cmd1")).is_some());
    assert!(read_meta(|m| m.get_comp_spec("cmd2")).is_some());
  }

  #[test]
  fn complete_register_function() {
    let _g = TestGuard::new();
    test_input("complete -F _my_comp mycmd").unwrap();

    let spec = read_meta(|m| m.get_comp_spec("mycmd"));
    assert!(spec.is_some());
  }

  #[test]
  fn complete_register_combined_flags() {
    let _g = TestGuard::new();
    test_input("complete -f -d -v mycmd").unwrap();

    let spec = read_meta(|m| m.get_comp_spec("mycmd"));
    assert!(spec.is_some());
  }

  #[test]
  fn complete_overwrite_spec() {
    let _g = TestGuard::new();
    test_input("complete -W 'old' mycmd").unwrap();
    test_input("complete -W 'new' mycmd").unwrap();

    let spec = read_meta(|m| m.get_comp_spec("mycmd"));
    assert!(spec.is_some());
    // Verify the source reflects the latest registration
    assert!(spec.unwrap().source().contains("new"));
  }

  #[test]
  fn complete_no_command_fails() {
    let _g = TestGuard::new();
    test_input("complete -W 'foo'").ok();
    assert_ne!(state::get_status(), 0);
  }

  // ===================== complete -r: Removal =====================

  #[test]
  fn complete_remove_spec() {
    let _g = TestGuard::new();
    test_input("complete -W 'foo' mycmd").unwrap();
    assert!(read_meta(|m| m.get_comp_spec("mycmd")).is_some());

    test_input("complete -r mycmd").unwrap();
    assert!(read_meta(|m| m.get_comp_spec("mycmd")).is_none());
  }

  #[test]
  fn complete_remove_multiple() {
    let _g = TestGuard::new();
    test_input("complete -W 'a' cmd1").unwrap();
    test_input("complete -W 'b' cmd2").unwrap();

    test_input("complete -r cmd1 cmd2").unwrap();
    assert!(read_meta(|m| m.get_comp_spec("cmd1")).is_none());
    assert!(read_meta(|m| m.get_comp_spec("cmd2")).is_none());
  }

  #[test]
  fn complete_remove_nonexistent_is_ok() {
    let _g = TestGuard::new();
    // Removing a spec that doesn't exist should not error
    test_input("complete -r nosuchcmd").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  // ===================== complete -p: Print =====================

  #[test]
  fn complete_print_specific() {
    let guard = TestGuard::new();
    test_input("complete -W 'alpha beta' mycmd").unwrap();
    guard.read_output();

    test_input("complete -p mycmd").unwrap();
    let out = guard.read_output();
    assert!(out.contains("mycmd"));
  }

  #[test]
  fn complete_print_all() {
    let guard = TestGuard::new();
    // Clear any existing specs and register two
    test_input("complete -W 'a' cmd1").unwrap();
    test_input("complete -W 'b' cmd2").unwrap();
    guard.read_output();

    test_input("complete -p").unwrap();
    let out = guard.read_output();
    assert!(out.contains("cmd1"));
    assert!(out.contains("cmd2"));
  }

  // ===================== complete -o: Option flags =====================

  #[test]
  fn complete_option_default() {
    let _g = TestGuard::new();
    test_input("complete -o default -W 'foo' mycmd").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn complete_option_dirnames() {
    let _g = TestGuard::new();
    test_input("complete -o dirnames -W 'foo' mycmd").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn complete_option_invalid() {
    let _g = TestGuard::new();
    test_input("complete -o bogus -W 'foo' mycmd").ok();
    assert_ne!(state::get_status(), 0);
  }

  // ===================== compgen -W: Word list =====================

  #[test]
  fn compgen_wordlist_no_prefix() {
    let guard = TestGuard::new();
    test_input("compgen -W 'alpha beta gamma'").unwrap();
    let out = guard.read_output();
    assert!(out.contains("alpha"));
    assert!(out.contains("beta"));
    assert!(out.contains("gamma"));
  }

  #[test]
  fn compgen_wordlist_with_prefix() {
    let guard = TestGuard::new();
    test_input("compgen -W 'apple banana avocado' a").unwrap();
    let out = guard.read_output();
    assert!(out.contains("apple"));
    assert!(out.contains("avocado"));
    assert!(!out.contains("banana"));
  }

  #[test]
  fn compgen_wordlist_no_match() {
    let guard = TestGuard::new();
    test_input("compgen -W 'foo bar baz' z").unwrap();
    let out = guard.read_output();
    assert!(out.trim().is_empty());
  }

  #[test]
  fn compgen_wordlist_exact_match() {
    let guard = TestGuard::new();
    test_input("compgen -W 'hello help helm' hel").unwrap();
    let out = guard.read_output();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3);
  }

  #[test]
  fn compgen_wordlist_single_match() {
    let guard = TestGuard::new();
    test_input("compgen -W 'alpha beta gamma' g").unwrap();
    let out = guard.read_output();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0], "gamma");
  }

  #[test]
  fn compgen_wordlist_double_dash() {
    let guard = TestGuard::new();
    test_input("compgen -W 'alpha beta gamma' -- \"g\"").unwrap();
    let out = guard.read_output();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0], "gamma");
  }

  // ===================== compgen -v: Variables =====================

  #[test]
  fn compgen_variables() {
    let guard = TestGuard::new();
    write_vars(|v| v.set_var("TESTCOMPVAR", VarKind::Str("x".into()), VarFlags::NONE)).unwrap();

    test_input("compgen -v TESTCOMP").unwrap();
    let out = guard.read_output();
    assert!(out.contains("TESTCOMPVAR"));
  }

  // ===================== compgen -a: Aliases =====================

  #[test]
  fn compgen_aliases() {
    let guard = TestGuard::new();
    test_input("alias testcompalias='echo hi'").unwrap();
    guard.read_output();

    test_input("compgen -a testcomp").unwrap();
    let out = guard.read_output();
    assert!(out.contains("testcompalias"));
  }

  // ===================== compgen -d: Directories =====================

  #[test]
  fn compgen_dirs() {
    let guard = TestGuard::new();
    let tmp = TempDir::new().unwrap();
    let sub = tmp.path().join("subdir");
    fs::create_dir(&sub).unwrap();

    let prefix = format!("{}/", tmp.path().display());
    test_input(format!("compgen -d {prefix}")).unwrap();
    let out = guard.read_output();
    assert!(out.contains("subdir"));
  }

  // ===================== compgen -f: Files =====================

  #[test]
  fn compgen_files() {
    let guard = TestGuard::new();
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("testfile.txt"), "").unwrap();
    fs::create_dir(tmp.path().join("testdir")).unwrap();

    let prefix = format!("{}/test", tmp.path().display());
    test_input(format!("compgen -f {prefix}")).unwrap();
    let out = guard.read_output();
    assert!(out.contains("testfile.txt"));
    assert!(out.contains("testdir"));
  }

  // ===================== compgen -F: Completion function =====================

  #[test]
  fn compgen_function() {
    let guard = TestGuard::new();
    // Define a completion function that sets COMPREPLY
    test_input("_mycomp() { COMPREPLY=(opt1 opt2 opt3); }").unwrap();
    guard.read_output();

    test_input("compgen -F _mycomp").unwrap();
    let out = guard.read_output();
    assert!(out.contains("opt1"));
    assert!(out.contains("opt2"));
    assert!(out.contains("opt3"));
  }

  // ===================== compgen: combined flags =====================

  #[test]
  fn compgen_wordlist_and_aliases() {
    let guard = TestGuard::new();
    test_input("alias testcga='true'").unwrap();
    guard.read_output();

    test_input("compgen -W 'testcgw' -a testcg").unwrap();
    let out = guard.read_output();
    assert!(out.contains("testcgw"));
    assert!(out.contains("testcga"));
  }

  // ===================== Status =====================

  #[test]
  fn complete_status_zero() {
    let _g = TestGuard::new();
    test_input("complete -W 'x' mycmd").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn compgen_status_zero() {
    let _g = TestGuard::new();
    test_input("compgen -W 'hello'").unwrap();
    assert_eq!(state::get_status(), 0);
  }
}
