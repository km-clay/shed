use itertools::{EitherOrBoth, Itertools};

use crate::state::vars::{VarStr, VarStrSliceExt};

use super::{
  BuiltinArgs, Dispatcher, NdRule, Node, ShResult,
  opt::{Opt, OptSpec, Word, parse_opts},
  out, outln,
  readline::{BashCompSpec, Candidate, CompContext, CompFlags, CompOptFlags, CompOpts, CompSpec},
  sherr,
  state::{Shed, vars::VarKind},
  with_status,
};

pub(super) struct Complete;
impl super::Builtin for Complete {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::new_short("jobs", 'j'),
      OptSpec::new_short("print_specs", 'p'),
      OptSpec::new_short("remove_spec", 'r'),
      OptSpec::new_short("filenames", 'f'),
      OptSpec::new_short("directories", 'd'),
      OptSpec::new_short("commands", 'c'),
      OptSpec::new_short("users", 'u'),
      OptSpec::new_short("variables", 'v'),
      OptSpec::new_short("aliases", 'a'),
      OptSpec::new_short("builtins", 'b'),
      OptSpec::new_short("signals", 'S'),
      OptSpec::new_short("option", 'o').argc(1),
      OptSpec::new_short("function", 'F').argc(1),
      OptSpec::new_short("wordlist", 'W').argc(1),
      OptSpec::new_short("action", 'A').argc(1),
    ]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let blame = args.span();
    let src = build_source(&args);
    let comp_opts = get_comp_opts(args.options())?;

    if comp_opts.flags.contains(CompFlags::PRINT) {
      if args.arguments().next().is_none() {
        Shed::meta(|m| -> ShResult<()> {
          let specs = m.comp_specs().values();
          for spec in specs {
            outln!("{}", spec.source());
          }
          Ok(())
        })?;
      } else {
        Shed::meta(|m| -> ShResult<()> {
          for (cmd, _) in args.arguments() {
            if let Some(spec) = m.comp_specs().get(cmd) {
              out!("{}", spec.source());
            }
          }
          Ok(())
        })?;
      }

      return with_status(0);
    }

    if comp_opts.flags.contains(CompFlags::REMOVE) {
      Shed::meta_mut(|m| {
        for (cmd, _) in args.arguments() {
          m.remove_comp_spec(cmd.as_str());
        }
      });

      return with_status(0);
    }

    if args.arguments().next().is_none() {
      return Err(sherr!(
        ExecFail @ blame,
        "complete: no command specified",
      ));
    }

    let comp_spec = BashCompSpec::from_comp_opts(comp_opts).with_source(src);

    for (cmd, _) in args.arguments() {
      Shed::meta_mut(|m| m.set_comp_spec(cmd.clone(), Box::new(comp_spec.clone())));
    }

    with_status(0)
  }
}

pub(super) struct CompGen;
impl super::Builtin for CompGen {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::new_short("jobs", 'j'),
      OptSpec::new_short("filenames", 'f'),
      OptSpec::new_short("directories", 'd'),
      OptSpec::new_short("commands", 'c'),
      OptSpec::new_short("users", 'u'),
      OptSpec::new_short("variables", 'v'),
      OptSpec::new_short("aliases", 'a'),
      OptSpec::new_short("signals", 'S'),
      OptSpec::new_short("builtins", 'b'),
      OptSpec::new_short("option", 'o').argc(1),
      OptSpec::new_short("function", 'F').argc(1),
      OptSpec::new_short("wordlist", 'W').argc(1),
    ]
  }
  fn execute(&self, _args: super::BuiltinArgs) -> ShResult<()> {
    unreachable!("CompGen uses run_builtin directly")
  }
  fn run_builtin(&self, node: &Node, _dispatcher: &mut Dispatcher) -> ShResult<()> {
    let NdRule::Command {
      assignments: _,
      argv,
    } = &node.class
    else {
      unreachable!()
    };

    let parsed = parse_opts(argv, &self.opts())?;
    // the whole expanded command line, for the spec's stored source
    let src = parsed.trace.join_with(" ");

    let mut opts = vec![];
    let mut args = vec![];
    for word in parsed.words {
      match word {
        Word::Opt(opt) => opts.push(opt),
        Word::Arg(value, span) => args.push((value, span)),
        Word::Sep(_) => {}
      }
    }

    // the word to complete is the first operand after `compgen` itself
    let mut prefix = args
      .into_iter()
      .nth(1)
      .map(|(v, _)| v.to_string())
      .unwrap_or_default();
    if prefix.as_str() == "--" {
      prefix.clear();
    }
    let comp_opts = get_comp_opts(opts.iter())?;
    let comp_spec = BashCompSpec::from_comp_opts(comp_opts).with_source(src);

    let dummy_ctx = CompContext {
      words: vec![prefix.clone()],
      cword: 0,
      line: prefix.clone(),
      cursor_pos: prefix.as_str().len(),
    };

    let results = comp_spec.complete(&dummy_ctx)?;

    for result in &results {
      outln!("{result}");
    }

    with_status(0)
  }
}

