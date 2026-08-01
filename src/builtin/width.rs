use crate::{
  ShResult, outln,
  state::{terminal::calc_str_width, vars::VarStr},
  util::with_status,
};

pub(super) struct Width;
impl super::Builtin for Width {
  fn execute(&self, mut args: super::BuiltinArgs) -> ShResult<()> {
    let input = self
      .get_input_str(&mut args)
      .map_or_else(|| super::join_raw_args(args.argv).0, VarStr::from);

    let width = calc_str_width(&input);

    outln!("{width}");

    with_status(0)
  }
}
