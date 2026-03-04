use crate::{
  expand::expand_keymap, getopt::{Opt, OptSpec, get_opts_from_tokens}, libsh::error::{ShErr, ShErrKind, ShResult, ShResultExt}, parse::{NdRule, Node, execute::prepare_argv}, prelude::*, readline::keys::KeyEvent, state::{self, write_logic}
};

bitflags! {
	#[derive(Debug, Clone, Copy, PartialEq, Eq)]
	pub struct KeyMapFlags: u32 {
		const NORMAL 			= 0b0000001;
		const INSERT 			= 0b0000010;
		const VISUAL 			= 0b0000100;
		const EX 					= 0b0001000;
		const OP_PENDING 	= 0b0010000;
		const REPLACE 		= 0b0100000;
		const VERBATIM 		= 0b1000000;
	}
}

pub struct KeyMapOpts {
	remove: Option<String>,
	flags: KeyMapFlags,
}
impl KeyMapOpts {
	pub fn from_opts(opts: &[Opt]) -> ShResult<Self> {
		let mut flags = KeyMapFlags::empty();
		let mut remove = None;
		for opt in opts {
			match opt {
				Opt::Short('n') => flags |= KeyMapFlags::NORMAL,
				Opt::Short('i') => flags |= KeyMapFlags::INSERT,
				Opt::Short('v') => flags |= KeyMapFlags::VISUAL,
				Opt::Short('x') => flags |= KeyMapFlags::EX,
				Opt::Short('o') => flags |= KeyMapFlags::OP_PENDING,
				Opt::Short('r') => flags |= KeyMapFlags::REPLACE,
				Opt::LongWithArg(name, arg) if name == "remove" => {
					if remove.is_some() {
						return Err(ShErr::simple(ShErrKind::ExecFail, "Duplicate --remove option for keymap".to_string()));
					}
					remove = Some(arg.clone());
				},
				_ => return Err(ShErr::simple(ShErrKind::ExecFail, format!("Invalid option for keymap: {:?}", opt))),
			}
		}
		if flags.is_empty() {
			return Err(ShErr::simple(ShErrKind::ExecFail, "At least one mode option must be specified for keymap".to_string()).with_note("Use -n for normal mode, -i for insert mode, -v for visual mode, -x for ex mode, and -o for operator-pending mode".to_string()));
		}
		Ok(Self { remove, flags })
	}
	pub fn keymap_opts() -> [OptSpec;6] {
		[
			OptSpec {
				opt: Opt::Short('n'), // normal mode
				takes_arg: false
			},
			OptSpec {
				opt: Opt::Short('i'), // insert mode
				takes_arg: false
			},
			OptSpec {
				opt: Opt::Short('v'), // visual mode
				takes_arg: false
			},
			OptSpec {
				opt: Opt::Short('x'), // ex mode
				takes_arg: false
			},
			OptSpec {
				opt: Opt::Short('o'), // operator-pending mode
				takes_arg: false
			},
			OptSpec {
				opt: Opt::Short('r'), // replace mode
				takes_arg: false
			},
		]
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyMapMatch {
	NoMatch,
	IsPrefix,
	IsExact
}

#[derive(Debug, Clone)]
pub struct KeyMap {
	pub flags: KeyMapFlags,
	pub keys: String,
	pub action: String
}

impl KeyMap {
	pub fn keys_expanded(&self) -> Vec<KeyEvent> {
		expand_keymap(&self.keys)
	}
	pub fn action_expanded(&self) -> Vec<KeyEvent> {
		expand_keymap(&self.action)
	}
	pub fn compare(&self, other: &[KeyEvent]) -> KeyMapMatch {
		log::debug!("Comparing keymap keys {:?} with input {:?}", self.keys_expanded(), other);
		let ours = self.keys_expanded();
		if other == ours {
			KeyMapMatch::IsExact
		} else if ours.starts_with(other) {
			KeyMapMatch::IsPrefix
		} else {
			KeyMapMatch::NoMatch
		}
	}
}

pub fn keymap(node: Node) -> ShResult<()> {
  let span = node.get_span();
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

	let (argv, opts) = get_opts_from_tokens(argv, &KeyMapOpts::keymap_opts())?;
	let opts = KeyMapOpts::from_opts(&opts).promote_err(span.clone())?;
	if let Some(to_rm) = opts.remove {
		write_logic(|l| l.remove_keymap(&to_rm));
		state::set_status(0);
		return Ok(());
	}

  let mut argv = prepare_argv(argv)?;
  if !argv.is_empty() { argv.remove(0); }

	let Some((keys,_)) = argv.first() else {
		return Err(ShErr::at(ShErrKind::ExecFail, span, "missing keys argument".to_string()));
	};

	let Some((action,_)) = argv.get(1) else {
		return Err(ShErr::at(ShErrKind::ExecFail, span, "missing action argument".to_string()));
	};

	let keymap = KeyMap {
		flags: opts.flags,
		keys: keys.clone(),
		action: action.clone(),
	};

	write_logic(|l| l.insert_keymap(keymap));

  state::set_status(0);
  Ok(())
}
