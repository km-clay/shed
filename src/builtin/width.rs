use crate::{
  ShResult, outln,
  state::{terminal::calc_str_width, vars::VarStr},
  util::with_status,
};

pub(super) struct Width;
impl super::Builtin for Width {
  fn execute(&self, mut args: super::BuiltinArgs) -> ShResult<()> {
    let (arg_vec, _) = args.take_argv();
    let input = if arg_vec.is_empty() {
      self.get_input_str(&mut args)
    } else {
      None
    }
    .map_or_else(|| super::join_raw_args(arg_vec).0, VarStr::from);

    let width = calc_str_width(&input);

    outln!("{width}");

    with_status(0)
  }
}
