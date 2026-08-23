use std::path::PathBuf;

use crate::{
  COMPLETIONS, FUNCTIONS, HELP, ShResult,
  eval::execute::exec_nonint,
  state::{util::source_file, vars::VarStr},
  util,
};

use super::HashMap;

pub(crate) trait Autoloader {
  fn files(&self) -> impl Iterator<Item = &'static (&'static str, &'static [u8])>;
  fn custom_dir(&self) -> &'static str;
  fn get(&self, name: &str) -> Option<&'static [u8]> {
    self.files().find_map(|(n, c)| (name == *n).then_some(*c))
  }
  fn names(&self) -> impl Iterator<Item = &'static str> {
    self.files().map(|(n, _)| *n)
  }
  fn collect_all(&self) -> HashMap<String, AutoloadSrc> {
    let mut out: HashMap<String, AutoloadSrc> = self
      .files()
      .filter_map(|(name, content)| {
        let stem = PathBuf::from(name)
          .file_stem()
          .and_then(|n| n.to_str())?
          .to_string();
        (!stem.is_empty()).then(|| {
          (
            stem,
            AutoloadSrc::Embedded {
              name: VarStr::from(name.as_bytes()),
              body: VarStr::from(*content),
            },
          )
        })
      })
      .collect();

    let path_var = std::env::var(self.custom_dir()).unwrap_or_default();
    for entry in util::path_list_entries(&path_var) {
      let path = entry.path();
      if path.is_dir() {
        continue;
      }
      if let Some(name) = path.file_stem().and_then(|n| n.to_str()) {
        out.insert(name.to_string(), AutoloadSrc::Path(path));
      }
    }

    out
  }
}

pub(crate) struct CompLoader;
impl Autoloader for CompLoader {
  fn custom_dir(&self) -> &'static str {
    "SHED_COMPLETE_PATH"
  }
  fn files(&self) -> impl Iterator<Item = &'static (&'static str, &'static [u8])> {
    COMPLETIONS.iter()
  }
}

pub(crate) struct FuncLoader;
impl Autoloader for FuncLoader {
  fn custom_dir(&self) -> &'static str {
    "SHED_FUNC_PATH"
  }
  fn files(&self) -> impl Iterator<Item = &'static (&'static str, &'static [u8])> {
    FUNCTIONS.iter()
  }
}

pub(crate) struct HelpLoader;
impl Autoloader for HelpLoader {
  fn custom_dir(&self) -> &'static str {
    "SHED_HPATH"
  }
  fn files(&self) -> impl Iterator<Item = &'static (&'static str, &'static [u8])> {
    HELP.iter()
  }
}

#[derive(Clone, Debug)]
pub enum AutoloadSrc {
  Path(PathBuf),
  Embedded { name: VarStr, body: VarStr },
}

impl AutoloadSrc {
  pub fn source(&self) -> ShResult<()> {
    match self {
      Self::Path(p) => source_file(p.clone()),
      Self::Embedded { name, body } => exec_nonint(
        body.clone(),
        Some(format!("<include>/{}", name.to_str_lossy()).into()),
      ),
    }
  }
}