pub(super) struct Compadd;
impl super::Builtin for Compadd {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::new_short("prefix", 'P').argc(1),
      OptSpec::new_short("suffix", 'S').argc(1),
      OptSpec::new_short("desc_arr", 'd').argc(1),
      OptSpec::new_short("desc", 'D').argc(1),
      OptSpec::new_short("cand_arr", 'a').argc(1),
      OptSpec::new_short("assoc_arr", 'A').argc(1),
    ]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let mut prefix: Option<VarStr> = None;
    let mut suffix: Option<VarStr> = None;
    let mut desc_arr: Option<VarStr> = None;
    let mut cand_arr: Option<VarStr> = None;
    let mut assoc_arr: Option<VarStr> = None;
    let mut desc: Option<VarStr> = None;
    for opt in args.options() {
      match opt.key() {
        "desc_arr" => desc_arr = Some(opt.value()?.into()),
        "desc" => desc = Some(opt.value()?.into()),
        "prefix" => prefix = Some(opt.value()?.into()),
        "suffix" => suffix = Some(opt.value()?.into()),
        "cand_arr" => cand_arr = Some(opt.value()?.into()),
        "assoc_arr" => assoc_arr = Some(opt.value()?.into()),
        _ => {}
      }
    }

    let make_candidate = |a: &str| -> Candidate {
      match (&prefix, &suffix) {
        (Some(p), Some(s)) => format!("{p}{a}{s}").into(),
        (Some(p), None) => format!("{p}{a}").into(),
        (None, Some(s)) => format!("{a}{s}").into(),
        (None, None) => Candidate::from(a),
      }
    };

    let mut candidates: Vec<Candidate> = args
      .arguments()
      .map(|(s, _)| s.as_str())
      .map(make_candidate)
      .collect();

    if let Some(cand_arr) = cand_arr {
      let elems: Vec<Candidate> = Shed::vars(|v| v.get_arr_elems(&cand_arr))
        .iter()
        .map(VarStr::as_str)
        .map(make_candidate)
        .collect();

      candidates.extend(elems);
    }

    let descriptions = if let Some(desc) = desc {
      vec![desc; candidates.len()]
    } else if let Some(desc_arr) = desc_arr {
      Shed::vars(|v| v.get_arr_elems(&desc_arr))
    } else {
      vec![]
    };

    let mut described: Vec<Candidate> = candidates
      .into_iter()
      .zip_longest(descriptions)
      .filter_map(|pair| match pair {
        EitherOrBoth::Both(cand, desc) => Some(cand.with_desc(desc)),
        EitherOrBoth::Left(cand) => Some(cand),
        EitherOrBoth::Right(_) => None,
      })
      .collect();

    if let Some(assoc_arr) = assoc_arr
      && let Some(assoc_arr) = Shed::vars(|v| v.try_get_var_meta(&assoc_arr))
      && let VarKind::AssocArr(arr) = assoc_arr.kind()
    {
      for (cand, desc) in arr {
        let cand = make_candidate(cand.as_str()).with_desc(desc.clone());

        described.push(cand);
      }
    }

    Shed::meta_mut(|m| {
      for candidate in described {
        m.comp_add(candidate);
      }
    });

    with_status(0)
  }
}

fn build_source(args: &BuiltinArgs) -> VarStr {
  let mut parts: Vec<VarStr> = vec!["complete".into()];
  for opt in args.options() {
    // the flag as written (e.g. `-W`), followed by its argument words
    parts.push(opt.span().as_str().into());
    for (arg, _) in opt.args() {
      parts.push(arg.clone());
    }
  }
  for (arg, _) in args.arguments() {
    parts.push(arg.clone());
  }
  parts.join_with(" ")
}

