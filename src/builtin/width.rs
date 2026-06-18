use crate::{ShResult, outln, procio, state::terminal::calc_str_width, util::with_status};

pub(super) struct Width;
impl super::Builtin for Width {
  fn execute(&self, mut args: super::BuiltinArgs) -> ShResult<()> {
    let input = if args.argv.is_empty() || args.has_stdin() {
      if args.has_stdin() {
        args.take_stdin().unwrap()
      } else {
        procio::read_input()?
      }
    } else {
      super::join_raw_args(args.argv).0
    };

    let width = calc_str_width(&input);

    outln!("{width}");

    with_status(0)
  }
}
