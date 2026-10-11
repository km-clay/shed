use std::{collections::VecDeque, sync::Arc};

use crate::{
  HashSet,
  eval::lex::Span,
  opt,
  procio::{self, ScratchGuard, Sink},
  sherr, shopt,
  state::{
    Shed,
    vars::{VarFlags, VarKind, VarStr},
  },
  util::{self, error::ShResultExt, strops},
};

use super::{BuiltinArgs, ShResult, opt::OptSpec};

pub(super) struct Pack;
impl super::Builtin for Pack {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("exact" | b'E'),
      opt!("array" | b'a', 1),
      opt!("assoc" | b'A', 1),
    ]
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    match (args.opt_value("array"), args.opt_value("assoc")) {
      (Some(array), None) => Self::pack(&args, array, false),
      (None, Some(assoc)) => Self::pack(&args, assoc, true),

      (None, None) => Err(sherr!(
          ParseErr @ args.span(),
          "must specify either --array or --assoc"
      ))
      .with_code(2),

      (Some(_), Some(_)) => {
        let ar_span : Span   = args.opt_span("array").unwrap();
        let as_span : Span   = args.opt_span("assoc").unwrap();
        let ar_slice: VarStr = ar_span.slice();
        let as_slice: VarStr = as_span.slice();

        Err(sherr!(
            ParseErr @ ar_span,
            "cannot specify both {ar_slice} and {as_slice}"
        ))
        .with_code(2)
      }
    }
  }
}

impl Pack {
  fn pack(args: &BuiltinArgs, arr_name: VarStr, associative: bool) -> ShResult<()> {
    let mut arguments = args.arguments();
    let mut array: VecDeque<(Option<VarStr>, VarStr)> = if associative {
      Shed::vars(|v| v.try_get_assoc_items(&arr_name.to_str_lossy()))
        .option_promote(args.opt_span("assoc"))
        .promote_err(args.cmd_span())?
        .into_iter()
        .map(|(k, v)| (Some(k), v))
        .collect()
    } else {
      Shed::vars(|v| v.try_get_arr_elems(&arr_name.to_str_lossy()))
        .option_promote(args.opt_span("array"))
        .promote_err(args.cmd_span())?
        .into_iter()
        .map(|v| (None, v))
        .collect()
    };

    let mut scratch = procio::take_scratch();
    let     exact   = args.has_opt("exact");
    let mut written = 0;
    let     limit   = *shopt!(core.max_read_limit) as usize;
    let mut seen    = HashSet::default();

    while let Some((arg, span)) = arguments.next() {
      let (arg, pad) = match arg.strip_prefix(b"+") {
        Some(rest) => (rest.into(), true),
        None       => (arg.clone(), false),
      };

      let (name, arg): (Option<VarStr>, VarStr) = match strops::split_assignment_raw(&arg) {
        (rhs, Some(lhs)) => {
          let name: VarStr = rhs.into();
          if !seen.insert(name.clone()) {
            return Err(sherr!(ParseErr @ span, "duplicate field name '{name}' in pack"));
          }
          (associative.then_some(name), lhs.into())
        }
        (rhs, None) => {
          if associative && !pad {
            return Err(sherr!(
                ParseErr @ span,
                "associative pack requires field names (e.g. 'field=5')"
            ));
          } else {
            (None, rhs.into())
          }
        }
      };

      let count = arg
        .parse::<usize>()
        .map_err(|v| sherr!(ParseErr @ span, "invalid field length: '{v}'"))?;

      if count > limit {
        let size = strops::human_size(limit as u64);
        return Err(sherr!(
           ParseErr @ span,
           "field length {count} exceeds core.max_read_limit value ({size})",
        ));
      }

      let buf = scratch.buf_mut();
      if buf.len() < written + count {
        buf.resize(written + count, 0);
      }

      if pad {
        let write_end = written + count;
        buf[written..write_end].fill(0);

        written = write_end;
        continue;
      }

      let value = match name {
        Some(name) => {
          let Some(pos) = array.iter().position(|(k, _)| k.as_ref() == Some(&name)) else {
            return Err(sherr!(
                ParseErr @ args.cmd_span(),
                "field name '{name}' not found in {arr_name}"
            ));
          };

          let (_, value) = array.remove(pos).unwrap();
          value
        }
        None => match array.pop_front() {
          Some((_, value)) => value,
          None => {
            return Err(sherr!(
                ParseErr @ args.cmd_span(),
                "not enough fields in {arr_name} to pack"
            ));
          }
        },
      };

      if exact && value.len() != count {
        return Err(sherr!(
            ParseErr @ args.cmd_span(),
            "field length {count} does not match value length ({})",
            value.len()
        ));
      }

      // memcpy time
      let cap      : usize = value.len().min(count);
      let write_end: usize = written + cap;
      let pad_end  : usize = written + count;

      buf[written..write_end].copy_from_slice(&value[..cap]);
      buf[write_end..pad_end].fill(0);

      written += count;
    }

    procio::out_bytes(&scratch[..written]);

    util::with_status(0)
  }
}

