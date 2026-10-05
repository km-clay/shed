use crate::{opt, sub_command, util};

use super::{Builtin, BuiltinArgs, BuiltinRouter, ShResult, SubCommand, argv, opt as mod_opt};

mod trim;

pub(super) struct Str;
impl BuiltinRouter for Str {
  fn name(&self) -> &'static str {
    "str"
  }
  fn sub_commands(&self) -> &'static [SubCommand] {
    const SUB_COMMANDS: &[SubCommand] = &[
      sub_command!(
        &trim::Trim,
        "trim",
        "<string>",
        "trim whitespace or charsets from the string - accepts stdin or arguments"
      ),
      sub_command!(
        &clip::Clip,
        "clip",
        "<width> [string]",
        "clip a string to a given width, optionally with a marker and justification - accepts stdin or arguments"
      ),
    ];
    SUB_COMMANDS
  }
}

mod clip {
  use crate::{
    out, set_var, sherr,
    state::vars::VarStr,
    util::ui::{self, Justify},
  };

  use super::{Builtin, BuiltinArgs, ShResult, argv, mod_opt::OptSpec, opt, util};

  struct ClipSpec {
    width: usize,
    marker: VarStr,
    justify: Justify,
    var: Option<VarStr>,
  }

  impl ClipSpec {
    fn parse_args(args: &mut BuiltinArgs) -> ShResult<Self> {
      let mut arguments = args
        .arguments()
        .map(|(a, s)| (a.clone(), s))
        .collect::<Vec<_>>()
        .into_iter();

      let width = arguments
        .next()
        .map(|(a, s)| {
          a.parse::<usize>()
            .ok_or_else(|| sherr!(ParseErr @ s, "invalid width '{a}'"))
        })
        .transpose()?;

      let Some(width) = width else {
        return Err(sherr!(ExecFail @ args.cmd_span(), "missing required width argument"));
      };

      let marker = args.opt_value("marker").unwrap_or_else(|| "…".into());
      let justify = args
        .opt_value("justify")
        .map(|j| {
          j.parse::<Justify>().ok_or_else(
            || sherr!(ParseErr @ args.opt_span("justify").unwrap(), "invalid justify value '{j}'"),
          )
        })
        .transpose()?
        .unwrap_or(Justify::Left);

      let var = args.opt_value("var");

      Ok(Self {
        width,
        marker,
        justify,
        var,
      })
    }
  }

  pub(super) struct Clip;
  #[rustfmt::skip]
  impl Builtin for Clip {
    fn opts(&self) -> Vec<OptSpec> {
      vec![
        opt!("justify" | b'j', 1),
        opt!("marker"  | b'm', 1),
        opt!("var"     | b'v', 1),
      ]
    }
    fn execute(&self, mut args: BuiltinArgs) -> ShResult<()> {
      let string = self
        .get_input_with(&mut args, |a| a.arguments().count() <= 1)
        .map_or_else(|| argv::join_raw_arg_iter(args.arguments().skip(1)).0, VarStr::from);

      let ClipSpec { width, marker, justify, var } = ClipSpec::parse_args(&mut args)?;

      let out = ui::truncate_with_marker(
        &string.to_str_lossy(),
        width,
        &marker.to_str_lossy(),
        justify,
      );

      match var {
        Some(name) => {
          set_var!(&name.to_str_lossy(), string(VarStr::from(out)))?;
        }
        None => out!("{out}"),
      }

      util::with_status(0)
    }
  }
}
