use bitflags::bitflags;
use nix::{libc::STDOUT_FILENO, unistd::write};

use crate::{
  builtin::setup_builtin,
  getopt::{Opt, OptSpec, get_opts_from_tokens},
  jobs::JobBldr,
  libsh::error::{ShErr, ShErrKind, ShResult},
  parse::{NdRule, Node},
  procio::{IoStack, borrow_fd},
  readline::complete::{BashCompSpec, CompContext, CompSpec},
  state::{self, read_meta, write_meta},
};

pub const COMPGEN_OPTS: [OptSpec; 11] = [
  OptSpec {
    opt: Opt::Short('F'),
    takes_arg: true,
  },
  OptSpec {
    opt: Opt::Short('W'),
    takes_arg: true,
  },
  OptSpec {
    opt: Opt::Short('j'),
    takes_arg: false,
  },
  OptSpec {
    opt: Opt::Short('f'),
    takes_arg: false,
  },
  OptSpec {
    opt: Opt::Short('d'),
    takes_arg: false,
  },
  OptSpec {
    opt: Opt::Short('c'),
    takes_arg: false,
  },
  OptSpec {
    opt: Opt::Short('u'),
    takes_arg: false,
  },
  OptSpec {
    opt: Opt::Short('v'),
    takes_arg: false,
  },
  OptSpec {
    opt: Opt::Short('a'),
    takes_arg: false,
  },
  OptSpec {
    opt: Opt::Short('S'),
    takes_arg: false,
  },
  OptSpec {
    opt: Opt::Short('o'),
    takes_arg: true,
  },
];

pub const COMP_OPTS: [OptSpec; 14] = [
  OptSpec {
    opt: Opt::Short('F'),
    takes_arg: true,
  },
  OptSpec {
    opt: Opt::Short('W'),
    takes_arg: true,
  },
  OptSpec {
    opt: Opt::Short('A'),
    takes_arg: true,
  },
  OptSpec {
    opt: Opt::Short('j'),
    takes_arg: false,
  },
  OptSpec {
    opt: Opt::Short('p'),
    takes_arg: false,
  },
  OptSpec {
    opt: Opt::Short('r'),
    takes_arg: false,
  },
  OptSpec {
    opt: Opt::Short('f'),
    takes_arg: false,
  },
  OptSpec {
    opt: Opt::Short('d'),
    takes_arg: false,
  },
  OptSpec {
    opt: Opt::Short('c'),
    takes_arg: false,
  },
  OptSpec {
    opt: Opt::Short('u'),
    takes_arg: false,
  },
  OptSpec {
    opt: Opt::Short('v'),
    takes_arg: false,
  },
  OptSpec {
    opt: Opt::Short('a'),
    takes_arg: false,
  },
  OptSpec {
    opt: Opt::Short('S'),
    takes_arg: false,
  },
  OptSpec {
    opt: Opt::Short('o'),
    takes_arg: true,
  },
];

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

pub fn complete_builtin(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
  let blame = node.get_span().clone();
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };
  assert!(!argv.is_empty());
  let src = argv
    .clone()
    .into_iter()
    .map(|tk| tk.expand().map(|tk| tk.get_words().join(" ")))
    .collect::<ShResult<Vec<String>>>()?
    .join(" ");

  let (argv, opts) = get_opts_from_tokens(argv, &COMP_OPTS)?;
  let comp_opts = get_comp_opts(opts)?;
  let (argv, _) = setup_builtin(Some(argv), job, Some((io_stack, node.redirs)))?;
  let argv = argv.unwrap();

  if comp_opts.flags.contains(CompFlags::PRINT) {
    if argv.is_empty() {
      read_meta(|m| {
        let specs = m.comp_specs().values();
        for spec in specs {
          println!("{}", spec.source());
        }
      })
    } else {
      read_meta(|m| {
        for (cmd, _) in &argv {
          if let Some(spec) = m.comp_specs().get(cmd) {
            println!("{}", spec.source());
          }
        }
      })
    }

    state::set_status(0);
    return Ok(());
  }

  if comp_opts.flags.contains(CompFlags::REMOVE) {
    write_meta(|m| {
      for (cmd, _) in &argv {
        m.remove_comp_spec(cmd);
      }
    });

    state::set_status(0);
    return Ok(());
  }

  if argv.is_empty() {
    state::set_status(1);
    return Err(ShErr::at(ShErrKind::ExecFail, blame, "complete: no command specified"));
  }

  let comp_spec = BashCompSpec::from_comp_opts(comp_opts).with_source(src);

  for (cmd, _) in argv {
    write_meta(|m| m.set_comp_spec(cmd, Box::new(comp_spec.clone())));
  }

  state::set_status(0);
  Ok(())
}

pub fn compgen_builtin(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
  let _blame = node.get_span().clone();
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };
  assert!(!argv.is_empty());
  let src = argv
    .clone()
    .into_iter()
    .map(|tk| tk.expand().map(|tk| tk.get_words().join(" ")))
    .collect::<ShResult<Vec<String>>>()?
    .join(" ");

  let (argv, opts) = get_opts_from_tokens(argv, &COMPGEN_OPTS)?;
  let prefix = argv.clone().into_iter().nth(1).unwrap_or_default();
  let comp_opts = get_comp_opts(opts)?;
  let (_, _guard) = setup_builtin(Some(argv), job, Some((io_stack, node.redirs)))?;

  let comp_spec = BashCompSpec::from_comp_opts(comp_opts).with_source(src);

  let dummy_ctx = CompContext {
    words: vec![prefix.clone()],
    cword: 0,
    line: prefix.to_string(),
    cursor_pos: prefix.as_str().len(),
  };

  let results = comp_spec.complete(&dummy_ctx)?;

  let stdout = borrow_fd(STDOUT_FILENO);
  for result in &results {
    write(stdout, result.as_bytes())?;
    write(stdout, b"\n")?;
  }

  state::set_status(0);
  Ok(())
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
        _ => {
          let span: crate::parse::lex::Span = Default::default();
          return Err(ShErr::at(ShErrKind::InvalidOpt, span, format!("complete: invalid option: {}", opt_flag)));
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