pub(super) struct Unpack;
impl super::Builtin for Unpack {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("append" | b'C'),
      opt!("array" | b'a', 1),
      opt!("assoc" | b'A', 1),
    ]
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    match (args.opt_value("array"), args.opt_value("assoc")) {
      (Some(array), None) => Self::unpack(&args, array, false, args.has_opt("append")),
      (None, Some(assoc)) => Self::unpack(&args, assoc, true, args.has_opt("append")),

      (None, None) => Err(sherr!(
          ParseErr @ args.span(),
          "must specify either --array or --assoc"
      ))
      .with_code(2),

      (Some(_), Some(_)) => {
        let ar_span : Span   = args.opt_span("array").unwrap();
        let as_span : Span   = args.opt_span("assoc").unwrap();
        let ar_slice: VarStr = ar_span.slice();
        let as_slice: VarStr = as_span.slice();

        Err(sherr!(
            ParseErr @ ar_span,
            "cannot specify both {ar_slice} and {as_slice}"
        ))
        .with_code(2)
      }
    }
  }
}

impl Unpack {
  fn unpack(args: &BuiltinArgs, arr_name: VarStr, associative: bool, append: bool) -> ShResult<()> {
    let mut arguments              = args.arguments();
    let     reader : Arc<dyn Sink> = procio::stdin_sink()?;
    let mut scratch: ScratchGuard  = procio::take_scratch();
    let     buffer : &mut Vec<u8>  = scratch.buf_mut();
    let     limit  : usize         = *shopt!(core.max_read_limit) as usize;

    let mut fields: Vec<(Option<VarStr>, VarStr, Span)> = vec![];
    let mut consumed: usize = 0;

    while let Some((arg, span)) = arguments.next() {
      let (arg, skip) = match arg.strip_prefix(b"+") {
        Some(skip) => (skip.into(), true),
        None       => (arg.clone(), false),
      };

      let (name, arg): (Option<VarStr>, VarStr) = match strops::split_assignment_raw(&arg) {
        (rhs, Some(lhs)) => {
          let rhs = rhs.into();
          if fields.iter().any(|f| f.0.as_ref() == Some(&rhs)) {
            return Err(sherr!(ParseErr @ span, "duplicate field name '{rhs}' in unpack"));
          }
          (Some(rhs), lhs.into())
        }
        (rhs, None) => {
          if associative && !skip {
            return Err(sherr!(
              ParseErr @ span,
              "associative unpack requires field names (e.g. 'field=5')"
            ));
          } else {
            (None, rhs.into())
          }
        }
      };

      let count = arg
        .parse::<usize>()
        .map_err(|v| sherr!(ParseErr @ span, "invalid field length: '{v}'"))?;

      if count > limit {
        let size = strops::human_size(limit as u64);
        return Err(sherr!(
           ParseErr @ span,
           "field length {count} exceeds core.max_read_limit value ({size})",
        ));
      }

      if count == 0 {
        if !skip {
          fields.push((name, buffer[..count].into(), span));
        }
        continue;
      }

      if buffer.len() < count {
        buffer.resize(count, 0);
      }

      let got = match reader.read_all(&mut buffer[..count]) {
        Ok(0) if consumed == 0 => return util::with_status(1),
        Ok(n) if n < count => {
          return Err(sherr!(
              ParseErr @ span,
              "expected {count} bytes, got {n}"
          ));
        }
        Ok(got) => got,

        Err(e) => return Err(e).promote_err(span),
      };

      consumed += got;

      if !skip {
        fields.push((name, buffer[..count].into(), span));
      }
    }

    Shed::vars_mut(|v| {
      if associative {
        if append {
          let assoc = v
            .get_assoc_mut(&arr_name.to_str_lossy())
            .promote_err(args.cmd_span())?;

          if let Some((Some(name), _, span)) = fields
            .iter()
            .find(|(n, _, _)| assoc.iter().any(|(k, _)| Some(k) == n.as_ref()))
          {
            return Err(sherr!(
                ParseErr @ *span,
                "duplicate field name '{name}' in unpack"
            ));
          }

          for (name, data, span) in fields {
            let Some(name) = name else {
              return Err(sherr!(
                ParseErr @ span,
                "associative unpack requires field names (e.g. 'field=5')"
              ));
            };

            assoc.push((name, data));
          }
          Ok(())
        } else {
          let mut assoc = vec![];
          for (name, data, span) in fields {
            let Some(name) = name else {
              return Err(sherr!(
                ParseErr @ span,
                "associative unpack requires field names (e.g. 'field=5')"
              ));
            };

            assoc.push((name, data));
          }

          v.set_var(
            &arr_name.to_str_lossy(),
            VarKind::assoc_arr(assoc),
            VarFlags::empty(),
          )
        }
      } else {
        let field_iter = fields.into_iter().map(|(_, v, _)| v);
        if append {
          let arr = v
            .get_arr_mut(&arr_name.to_str_lossy())
            .promote_err(args.cmd_span())?;
          arr.extend(field_iter);
          Ok(())
        } else {
          v.set_var(
            &arr_name.to_str_lossy(),
            VarKind::arr(field_iter),
            VarFlags::empty(),
          )
        }
      }
    })
    .promote_err(args.cmd_span())?;

    util::with_status(0)
  }
}

