use bitflags::bitflags;
use bstr::ByteSlice;

use crate::{
  opt, outln,
  util::{self, error::ShResult, ui},
};

use super::opt::OptSpec;

bitflags! {
  struct LenFlags: u8 {
    const CHARS = 0b0000_0001;
    const BYTES = 0b0000_0010;
    const WIDTH = 0b0000_0100;
  }
}

pub(super) struct Len;
impl super::Builtin for Len {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("chars" | b'c'),
      opt!("bytes" | b'b'),
      opt!("width" | b'w'),
    ]
  }
  fn execute(&self, mut args: super::BuiltinArgs) -> ShResult<()> {
    let input = self
      .get_input_str(&mut args)
      .unwrap_or_else(|| super::argv::join_raw_arg_iter(args.arguments()).0);

    let mut flags = LenFlags::empty();

    if args.has_opt("bytes") {
      flags |= LenFlags::BYTES;
    }
    if args.has_opt("width") {
      flags |= LenFlags::WIDTH;
    }
    if args.has_opt("chars") || flags.is_empty() {
      flags |= LenFlags::CHARS;
    }

    let needs_prefix = flags.bits().count_ones() > 1;
    let print = |prefix: &str, n: usize| {
      if needs_prefix {
        outln!("{prefix}: {n}");
      } else {
        outln!("{n}");
      }
    };

    if flags.contains(LenFlags::BYTES) {
      print("bytes", input.len());
    }
    if flags.contains(LenFlags::CHARS) {
      print("chars", input.chars().count());
    }
    if flags.contains(LenFlags::WIDTH) {
      print("width", ui::calc_str_width(&input.to_str_lossy()));
    }

    util::with_status(0)
  }
}
