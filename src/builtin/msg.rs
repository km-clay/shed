use crate::{
  builtin::join_raw_args,
  getopt::{Opt, OptSpec, get_opts_from_tokens},
  libsh::error::{ShErr, ShErrKind, ShResult},
  parse::{NdRule, Node},
  prelude::*,
  state::{self, write_meta},
};

bitflags! {
  pub struct MsgFlags: u32 {
    const SYSTEM = 1 << 0;
    const STATUS = 1 << 1;
  }
}

fn msg_opts() -> [OptSpec; 4] {
  [
    OptSpec {
      opt: Opt::Long("status".into()),
      takes_arg: false,
    },
    OptSpec {
      opt: Opt::Long("system".into()),
      takes_arg: false,
    },
    OptSpec {
      opt: Opt::Short('s'),
      takes_arg: false,
    },
    OptSpec {
      opt: Opt::Short('S'),
      takes_arg: false,
    },
  ]
}

pub fn msg(node: Node) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let (mut argv, opts) = get_opts_from_tokens(argv, &msg_opts())?;
  let flags = get_msg_flags(opts)?;
  argv.remove(0);
  let (msg, _span) = join_raw_args(argv);

  if flags.contains(MsgFlags::SYSTEM) {
    write_meta(|m| {
      m.post_system_message(msg);
    })
  } else if flags.contains(MsgFlags::STATUS) {
    write_meta(|m| {
      m.post_status_message(msg);
    })
  } else {
    // just default to status messages i guess?
    write_meta(|m| {
      m.post_status_message(msg);
    })
  }

  state::set_status(0);
  Ok(())
}

pub fn get_msg_flags(opts: Vec<Opt>) -> ShResult<MsgFlags> {
  let mut flags = MsgFlags::empty();

  for opt in opts {
    match opt {
      Opt::Short('S') => flags |= MsgFlags::SYSTEM,
      Opt::Short('s') => flags |= MsgFlags::STATUS,
      Opt::Long(o) if o.as_str() == "system" => flags |= MsgFlags::SYSTEM,
      Opt::Long(o) if o.as_str() == "status" => flags |= MsgFlags::STATUS,
      _ => {
        return Err(ShErr::simple(
          ShErrKind::ExecFail,
          format!("msg: Unexpected flag '{opt}'"),
        ));
      }
    }
  }

  Ok(flags)
}