pub fn get_comp_opts<'a>(opts: impl Iterator<Item = &'a Opt>) -> ShResult<CompOpts> {
  let mut comp_opts = CompOpts::default();
  comp_opts.opt_flags |= CompOptFlags::SPACE;

  for opt in opts {
    match opt.key() {
      "function" => comp_opts.func = Some(opt.value()?.into()),
      "wordlist" => {
        comp_opts.wordlist = Some(opt.value()?.split_whitespace().map(VarStr::from).collect())
      }
      "action" => comp_opts.action = Some(opt.value()?.into()),
      "option" => match opt.value()? {
        "default" => comp_opts.opt_flags |= CompOptFlags::DEFAULT,
        "dirnames" => comp_opts.opt_flags |= CompOptFlags::DIRNAMES,
        "space" => comp_opts.opt_flags |= CompOptFlags::SPACE,
        "filenames" => comp_opts.opt_flags |= CompOptFlags::FILENAMES,
        "nospace" => comp_opts.opt_flags &= !CompOptFlags::SPACE,
        opt_flag => {
          return Err(sherr!(
            InvalidOpt @ opt.span().clone(),
            "complete: invalid option: {opt_flag}"
          ));
        }
      },

      "aliases" => comp_opts.flags |= CompFlags::ALIAS,
      "signals" => comp_opts.flags |= CompFlags::SIGNALS,
      "remove_spec" => comp_opts.flags |= CompFlags::REMOVE,
      "jobs" => comp_opts.flags |= CompFlags::JOBS,
      "print_specs" => comp_opts.flags |= CompFlags::PRINT,
      "filenames" => comp_opts.flags |= CompFlags::FILES,
      "directories" => comp_opts.flags |= CompFlags::DIRS,
      "commands" => comp_opts.flags |= CompFlags::CMDS,
      "builtins" => comp_opts.flags |= CompFlags::BUILTINS,
      "users" => comp_opts.flags |= CompFlags::USERS,
      "variables" => comp_opts.flags |= CompFlags::VARS,
      _ => unreachable!(),
    }
  }

  Ok(comp_opts)
}

#[cfg(test)]
mod tests {
  use crate::{
    readline::Candidate,
    state::{
      self, Shed,
      meta::MetaTab,
      vars::{VarFlags, VarKind},
    },
    tests::testutil::{TestGuard, test_input},
  };
  use std::fs;
  use tempfile::TempDir;

  // ===================== complete: Registration =====================

  #[test]
  fn complete_register_wordlist() {
    let _g = TestGuard::new();
    test_input("complete -W 'foo bar baz' mycmd").unwrap();

    let spec = Shed::meta(|m| m.get_comp_spec("mycmd"));
    assert!(spec.is_some());
  }

  #[test]
  fn complete_register_files() {
    let _g = TestGuard::new();
    test_input("complete -f mycmd").unwrap();

    let spec = Shed::meta(|m| m.get_comp_spec("mycmd"));
    assert!(spec.is_some());
  }

  #[test]
  fn complete_register_dirs() {
    let _g = TestGuard::new();
    test_input("complete -d mycmd").unwrap();

    let spec = Shed::meta(|m| m.get_comp_spec("mycmd"));
    assert!(spec.is_some());
  }

  #[test]
  fn complete_register_multiple_commands() {
    let _g = TestGuard::new();
    test_input("complete -W 'x y' cmd1 cmd2").unwrap();

    assert!(Shed::meta(|m| m.get_comp_spec("cmd1")).is_some());
    assert!(Shed::meta(|m| m.get_comp_spec("cmd2")).is_some());
  }

  #[test]
  fn complete_register_function() {
    let _g = TestGuard::new();
    test_input("complete -F _my_comp mycmd").unwrap();

    let spec = Shed::meta(|m| m.get_comp_spec("mycmd"));
    assert!(spec.is_some());
  }

  #[test]
  fn complete_register_combined_flags() {
    let _g = TestGuard::new();
    test_input("complete -f -d -v mycmd").unwrap();

    let spec = Shed::meta(|m| m.get_comp_spec("mycmd"));
    assert!(spec.is_some());
  }

