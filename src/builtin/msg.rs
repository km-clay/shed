use chrono::{DateTime, Local};

use crate::{
  builtin::join_raw_args,
  getopt::{Opt, OptSpec},
  sherr,
  state::{read_meta, write_meta},
  util::{error::ShResult, with_status, write_ln_out},
};

pub(super) struct Msg;
impl super::Builtin for Msg {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::flag('s'),
      OptSpec::flag('S'),
      OptSpec::flag("status"),
      OptSpec::flag("system"),
    ]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let mut system = false;
    let mut status = false;

    for opt in args.opts {
      match opt {
        Opt::Short('S') => system = true,
        Opt::Short('s') => status = true,
        Opt::Long(o) if o.as_str() == "system" => system = true,
        Opt::Long(o) if o.as_str() == "status" => status = true,
        _ => {
          return Err(sherr!(ExecFail, "msg: Unexpected flag '{opt}'",));
        }
      }
    }

    if args.argv.is_empty() {
      // argv is empty, maybe they want us to list past messages?
      read_meta(|m| -> ShResult<()> {
        let history = if system {
          m.system_msg_history()
        } else {
          m.status_msg_history()
        };
        for (time, msg) in history {
          let time: DateTime<Local> = (*time).into();
          let formatted = time.format("[%H:%M:%S]").to_string();
          let msg = msg.trim().replace('\n', "\n\t\t"); // aligns multiline messages

          write_ln_out(format!("{formatted}\t{msg}"))?;
        }

        Ok(())
      })?;
    }

    let (msg, _span) = join_raw_args(args.argv);

    if system {
      write_meta(|m| {
        m.post_system_message(msg.clone());
      })
    }
    // defaults to status messages if no flag is provided, but if both are provided we post to both
    if status || !system {
      write_meta(|m| {
        m.post_status_message(msg);
      })
    }

    with_status(0)
  }
}