#[cfg(test)]
mod tests {
  use crate::tests::testutil::{TestGuard, test_input};

  fn out_of(cmd: &str) -> String {
    let g = TestGuard::new();
    test_input(cmd).unwrap();
    g.read_output()
  }

  fn fails(cmd: &str) -> bool {
    let g   = TestGuard::new();
    let _   = test_input(cmd);
    let out = g.read_output();
    out.contains("Error") || out.trim_end().ends_with("st=1") || out.trim_end().ends_with("st=2")
  }

  const ELF_SPEC: &str =
    "SPEC=(magic=4 class=1 data=1 ver=1 osabi=1 +8 type=2 machine=2 version=4 entry=8)";
  const ELF_HDR: &str = r"printf '\x7f\x45\x4c\x46\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x03\x00\x3e\x00\x01\x00\x00\x00\xc0\x47\x3a\x00\x00\x00\x00\x00'";

  #[test]
  fn slices_positional_fields_by_width() {
    assert_eq!(
      out_of(r"printf 'abcdefgh' | unpack -a f 2 3 3; printf '%s,' ${f[@]}"),
      "ab,cde,fgh,"
    );
  }

  #[test]
  fn a_plus_prefix_discards_bytes() {
    assert_eq!(
      out_of(r"printf 'abcdefgh' | unpack -a f 2 +3 3; printf '%s,' ${f[@]}"),
      "ab,fgh,"
    );
  }

  #[test]
  fn positional_field_names_are_decorative() {
    assert_eq!(
      out_of(r"printf 'abcdefgh' | unpack -a f x=2 y=3 z=3; printf '%s,' ${f[@]}"),
      "ab,cde,fgh,"
    );
  }

  #[test]
  fn assoc_fields_land_under_their_names() {
    assert_eq!(
      out_of(
        r"printf 'abcdefgh' | unpack -A h x=2 y=3 z=3; printf '%s|%s|%s' ${h[x]} ${h[y]} ${h[z]}"
      ),
      "ab|cde|fgh"
    );
  }