  #[test]
  fn complete_overwrite_spec() {
    let _g = TestGuard::new();
    test_input("complete -W 'old' mycmd").unwrap();
    test_input("complete -W 'new' mycmd").unwrap();

    let spec = Shed::meta(|m| m.get_comp_spec("mycmd"));
    assert!(spec.is_some());
    // Verify the source reflects the latest registration
    assert!(spec.unwrap().source().contains("new"));
  }

  #[test]
  fn complete_no_command_fails() {
    let _g = TestGuard::new();
    test_input("complete -W 'foo'").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  // ===================== complete -r: Removal =====================

  #[test]
  fn complete_remove_spec() {
    let _g = TestGuard::new();
    test_input("complete -W 'foo' mycmd").unwrap();
    assert!(Shed::meta(|m| m.get_comp_spec("mycmd")).is_some());

    test_input("complete -r mycmd").unwrap();
    assert!(Shed::meta(|m| m.get_comp_spec("mycmd")).is_none());
  }

  #[test]
  fn complete_remove_multiple() {
    let _g = TestGuard::new();
    test_input("complete -W 'a' cmd1").unwrap();
    test_input("complete -W 'b' cmd2").unwrap();

    test_input("complete -r cmd1 cmd2").unwrap();
    assert!(Shed::meta(|m| m.get_comp_spec("cmd1")).is_none());
    assert!(Shed::meta(|m| m.get_comp_spec("cmd2")).is_none());
  }

  #[test]
  fn complete_remove_nonexistent_is_ok() {
    let _g = TestGuard::new();
    // Removing a spec that doesn't exist should not error
    test_input("complete -r nosuchcmd").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
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
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn complete_option_dirnames() {
    let _g = TestGuard::new();
    test_input("complete -o dirnames -W 'foo' mycmd").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn complete_option_invalid() {
    let _g = TestGuard::new();
    test_input("complete -o bogus -W 'foo' mycmd").ok();
    assert_ne!(state::Shed::get_status(), 0);
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
  fn compgen_emits_ascending_order() {
    // Regression: candidates used to be emitted reverse-alphabetically
    // (a stray `.reverse()` after the ascending sort).
    let guard = TestGuard::new();
    test_input("compgen -W 'delta alpha charlie bravo' ").unwrap();
    let out = guard.read_output();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines, ["alpha", "bravo", "charlie", "delta"]);
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
    Shed::vars_mut(|v| v.set_var("TESTCOMPVAR", VarKind::Str("x".into()), VarFlags::empty()))
      .unwrap();

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
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn compgen_status_zero() {
    let _g = TestGuard::new();
    test_input("compgen -W 'hello'").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  // ===================== compadd =====================

  fn take_comp_candidates() -> Vec<Candidate> {
    Shed::meta_mut(MetaTab::take_comp_candidates)
  }

  fn collect_contents(cands: &[Candidate]) -> Vec<&str> {
    cands.iter().map(Candidate::content).collect()
  }

  #[test]
  fn compadd_basic_words() {
    let _g = TestGuard::new();
    test_input("compadd a b c").unwrap();
    let cands = take_comp_candidates();
    let contents: Vec<&str> = collect_contents(&cands);
    assert_eq!(contents, vec!["a", "b", "c"]);
    for c in &cands {
      assert_eq!(c.desc(), None);
    }
  }

  #[test]
  fn compadd_accumulates_across_calls() {
    let _g = TestGuard::new();
    test_input("compadd a b").unwrap();
    test_input("compadd c d").unwrap();
    let cands = take_comp_candidates();
    let contents: Vec<&str> = collect_contents(&cands);
    assert_eq!(contents, vec!["a", "b", "c", "d"]);
  }

  #[test]
  fn compadd_take_drains() {
    // After draining, a fresh take should return empty.
    let _g = TestGuard::new();
    test_input("compadd x y").unwrap();
    let _ = take_comp_candidates();
    let cands = take_comp_candidates();
    assert!(cands.is_empty(), "candidates should reset after take");
  }

  #[test]
  fn compadd_prefix() {
    let _g = TestGuard::new();
    test_input("compadd -P 'pre_' a b").unwrap();
    let cands = take_comp_candidates();
    let contents: Vec<&str> = collect_contents(&cands);
    assert_eq!(contents, vec!["pre_a", "pre_b"]);
  }

  #[test]
  fn compadd_suffix() {
    let _g = TestGuard::new();
    test_input("compadd -S '_suf' a b").unwrap();
    let cands = take_comp_candidates();
    let contents: Vec<&str> = collect_contents(&cands);
    assert_eq!(contents, vec!["a_suf", "b_suf"]);
  }

  #[test]
  fn compadd_prefix_and_suffix() {
    let _g = TestGuard::new();
    test_input("compadd -P 'p.' -S '=' x y").unwrap();
    let cands = take_comp_candidates();
    let contents: Vec<&str> = collect_contents(&cands);
    assert_eq!(contents, vec!["p.x=", "p.y="]);
  }

  #[test]
  fn compadd_array_source() {
    let _g = TestGuard::new();
    test_input("words=(alpha beta gamma)").unwrap();
    test_input("compadd -a words").unwrap();
    let cands = take_comp_candidates();
    let contents: Vec<&str> = collect_contents(&cands);
    assert_eq!(contents, vec!["alpha", "beta", "gamma"]);
  }

  #[test]
  fn compadd_parallel_descriptions() {
    let _g = TestGuard::new();
    test_input("words=(a b c)").unwrap();
    test_input("descs=(\"first\" \"second\" \"third\")").unwrap();
    test_input("compadd -d descs -a words").unwrap();
    let cands = take_comp_candidates();
    assert_eq!(cands.len(), 3);
    assert_eq!(cands[0].content(), "a");
    assert_eq!(cands[0].desc(), Some("first"));
    assert_eq!(cands[1].content(), "b");
    assert_eq!(cands[1].desc(), Some("second"));
    assert_eq!(cands[2].content(), "c");
    assert_eq!(cands[2].desc(), Some("third"));
  }

  #[test]
  fn compadd_shared_description() {
    // -D applies one description to every candidate.
    let _g = TestGuard::new();
    test_input("compadd -D 'a file' x y z").unwrap();
    let cands = take_comp_candidates();
    assert_eq!(cands.len(), 3);
    for c in &cands {
      assert_eq!(c.desc(), Some("a file"));
    }
  }

  #[test]
  fn compadd_shared_description_with_array() {
    // -D also covers candidates pulled from -a.
    let _g = TestGuard::new();
    test_input("words=(alpha beta)").unwrap();
    test_input("compadd -D 'word' -a words").unwrap();
    let cands = take_comp_candidates();
    assert_eq!(collect_contents(&cands), vec!["alpha", "beta"]);
    for c in &cands {
      assert_eq!(c.desc(), Some("word"));
    }
  }

  #[test]
  fn compadd_extra_descriptions_dropped() {
    // More descriptions than candidates -> extras are silently dropped.
    let _g = TestGuard::new();
    test_input("words=(a b)").unwrap();
    test_input("descs=(\"first\" \"second\" \"third\")").unwrap();
    test_input("compadd -d descs -a words").unwrap();
    let cands = take_comp_candidates();
    assert_eq!(cands.len(), 2);
    assert_eq!(cands[0].desc(), Some("first"));
    assert_eq!(cands[1].desc(), Some("second"));
  }

  #[test]
  fn compadd_fewer_descriptions_leaves_remainder_undescribed() {
    // Fewer descriptions than candidates -> remainder has no description.
    let _g = TestGuard::new();
    test_input("words=(a b c d)").unwrap();
    test_input("descs=(\"only\")").unwrap();
    test_input("compadd -d descs -a words").unwrap();
    let cands = take_comp_candidates();
    assert_eq!(cands.len(), 4);
    assert_eq!(cands[0].desc(), Some("only"));
    assert_eq!(cands[1].desc(), None);
    assert_eq!(cands[2].desc(), None);
    assert_eq!(cands[3].desc(), None);
  }

  #[test]
  fn compadd_prefix_suffix_with_array_source() {
    let _g = TestGuard::new();
    test_input("words=(x y)").unwrap();
    test_input("compadd -P 'opt.' -S '=' -a words").unwrap();
    let cands = take_comp_candidates();
    let contents: Vec<&str> = collect_contents(&cands);
    assert_eq!(contents, vec!["opt.x=", "opt.y="]);
  }

  #[test]
  fn compadd_status_zero() {
    let _g = TestGuard::new();
    test_input("compadd a b c").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  // ===================== compadd -A (assoc-array source) =====================

  #[test]
  fn compadd_assoc_basic() {
    let _g = TestGuard::new();
    test_input("declare -A m=([alpha]=\"first letter\" [beta]=\"second letter\")").unwrap();
    test_input("compadd -A m").unwrap();
    let cands = take_comp_candidates();
    assert_eq!(cands.len(), 2);
    let map: crate::HashMap<&str, Option<&str>> =
      cands.iter().map(|c| (c.content(), c.desc())).collect();
    assert_eq!(map["alpha"], Some("first letter"));
    assert_eq!(map["beta"], Some("second letter"));
  }

  #[test]
  fn compadd_assoc_preserves_insertion_order() {
    // VarKind::AssocArr is Vec-backed, so the order of -A iteration
    // should match the order entries were added.
    let _g = TestGuard::new();
    test_input("declare -A m=([gamma]=g [alpha]=a [beta]=b)").unwrap();
    test_input("compadd -A m").unwrap();
    let cands = take_comp_candidates();
    let order: Vec<&str> = collect_contents(&cands);
    assert_eq!(order, vec!["gamma", "alpha", "beta"]);
  }

  #[test]
  fn compadd_assoc_with_prefix_and_suffix() {
    let _g = TestGuard::new();
    test_input("declare -A m=([n]=normal [i]=insert)").unwrap();
    test_input("compadd -P '-' -S 'X' -A m").unwrap();
    let cands = take_comp_candidates();
    let map: crate::HashMap<&str, Option<&str>> =
      cands.iter().map(|c| (c.content(), c.desc())).collect();
    assert_eq!(map["-nX"], Some("normal"));
    assert_eq!(map["-iX"], Some("insert"));
  }

  #[test]
  fn compadd_assoc_combined_with_parallel_arrays() {
    // -A entries should be additive on top of -a + -d entries.
    let _g = TestGuard::new();
    test_input("words=(p q)").unwrap();
    test_input("descs=(\"P desc\" \"Q desc\")").unwrap();
    test_input("declare -A m=([r]=\"R desc\" [s]=\"S desc\")").unwrap();
    test_input("compadd -a words -d descs -A m").unwrap();
    let cands = take_comp_candidates();
    assert_eq!(cands.len(), 4);
    let map: crate::HashMap<&str, Option<&str>> =
      cands.iter().map(|c| (c.content(), c.desc())).collect();
    assert_eq!(map["p"], Some("P desc"));
    assert_eq!(map["q"], Some("Q desc"));
    assert_eq!(map["r"], Some("R desc"));
    assert_eq!(map["s"], Some("S desc"));
  }

  #[test]
  fn compadd_assoc_combined_with_positional_args() {
    // Positional args should also coexist with -A.
    let _g = TestGuard::new();
    test_input("declare -A m=([y]=yes [n]=no)").unwrap();
    test_input("compadd -A m maybe").unwrap();
    let cands = take_comp_candidates();
    assert_eq!(cands.len(), 3);
    let contents: crate::HashSet<&str> = collect_contents(&cands).into_iter().collect();
    assert!(contents.contains("y"));
    assert!(contents.contains("n"));
    assert!(contents.contains("maybe"));
  }

  #[test]
  fn compadd_assoc_empty_produces_no_candidates() {
    let _g = TestGuard::new();
    test_input("declare -A m").unwrap();
    test_input("compadd -A m").unwrap();
    let cands = take_comp_candidates();
    assert!(cands.is_empty(), "got {cands:?}");
  }

  #[test]
  fn compadd_assoc_unbound_name_silently_skipped() {
    // -A with a name that doesn't resolve to an assoc array (here:
    // doesn't exist at all) should just contribute nothing.
    let _g = TestGuard::new();
    test_input("compadd -A nonexistent_var fallback").unwrap();
    let cands = take_comp_candidates();
    let contents: Vec<&str> = collect_contents(&cands);
    assert_eq!(contents, vec!["fallback"]);
  }

  #[test]
  fn compadd_assoc_wrong_kind_silently_skipped() {
    // -A with a non-assoc var (a plain string here) should also contribute
    // nothing, rather than panicking or producing garbage.
    let _g = TestGuard::new();
    test_input("not_assoc=hello").unwrap();
    test_input("compadd -A not_assoc fallback").unwrap();
    let cands = take_comp_candidates();
    let contents: Vec<&str> = collect_contents(&cands);
    assert_eq!(contents, vec!["fallback"]);
  }
}
