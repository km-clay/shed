use std::{fmt::Display, str::FromStr};

use crate::libsh::error::{Note, ShErr, ShErrKind, ShResult};

#[derive(Clone, Copy, Debug)]
pub enum ShedBellStyle {
  Audible,
  Visible,
  Disable,
}

impl FromStr for ShedBellStyle {
  type Err = ShErr;
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s.to_ascii_lowercase().as_str() {
      "audible" => Ok(Self::Audible),
      "visible" => Ok(Self::Visible),
      "disable" => Ok(Self::Disable),
      _ => Err(ShErr::simple(
        ShErrKind::SyntaxErr,
        format!("Invalid bell style '{s}'"),
      )),
    }
  }
}

#[derive(Default, Clone, Copy, Debug)]
pub enum ShedEditMode {
  #[default]
  Vi,
  Emacs,
}

impl FromStr for ShedEditMode {
  type Err = ShErr;
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s.to_ascii_lowercase().as_str() {
      "vi" => Ok(Self::Vi),
      "emacs" => Ok(Self::Emacs),
      _ => Err(ShErr::simple(
        ShErrKind::SyntaxErr,
        format!("Invalid edit mode '{s}'"),
      )),
    }
  }
}

impl Display for ShedEditMode {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      ShedEditMode::Vi => write!(f, "vi"),
      ShedEditMode::Emacs => write!(f, "emacs"),
    }
  }
}

#[derive(Clone, Debug)]
pub struct ShOpts {
  pub core: ShOptCore,
  pub prompt: ShOptPrompt,
}

impl Default for ShOpts {
  fn default() -> Self {
    let core = ShOptCore::default();

    let prompt = ShOptPrompt::default();

    Self { core, prompt }
  }
}

impl ShOpts {
  pub fn query(&mut self, query: &str) -> ShResult<Option<String>> {
    if let Some((opt, new_val)) = query.split_once('=') {
      self.set(opt, new_val)?;
      Ok(None)
    } else {
      self.get(query)
    }
  }

  pub fn display_opts(&mut self) -> ShResult<String> {
    let output = [
      format!("core:\n{}", self.query("core")?.unwrap_or_default()),
      format!("prompt:\n{}", self.query("prompt")?.unwrap_or_default()),
    ];

    Ok(output.join("\n"))
  }

  pub fn set(&mut self, opt: &str, val: &str) -> ShResult<()> {
    let mut query = opt.split('.');
    let Some(key) = query.next() else {
      return Err(ShErr::simple(
        ShErrKind::SyntaxErr,
        "shopt: No option given",
      ));
    };

    let remainder = query.collect::<Vec<_>>().join(".");

    match key {
      "core" => self.core.set(&remainder, val)?,
      "prompt" => self.prompt.set(&remainder, val)?,
      _ => {
        return Err(
          ShErr::simple(
            ShErrKind::SyntaxErr,
            "shopt: expected 'core' or 'prompt' in shopt key",
          )
          .with_note(
            Note::new("'shopt' takes arguments separated by periods to denote namespaces")
              .with_sub_notes(vec!["Example: 'shopt core.autocd=true'"]),
          ),
        );
      }
    }
    Ok(())
  }

  pub fn get(&self, query: &str) -> ShResult<Option<String>> {
    // TODO: handle escapes?
    let mut query = query.split('.');
    let Some(key) = query.next() else {
      return Err(ShErr::simple(
        ShErrKind::SyntaxErr,
        "shopt: No option given",
      ));
    };
    let remainder = query.collect::<Vec<_>>().join(".");

    match key {
      "core" => self.core.get(&remainder),
      "prompt" => self.prompt.get(&remainder),
      _ => Err(
        ShErr::simple(
          ShErrKind::SyntaxErr,
          "shopt: Expected 'core' or 'prompt' in shopt key",
        )
        .with_note(
          Note::new("'shopt' takes arguments separated by periods to denote namespaces")
            .with_sub_notes(vec!["Example: 'shopt core.autocd=true'"]),
        ),
      ),
    }
  }
}

