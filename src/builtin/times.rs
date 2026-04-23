use nix::sys::resource::{UsageWho, getrusage};

use crate::{
  outln,
  state::CmdTimer,
  util::{error::ShResult, with_status},
};

pub(super) struct Times;
impl super::Builtin for Times {
  fn execute(&self, _args: super::BuiltinArgs) -> ShResult<()> {
    let self_usage = getrusage(UsageWho::RUSAGE_SELF)?;
    let child_usage = getrusage(UsageWho::RUSAGE_CHILDREN)?;

    let self_user = CmdTimer::tv_to_ms(self_usage.user_time());
    let self_sys = CmdTimer::tv_to_ms(self_usage.system_time());

    let child_user = CmdTimer::tv_to_ms(child_usage.user_time());
    let child_sys = CmdTimer::tv_to_ms(child_usage.system_time());

    let self_user_minutes = self_user / 60_000;
    let self_user_seconds = self_user / 1000;
    let self_user_milliseconds = self_user % 1000;

    let self_sys_minutes = self_sys / 60_000;
    let self_sys_seconds = self_sys / 1000;
    let self_sys_milliseconds = self_sys % 1000;

    let child_user_minutes = child_user / 60_000;
    let child_user_seconds = child_user / 1000;
    let child_user_milliseconds = child_user % 1000;

    let child_sys_minutes = child_sys / 60_000;
    let child_sys_seconds = child_sys / 1000;
    let child_sys_milliseconds = child_sys % 1000;

    outln!(
      "{self_user_minutes}m{self_user_seconds}.{self_user_milliseconds:03} {self_sys_minutes}m{self_sys_seconds}.{self_sys_milliseconds:03}\n"
    )?;
    outln!(
      "{child_user_minutes}m{child_user_seconds}.{child_user_milliseconds:03} {child_sys_minutes}m{child_sys_seconds}.{child_sys_milliseconds:03}\n"
    )?;

    with_status(0)
  }
}
