use crate::{
  HashMap,
  eval::lex::TkFlags,
  expand::Expander,
  state::{Shed, vars::VarStr},
  try_var,
  util::{
    self, guards,
    strops::{ByteCursor, SliceCursor},
  },
  varstr,
};

use nix::libc;
use unicode_segmentation::UnicodeSegmentation;

use super::{
  ShResult,
  eval::lex::{LexFlags, LexStream},
  match_loop, sherr,
  vars::{ArrIndex, Var, VarFlags, VarKind},
};

/// Parse `arr[idx]` into (name, `raw_index_expr`). Pure parsing, no expansion.
pub fn parse_arr_bracket(var_name: &[u8]) -> Option<(VarStr, VarStr)> {
  if !var_name.contains(&b'[') {
    return None;
  }
  let mut cur = SliceCursor::new(var_name);
  let mut name = util::scratch_buf();
  let mut idx_raw = util::scratch_buf();
  let mut bracket_depth = 0;

  match_loop!(cur.next_byte() => ch, {
    b'\\' => {
      cur.next_byte();
    }
    b'[' => {
      bracket_depth += 1;
      if bracket_depth > 1 {
        idx_raw.push(ch);
      }
    }
    b']' => {
      if bracket_depth > 0 {
        bracket_depth -= 1;
        if bracket_depth == 0 {
          if idx_raw.is_empty() {
            return None;
          }
          break;
        }
      }
      idx_raw.push(ch);
    }
    _ if bracket_depth > 0 => idx_raw.push(ch),
    _ => name.push(ch),
  });

  if name.is_empty() || idx_raw.is_empty() {
    None
  } else {
    Some((name.into(), idx_raw.into()))
  }
}

/// Expand the raw index expression and parse it into an `ArrIndex`.
pub fn expand_arr_index(idx_raw: &[u8], allow_side_effects: bool) -> ShResult<ArrIndex> {
  let expanded = LexStream::new(idx_raw, LexFlags::empty())
    .map(|tk| tk.and_then(|tk| tk.expand()).map(|tk| tk.get_words()))
    .try_fold(vec![], |mut acc, wrds| {
      match wrds {
        Ok(wrds) => acc.extend_from_slice(&wrds),
        Err(e) => return Err(e),
      }
      Ok(acc)
    })?
    .into_iter()
    .next()
    .ok_or_else(|| sherr!(ParseErr, "Empty array index"))?;

  ArrIndex::parse(&expanded.to_str_lossy(), allow_side_effects)
    .map_err(|_| sherr!(ParseErr, "Invalid array index: {}", expanded,))
}

/*
 * the functions below are some of the most important in the entire codebase
 * it's very important to understand these if you want to get anything done around here.
 *
 * Each one accesses a different part of the shared state (the "Shed" struct),
 * and they take a closure that operates on that part of the state.
 *
 *
 * With these, we can access shell state anywhere without threading a state object through every function.
 * However, we must be mindful of what the callstack looks like when we call them, to avoid re-entrancy issues.
 */

/// Query the `SQLite` database.
///
/// Takes a function that returns `ShResult<T>`, and returns `ShResult<Option<T>>`.
/// The option is necessary because `Shed.db_conn` can be None. This happens
/// in non-interactive cases, or cases where the database cannot be opened.
///
/// The returns look basically like this:
/// * Ok(None) means "there's no database connection"
/// * Err(e) is your function's `ShErr`
/// * Ok(Some(T)) means the connection exists and your function succeeded.
pub fn with_vars<F, H, V, T>(vars: H, f: F) -> T
where
  F: FnOnce() -> T,
  H: IntoIterator<Item = (VarStr, V)>,
  V: Into<Var>,
{
  let vars: HashMap<VarStr, V> = vars.into_iter().collect();
  let restores: Vec<(VarStr, Option<(VarKind, VarFlags)>)> = vars
    .keys()
    .map(|k| {
      let prev = Shed::vars(|v| {
        v.try_get_var_meta(&k.to_str_lossy())
          .map(|var| (var.kind().clone(), var.flags()))
      });
      (k.clone(), prev)
    })
    .collect();

  for (name, val) in vars {
    let val = val.into();
    Shed::vars_mut(|v| {
      v.set_var(&name.to_str_lossy(), val.kind().clone(), val.flags())
        .unwrap();
    });
  }

  let _guard = guards::guard(restores, |restores| {
    Shed::vars_mut(|v| {
      for (name, prev) in restores {
        match prev {
          Some((kind, flags)) => {
            v.set_var(&name.to_str_lossy(), kind, flags).ok();
          }
          None => {
            v.unset_var(&name.to_str_lossy()).ok();
          }
        }
      }
    });
  });
  f()
}