#[derive(Clone, Debug)]
pub struct ShOptCore {
  pub dotglob: bool,
  pub autocd: bool,
  pub hist_ignore_dupes: bool,
  pub max_hist: usize,
  pub interactive_comments: bool,
  pub auto_hist: bool,
  pub bell_enabled: bool,
  pub max_recurse_depth: usize,
}

impl ShOptCore {
  pub fn set(&mut self, opt: &str, val: &str) -> ShResult<()> {
    match opt {
      "dotglob" => {
        let Ok(val) = val.parse::<bool>() else {
          return Err(ShErr::simple(
            ShErrKind::SyntaxErr,
            "shopt: expected 'true' or 'false' for dotglob value",
          ));
        };
        self.dotglob = val;
      }
      "autocd" => {
        let Ok(val) = val.parse::<bool>() else {
          return Err(ShErr::simple(
            ShErrKind::SyntaxErr,
            "shopt: expected 'true' or 'false' for autocd value",
          ));
        };
        self.autocd = val;
      }
      "hist_ignore_dupes" => {
        let Ok(val) = val.parse::<bool>() else {
          return Err(ShErr::simple(
            ShErrKind::SyntaxErr,
            "shopt: expected 'true' or 'false' for hist_ignore_dupes value",
          ));
        };
        self.hist_ignore_dupes = val;
      }
      "max_hist" => {
        let Ok(val) = val.parse::<usize>() else {
          return Err(ShErr::simple(
            ShErrKind::SyntaxErr,
            "shopt: expected a positive integer for max_hist value",
          ));
        };
        self.max_hist = val;
      }
      "interactive_comments" => {
        let Ok(val) = val.parse::<bool>() else {
          return Err(ShErr::simple(
            ShErrKind::SyntaxErr,
            "shopt: expected 'true' or 'false' for interactive_comments value",
          ));
        };
        self.interactive_comments = val;
      }
      "auto_hist" => {
        let Ok(val) = val.parse::<bool>() else {
          return Err(ShErr::simple(
            ShErrKind::SyntaxErr,
            "shopt: expected 'true' or 'false' for auto_hist value",
          ));
        };
        self.auto_hist = val;
      }
      "bell_enabled" => {
        let Ok(val) = val.parse::<bool>() else {
          return Err(ShErr::simple(
            ShErrKind::SyntaxErr,
            "shopt: expected 'true' or 'false' for bell_enabled value",
          ));
        };
        self.bell_enabled = val;
      }
      "max_recurse_depth" => {
        let Ok(val) = val.parse::<usize>() else {
          return Err(ShErr::simple(
            ShErrKind::SyntaxErr,
            "shopt: expected a positive integer for max_recurse_depth value",
          ));
        };
        self.max_recurse_depth = val;
      }
      _ => {
        return Err(
          ShErr::simple(
            ShErrKind::SyntaxErr,
            format!("shopt: Unexpected 'core' option '{opt}'"),
          )
          .with_note(Note::new("options can be accessed like 'core.option_name'"))
          .with_note(
            Note::new("'core' contains the following options").with_sub_notes(vec![
              "dotglob",
              "autocd",
              "hist_ignore_dupes",
              "max_hist",
              "interactive_comments",
              "auto_hist",
              "bell_enabled",
              "max_recurse_depth",
            ]),
          ),
        );
      }
    }
    Ok(())
  }
  pub fn get(&self, query: &str) -> ShResult<Option<String>> {
    if query.is_empty() {
      return Ok(Some(format!("{self}")));
    }

    match query {
      "dotglob" => {
        let mut output = String::from("Include hidden files in glob patterns\n");
        output.push_str(&format!("{}", self.dotglob));
        Ok(Some(output))
      }
      "autocd" => {
        let mut output = String::from(
          "Allow navigation to directories by passing the directory as a command directly\n",
        );
        output.push_str(&format!("{}", self.autocd));
        Ok(Some(output))
      }
      "hist_ignore_dupes" => {
        let mut output = String::from("Ignore consecutive duplicate command history entries\n");
        output.push_str(&format!("{}", self.hist_ignore_dupes));
        Ok(Some(output))
      }
      "max_hist" => {
        let mut output = String::from(
          "Maximum number of entries in the command history file (default '.shedhist')\n",
        );
        output.push_str(&format!("{}", self.max_hist));
        Ok(Some(output))
      }
      "interactive_comments" => {
        let mut output = String::from("Whether or not to allow comments in interactive mode\n");
        output.push_str(&format!("{}", self.interactive_comments));
        Ok(Some(output))
      }
      "auto_hist" => {
        let mut output = String::from(
          "Whether or not to automatically save commands to the command history file\n",
        );
        output.push_str(&format!("{}", self.auto_hist));
        Ok(Some(output))
      }
      "bell_enabled" => {
        let mut output = String::from("Whether or not to allow shed to trigger the terminal bell");
        output.push_str(&format!("{}", self.bell_enabled));
        Ok(Some(output))
      }
      "max_recurse_depth" => {
        let mut output = String::from("Maximum limit of recursive shell function calls\n");
        output.push_str(&format!("{}", self.max_recurse_depth));
        Ok(Some(output))
      }
      _ => Err(
        ShErr::simple(
          ShErrKind::SyntaxErr,
          format!("shopt: Unexpected 'core' option '{query}'"),
        )
        .with_note(Note::new("options can be accessed like 'core.option_name'"))
        .with_note(
          Note::new("'core' contains the following options").with_sub_notes(vec![
            "dotglob",
            "autocd",
            "hist_ignore_dupes",
            "max_hist",
            "interactive_comments",
            "auto_hist",
            "bell_enabled",
            "max_recurse_depth",
          ]),
        ),
      ),
    }
  }
}