  #[test]
  fn a_skip_creates_no_key() {
    assert_eq!(
      out_of(
        r"printf 'abcdefgh' | unpack -A h x=2 +3 z=3; printf '%s|%s|%s' ${h[x]} ${h[z]} ${#h[@]}"
      ),
      "ab|fgh|2"
    );
  }

  #[test]
  fn a_field_is_assembled_from_several_reads() {
    assert_eq!(
      out_of(r"{ printf 'ab'; printf 'cd'; } | unpack -a f 4; printf '%s' ${f[0]}"),
      "abcd"
    );
  }

  #[test]
  fn records_can_be_read_in_a_loop_until_exhausted() {
    assert_eq!(
      out_of(
        r"printf 'aabbccdd' | { while unpack -a f 2 2; do printf '%s%s,' ${f[0]} ${f[1]}; done; }"
      ),
      "aabb,ccdd,"
    );
  }

  #[test]
  fn a_leading_skip_still_reports_a_truncated_record() {
    assert!(fails(r"printf '12345678' | unpack -a f +8 x=4"));
  }

  #[test]
  fn a_zero_width_field_lands_wherever_it_sits() {
    assert_eq!(
      out_of(r"printf 'abcd' | unpack -a f x=0 y=4; printf '%s;%s' ${#f[@]} ${f[1]}"),
      "2;abcd"
    );
    assert_eq!(
      out_of(r"printf 'abcd' | unpack -a f y=4 x=0; printf '%s;%s' ${#f[@]} ${f[0]}"),
      "2;abcd"
    );
  }

  #[test]
  fn a_truncated_field_leaves_an_array_alone() {
    assert_eq!(
      out_of(r"f=(orig); { printf 'ab' | unpack -a f 4; } 2>/dev/null; printf '%s' ${f[0]}"),
      "orig"
    );
  }

  #[test]
  fn a_truncated_field_leaves_an_assoc_alone() {
    assert_eq!(
      out_of(
        r"declare -A h; h[k]=orig; { printf 'ab' | unpack -A h x=4; } 2>/dev/null; printf '%s' ${h[k]}"
      ),
      "orig"
    );
  }

  #[test]
  fn assoc_unpack_demands_names() {
    assert!(fails(r"printf 'abcdefgh' | unpack -A h 2 3 3"));
  }

  #[test]
  fn unpack_rejects_duplicate_names() {
    assert!(fails(r"printf 'abcdefgh' | unpack -A h x=2 x=3"));
    assert!(fails(r"printf 'abcdefgh' | unpack -a f x=2 x=3"));
  }

  #[test]
  fn unpack_needs_exactly_one_mode() {
    assert!(fails(r"printf 'abcdefgh' | unpack -a f -A h 2"));
    assert!(fails(r"printf 'abcdefgh' | unpack 2 3"));
  }

  #[test]
  fn unpack_rejects_a_malformed_width() {
    assert!(fails(r"printf 'abcdefgh' | unpack -a f +"));
    assert!(fails(r"printf 'abcdefgh' | unpack -a f x="));
  }

  #[test]
  fn unpack_rejects_a_field_past_the_read_limit() {
    assert!(fails(r"printf 'ab' | unpack -a f 2000000000"));
  }

  #[test]
  fn short_input_is_a_failure() {
    assert!(fails(r"printf 'ab' | unpack -a f 4"));
  }

  #[test]
  fn concatenates_positional_fields() {
    assert_eq!(out_of(r"arr=(ab cde fgh); pack -a arr 2 3 3"), "abcdefgh");
  }

  #[test]
  fn pack_field_names_are_decorative() {
    assert_eq!(
      out_of(r"arr=(ab cde fgh); pack -a arr x=2 y=3 z=3"),
      "abcdefgh"
    );
  }

  #[test]
  fn the_spec_drives_assoc_output_order() {
    assert_eq!(
      out_of(r"declare -A h; h[a]=ab; h[b]=cd; pack -A h b=2 a=2"),
      "cdab"
    );
  }

  #[test]
  fn a_plus_prefix_emits_zeros() {
    assert_eq!(
      out_of(r"arr=(ab cd); pack -a arr 2 +4 2 >@v; printf '%s' ${#v}"),
      "8"
    );
  }

