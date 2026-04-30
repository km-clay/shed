use crate::{builtin::{join_raw_args, trap::TrapTarget}, errln, getopt::{Opt, OptSpec}, outln, state::{read_vars, write_logic, write_vars}, util::{error::{ShResult, ShResultExt}, with_status}};

pub(super) struct Defer;
impl super::Builtin for Defer {
  fn opts(&self) -> Vec<crate::getopt::OptSpec> {
    vec![
      OptSpec::flag('c')
    ]
  }
  fn execute(&self, args: super::BuiltinArgs) -> crate::util::error::ShResult<()> {
    if args.argv.is_empty() {
      read_vars(|s| -> ShResult<()> {
        for line in s.cur_scope().display_deferred_cmds().lines() {
          outln!("{line}")?;
        }
        Ok(())
      })?;
      return with_status(0);
    }

    let clear = args.opts.contains(&Opt::Short('c'));

    let command = join_raw_args(args.argv);

    if clear {
      write_vars(|v| v.cur_scope_mut().take_deferred_cmds()); // drops them
    }

    write_vars(|v| v.cur_scope_mut().defer_cmd(command.0));

    with_status(0)
  }
}