impl Display for ShOptCore {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let mut output = vec![];
    output.push(format!("dotglob = {}", self.dotglob));
    output.push(format!("autocd = {}", self.autocd));
    output.push(format!("hist_ignore_dupes = {}", self.hist_ignore_dupes));
    output.push(format!("max_hist = {}", self.max_hist));
    output.push(format!(
      "interactive_comments = {}",
      self.interactive_comments
    ));
    output.push(format!("auto_hist = {}", self.auto_hist));
    output.push(format!("bell_enabled = {}", self.bell_enabled));
    output.push(format!("max_recurse_depth = {}", self.max_recurse_depth));

    let final_output = output.join("\n");

    writeln!(f, "{final_output}")
  }
}

impl Default for ShOptCore {
  fn default() -> Self {
    ShOptCore {
      dotglob: false,
      autocd: false,
      hist_ignore_dupes: true,
      max_hist: 1000,
      interactive_comments: true,
      auto_hist: true,
      bell_enabled: true,
      max_recurse_depth: 1000,
    }
  }
}

#[derive(Clone, Debug)]
pub struct ShOptPrompt {
  pub trunc_prompt_path: usize,
  pub edit_mode: ShedEditMode,
  pub comp_limit: usize,
  pub highlight: bool,
  pub auto_indent: bool,
  pub linebreak_on_incomplete: bool,
}