pub fn get_comp_wordbreaks() -> VarStr {
  try_var!("COMP_WORDBREAKS").unwrap_or_else(|| "\"'><;|=&(:".into())
}

/// Get the first char of IFS
///
/// Used mainly for joining strings
pub fn get_separator() -> VarStr {
  let separators = get_separators();
  separators
    .to_str_lossy()
    .graphemes(true)
    .next()
    .unwrap_or_default()
    .into()
}

/// Get the entire IFS variable
///
/// Used mainly for splitting strings
pub fn get_separators() -> VarStr {
  try_var!("IFS").unwrap_or_else(|| " \t\n".into())
}

pub fn get_ps4() -> VarStr {
  try_var!("PS4")
    .and_then(|ps4| {
      Expander::from_raw(&ps4, TkFlags::empty())
        .expand_no_split()
        .ok()
    })
    .unwrap_or_else(|| varstr!("+ "))
}

pub fn get_time_fmt() -> VarStr {
  try_var!("TIMEFMT").unwrap_or_else(|| "\nreal\t%*E\nuser\t%*U\nsys\t%*S".into())
}

pub fn set_ver_info() -> ShResult<()> {
  let version = env!("CARGO_PKG_VERSION");
  let mut semver = version.split('.');
  let major = semver.next().unwrap_or("0");
  let minor = semver.next().unwrap_or("0");
  let patch = semver.next().unwrap_or("0");
  let arch = std::env::consts::ARCH;
  let os = std::env::consts::OS;
  let ver_info = vec![
    ("major".into(), major.into()),
    ("minor".into(), minor.into()),
    ("patch".into(), patch.into()),
    ("arch".into(), arch.into()),
    ("os".into(), os.into()),
  ];

  Shed::vars_mut(|v| {
    v.set_var(
      "SHED_VERSION",
      VarKind::Str(version.into()),
      VarFlags::EXPORT,
    )?;
    v.set_var(
      "SHED_VER_INFO",
      VarKind::AssocArr(ver_info),
      VarFlags::empty(),
    )
  })?;

  Ok(())
}

pub fn set_sh_lvl() -> ShResult<()> {
  // Increment SHLVL, or set to 1 if not present or invalid.
  // This var represents how many nested shell instances we're in
  if let Some(var) = try_var!("SHLVL")
    && let Ok(lvl) = var.to_str_lossy().parse::<u32>()
  {
    Shed::vars_mut(|v| {
      v.set_var(
        "SHLVL",
        VarKind::string((lvl + 1).to_string().into()),
        VarFlags::EXPORT,
      )
    })?;
  } else {
    Shed::vars_mut(|v| v.set_var("SHLVL", VarKind::Str("1".into()), VarFlags::EXPORT))?;
  }

  Ok(())
}

/// The process-wide history database connection.
#[cfg(target_os = "android")]
pub fn get_default_path() -> Option<String> {
  // Android does not have conf_str or _CS_PATH
  // So we return None here.
  None
}

#[cfg(not(target_os = "android"))]
pub fn get_default_path() -> Option<String> {
  unsafe {
    let needed = libc::confstr(libc::_CS_PATH, std::ptr::null_mut(), 0);
    if needed == 0 {
      return None;
    }
    let mut buf = vec![0u8; needed];
    let written = libc::confstr(
      libc::_CS_PATH,
      buf.as_mut_ptr().cast::<std::ffi::c_char>(),
      needed,
    );
    if !(1..=needed).contains(&written) {
      return None;
    }

    // check for null byte
    if buf.ends_with(b"\0") {
      buf.truncate(written - 1);
    }
    String::from_utf8(buf).ok()
  }
}

