use crate::{
  outln,
  state::vars::VarStr,
  util::{self, error::ShResult, ui},
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

    let width = ui::calc_str_width(&input.to_str_lossy());

    outln!("{width}");

    util::with_status(0)
  }
}