impl ShOptPrompt {
  pub fn set(&mut self, opt: &str, val: &str) -> ShResult<()> {
    match opt {
      "trunc_prompt_path" => {
        let Ok(val) = val.parse::<usize>() else {
          return Err(ShErr::simple(
            ShErrKind::SyntaxErr,
            "shopt: expected a positive integer for trunc_prompt_path value",
          ));
        };
        self.trunc_prompt_path = val;
      }
      "edit_mode" => {
        let Ok(val) = val.parse::<ShedEditMode>() else {
          return Err(ShErr::simple(
            ShErrKind::SyntaxErr,
            "shopt: expected 'vi' or 'emacs' for edit_mode value",
          ));
        };
        self.edit_mode = val;
      }
      "comp_limit" => {
        let Ok(val) = val.parse::<usize>() else {
          return Err(ShErr::simple(
            ShErrKind::SyntaxErr,
            "shopt: expected a positive integer for comp_limit value",
          ));
        };
        self.comp_limit = val;
      }
      "highlight" => {
        let Ok(val) = val.parse::<bool>() else {
          return Err(ShErr::simple(
            ShErrKind::SyntaxErr,
            "shopt: expected 'true' or 'false' for highlight value",
          ));
        };
        self.highlight = val;
      }
      "auto_indent" => {
        let Ok(val) = val.parse::<bool>() else {
          return Err(ShErr::simple(
            ShErrKind::SyntaxErr,
            "shopt: expected 'true' or 'false' for auto_indent value",
          ));
        };
        self.auto_indent = val;
      }
      "linebreak_on_incomplete" => {
        let Ok(val) = val.parse::<bool>() else {
          return Err(ShErr::simple(
            ShErrKind::SyntaxErr,
            "shopt: expected 'true' or 'false' for linebreak_on_incomplete value",
          ));
        };
        self.linebreak_on_incomplete = val;
      }
      "custom" => {
        todo!()
      }
      _ => {
        return Err(
          ShErr::simple(
            ShErrKind::SyntaxErr,
            format!("shopt: Unexpected 'prompt' option '{opt}'"),
          )
          .with_note(Note::new(
            "options can be accessed like 'prompt.option_name'",
          ))
          .with_note(
            Note::new("'prompt' contains the following options").with_sub_notes(vec![
              "trunc_prompt_path",
              "edit_mode",
              "comp_limit",
              "highlight",
              "auto_indent",
              "linebreak_on_incomplete",
              "custom",
            ]),
          ),
        );
      }
    }
    Ok(())
  }
  pub fn get(&self, query: &str) -> ShResult<Option<String>> {
    if query.is_empty() {
      return Ok(Some(format!("{self}")));
    }

    match query {
      "trunc_prompt_path" => {
        let mut output = String::from(
          "Maximum number of path segments used in the '\\W' prompt escape sequence\n",
        );
        output.push_str(&format!("{}", self.trunc_prompt_path));
        Ok(Some(output))
      }
      "edit_mode" => {
        let mut output =
          String::from("The style of editor shortcuts used in the line-editing of the prompt\n");
        output.push_str(&format!("{}", self.edit_mode));
        Ok(Some(output))
      }
      "comp_limit" => {
        let mut output =
          String::from("Maximum number of completion candidates displayed upon pressing tab\n");
        output.push_str(&format!("{}", self.comp_limit));
        Ok(Some(output))
      }
      "highlight" => {
        let mut output =
          String::from("Whether to enable or disable syntax highlighting on the prompt\n");
        output.push_str(&format!("{}", self.highlight));
        Ok(Some(output))
      }
      "auto_indent" => {
        let mut output =
          String::from("Whether to automatically indent new lines in multiline commands\n");
        output.push_str(&format!("{}", self.auto_indent));
        Ok(Some(output))
      }
      "linebreak_on_incomplete" => {
        let mut output =
          String::from("Whether to automatically insert a newline when the input is incomplete\n");
        output.push_str(&format!("{}", self.linebreak_on_incomplete));
        Ok(Some(output))
      }
      _ => Err(
        ShErr::simple(
          ShErrKind::SyntaxErr,
          format!("shopt: Unexpected 'prompt' option '{query}'"),
        )
        .with_note(Note::new(
          "options can be accessed like 'prompt.option_name'",
        ))
        .with_note(
          Note::new("'prompt' contains the following options").with_sub_notes(vec![
            "trunc_prompt_path",
            "edit_mode",
            "comp_limit",
            "highlight",
            "auto_indent",
            "linebreak_on_incomplete",
          ]),
        ),
      ),
    }
  }
}

impl Display for ShOptPrompt {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let mut output = vec![];

    output.push(format!("trunc_prompt_path = {}", self.trunc_prompt_path));
    output.push(format!("edit_mode = {}", self.edit_mode));
    output.push(format!("comp_limit = {}", self.comp_limit));
    output.push(format!("highlight = {}", self.highlight));
    output.push(format!("auto_indent = {}", self.auto_indent));
    output.push(format!(
      "linebreak_on_incomplete = {}",
      self.linebreak_on_incomplete
    ));

    let final_output = output.join("\n");

    writeln!(f, "{final_output}")
  }
}

impl Default for ShOptPrompt {
  fn default() -> Self {
    ShOptPrompt {
      trunc_prompt_path: 4,
      edit_mode: ShedEditMode::Vi,
      comp_limit: 100,
      highlight: true,
      auto_indent: true,
      linebreak_on_incomplete: true,
    }
  }
}