pub fn get_exec_wrappers() -> Vec<VarStr> {
  let mut wrappers = vec![
    "sudo".into(),
    "doas".into(),
    "pkexec".into(),
    "run0".into(),
    "please".into(),
    "gosu".into(),
    "strace".into(),
    "ltrace".into(),
    "ktrace".into(),
    "valgrind".into(),
    "heaptrack".into(),
    "nohup".into(),
    "nice".into(),
    "ionice".into(),
    "chrt".into(),
    "setsid".into(),
    "setpriv".into(),
    "prlimit".into(),
    "unshare".into(),
    "bwrap".into(),
    "firejail".into(),
    "systemd-run".into(),
    "proot".into(),
    "watch".into(),
    "chronic".into(),
    "parallel".into(),
    "stdbuf".into(),
    "hyperfine".into(),
    "command".into(),
    "builtin".into(),
    "env".into(),
    "exec".into(),
    "defer".into(),
  ];

  // lets users define their own exec wrappers for the highlighter if they want
  // for instance, my personal config has a wrapper function called 'invoke'
  let user_wrappers = Shed::vars(|v| v.get_arr_elems("SHED_EXEC_WRAPPERS"));
  wrappers.extend(user_wrappers);

  wrappers
}

#[cfg(test)]
mod set_ver_info_tests {
  //! `set_ver_info` populates two shell vars from compile-time
  //! constants: `SHED_VERSION` (the Cargo.toml version string) and
  //! `SHED_VER_INFO` (an `AssocArr` with major/minor/patch/arch/os keys).
  //! The tests pin the structure rather than specific values, so they
  //! don't churn each version bump.

  use super::*;
  use crate::tests::testutil::TestGuard;
  use crate::var;

  #[test]
  fn sets_shed_version_to_cargo_pkg_version() {
    let _g = TestGuard::new();
    set_ver_info().unwrap();
    assert_eq!(var!("SHED_VERSION"), env!("CARGO_PKG_VERSION"));
  }

  #[test]
  fn ver_info_is_assoc_array_with_five_keys() {
    let _g = TestGuard::new();
    set_ver_info().unwrap();
    let kind = Shed::vars(|v| v.try_get_var_kind("SHED_VER_INFO"));
    match kind {
      Some(VarKind::AssocArr(items)) => {
        let keys: crate::HashSet<&VarStr> = items.iter().map(|(k, _)| k).collect();
        assert_eq!(keys.len(), 5, "got: {keys:?}");
        for expected in ["major", "minor", "patch", "arch", "os"] {
          assert!(
            keys.contains(&VarStr::from(expected)),
            "missing key {expected}, got: {keys:?}"
          );
        }
      }
      other => panic!("expected AssocArr, got {other:?}"),
    }
  }

  #[test]
  fn ver_info_arch_and_os_match_compile_time_consts() {
    let _g = TestGuard::new();
    set_ver_info().unwrap();
    let items = match Shed::vars(|v| v.try_get_var_kind("SHED_VER_INFO")) {
      Some(VarKind::AssocArr(items)) => items,
      other => panic!("expected AssocArr, got {other:?}"),
    };
    let get = |k: &str| {
      items
        .iter()
        .find(|(key, _)| key == k)
        .map(|(_, v)| v.clone())
        .unwrap_or_default()
    };
    assert_eq!(get("arch"), std::env::consts::ARCH);
    assert_eq!(get("os"), std::env::consts::OS);
  }

  #[test]
  fn ver_info_semver_components_match_cargo_pkg_version() {
    let _g = TestGuard::new();
    set_ver_info().unwrap();
    let items = match Shed::vars(|v| v.try_get_var_kind("SHED_VER_INFO")) {
      Some(VarKind::AssocArr(items)) => items,
      other => panic!("expected AssocArr, got {other:?}"),
    };
    let get = |k: &str| {
      items
        .iter()
        .find(|(key, _)| key == k)
        .map(|(_, v)| v.clone())
        .unwrap_or_default()
    };
    let expected: Vec<&str> = env!("CARGO_PKG_VERSION").split('.').collect();
    assert_eq!(get("major"), expected[0]);
    assert_eq!(get("minor"), expected[1]);
    assert_eq!(get("patch"), expected[2]);
  }
}