  #[test]
  fn a_short_value_is_padded_without_shifting_the_layout() {
    assert_eq!(
      out_of(r"arr=(ab x cd); pack -a arr 2 4 2 >@v; printf '%s' ${#v}"),
      "8"
    );
  }

  #[test]
  fn a_long_value_is_truncated_to_its_width() {
    assert_eq!(out_of(r"arr=(abcdef); pack -a arr 2"), "ab");
  }

  #[test]
  fn only_the_packed_prefix_is_emitted() {
    assert_eq!(
      out_of(r"arr=(ab); pack -a arr 2 >@v; printf '%s' ${#v}"),
      "2"
    );
  }

  #[test]
  fn a_field_larger_than_the_scratch_survives() {
    assert_eq!(
      out_of(
        r#"b=a; while [ ${#b} -lt 300000 ]; do b="$b$b"; done; a=( "${b:0:300000}" ); pack -E -a a 300000 >@out; printf '%s' ${#out}"#
      ),
      "300000"
    );
  }

  #[test]
  fn exact_accepts_a_matching_width() {
    assert_eq!(
      out_of(r"arr=(ab); pack -E -a arr 2 >@v; printf '%s' ${#v}"),
      "2"
    );
  }

  #[test]
  fn exact_rejects_a_width_mismatch() {
    assert!(fails(r"arr=(ab); pack -E -a arr 4"));
  }

  #[test]
  fn pack_rejects_duplicate_names() {
    assert!(fails(r"declare -A h; h[foo]=abcd; pack -A h foo=4 foo=4"));
    assert!(fails(r"arr=(ab cd); pack -a arr x=2 x=2"));
  }

  #[test]
  fn pack_rejects_an_unknown_name() {
    assert!(fails(r"declare -A h; h[a]=ab; pack -A h zz=2"));
  }

  #[test]
  fn pack_rejects_too_few_fields() {
    assert!(fails(r"arr=(ab); pack -a arr 2 2"));
  }

  #[test]
  fn assoc_pack_demands_names() {
    assert!(fails(r"declare -A h; h[a]=ab; pack -A h 2"));
  }

  #[test]
  fn pack_needs_exactly_one_mode() {
    assert!(fails(r"arr=(ab); pack -a arr -A h 2"));
    assert!(fails(r"arr=(ab); pack 2"));
  }

  #[test]
  fn pack_rejects_a_field_past_the_limit() {
    assert!(fails(r"arr=(ab); pack -a arr 2000000000"));
  }

  #[test]
  fn a_shared_spec_round_trips_positionally() {
    assert_eq!(
      out_of(
        r"SPEC=(x=2 y=3); printf 'abcde' >@src; printf '%s' $src | unpack -a f ${SPEC[@]}; pack -E -a f ${SPEC[@]} >@back; [[ $src == $back ]] && printf same"
      ),
      "same"
    );
  }

  #[test]
  fn a_shared_spec_round_trips_through_an_assoc() {
    assert_eq!(
      out_of(
        r"SPEC=(x=2 y=3); printf 'abcde' >@src; printf '%s' $src | unpack -A h ${SPEC[@]}; pack -E -A h ${SPEC[@]} >@back; [[ $src == $back ]] && printf same"
      ),
      "same"
    );
  }

  #[test]
  fn an_elf_header_survives_the_round_trip() {
    let cmd = format!(
      "{ELF_SPEC}; {ELF_HDR} >@src; printf '%s' $src | unpack -A h ${{SPEC[@]}}; \
       pack -E -A h ${{SPEC[@]}} >@back; printf '%s:%s' ${{#back}} \
       \"$( [[ $src == $back ]] && printf same || printf diff )\""
    );
    assert_eq!(out_of(&cmd), "32:same");
  }

  #[test]
  fn an_unpacked_header_field_decodes() {
    let cmd = format!(
      "{ELF_SPEC}; {ELF_HDR} | unpack -A h ${{SPEC[@]}}; printf '%s' ${{h[machine]}} | readint -T u16"
    );
    assert_eq!(out_of(&cmd), "62");
  }
}
