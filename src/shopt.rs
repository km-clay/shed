use std::{fmt::Display, str::FromStr};

use nix::{libc::STDERR_FILENO, unistd::write};

use crate::sherr;
use crate::{
  libsh::error::{ShErr, ShResult},
  parse::lex::Span,
  procio::borrow_fd,
  state::{read_shopts, read_vars},
};

pub fn xtrace_print(argv: &[(String, Span)]) {
  if read_shopts(|o| o.set.xtrace) {
    let words = argv
      .iter()
      .map(|(s, _)| s.to_string())
      .collect::<Vec<String>>();

    let stderr = borrow_fd(STDERR_FILENO);
    let depth = read_vars(|v| v.depth());
    let prefix = "+".repeat((depth as usize) + 1);
    let output = format!("{prefix} {}", words.join(" "));
    log::debug!("xtrace: {output:?}");
    write(stderr, output.trim().as_bytes()).ok();
    write(stderr, b"\n").ok();
  }
}

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
      _ => Err(sherr!(SyntaxErr, "Invalid bell style '{s}'",)),
    }
  }
}

/// Generates a shopt group struct with `set`, `get`, `Display`, and `Default` impls.
///
/// Doc comments on each field become the description shown by `shopt get`.
/// Every field type must implement `FromStr + Display`.
///
/// Optional per-field validation: `#[validate(|val| expr)]` runs after parsing
/// and must return `Result<(), String>` where the error string is the message.
macro_rules! shopt_group {
  (
    $(#[$struct_meta:meta])*
    pub struct $name:ident ($group_name:literal) {
      $(
        $(#[doc = $desc:literal])*
        $(#[validate($validator:expr)])?
        $field:ident : $ty:ty = $default:expr
      ),* $(,)?
    }
  ) => {
    $(#[$struct_meta])*
    pub struct $name {
      $(pub $field: $ty,)*
    }

    impl Default for $name {
      fn default() -> Self {
        Self {
          $($field: $default,)*
        }
      }
    }

    impl $name {
      pub fn set(&mut self, opt: &str, val: &str) -> ShResult<()> {
        match opt {
          $(
            stringify!($field) => {
              let parsed = val.parse::<$ty>().map_err(|_| {
                sherr!(
                  SyntaxErr,
                  "shopt: invalid value '{}' for {}.{}", val, $group_name, opt,
                )
              })?;
              $(
                let validate: fn(&$ty) -> Result<(), String> = $validator;
                validate(&parsed).map_err(|msg| {
                  sherr!(SyntaxErr, "shopt: {msg}")
                })?;
              )?
              self.$field = parsed;
            }
          )*
          _ => {
            return Err(sherr!(
              SyntaxErr,
              "shopt: unexpected '{}' option '{opt}'", $group_name,
            ));
          }
        }
        Ok(())
      }

      pub fn get(&self, query: &str) -> ShResult<Option<String>> {
        if query.is_empty() {
          return Ok(Some(format!("{self}")));
        }
        match query {
          $(
            stringify!($field) => {
              let desc = concat!($($desc, "\n",)*);
              let output = format!("{}{}", desc, self.$field);
              Ok(Some(output))
            }
          )*
          _ => Err(sherr!(
            SyntaxErr,
            "shopt: unexpected '{}' option '{query}'", $group_name,
          )),
        }
      }
    }

    impl Display for $name {
      fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let output = [
          $(format!("{}.{}={}", $group_name, stringify!($field),
            $crate::expand::as_var_val_display(&self.$field.to_string())),)*
        ];
        writeln!(f, "{}", output.join("\n"))
      }
    }
  };
}

#[derive(Clone, Debug)]
pub struct ShOpts {
  pub core: ShOptCore,
  pub line: ShOptLine,
  pub set: ShOptSet,
  pub prompt: ShOptPrompt,
	pub highlight: ShOptHighlight,
}

impl Default for ShOpts {
  fn default() -> Self {
    let core = ShOptCore::default();
    let line = ShOptLine::default();
    let set = ShOptSet::default();
    let prompt = ShOptPrompt::default();
		let highlight = ShOptHighlight::default();

    Self {
      core,
      line,
      set,
      prompt,
			highlight,
    }
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
      self.query("core")?.unwrap_or_default().to_string(),
      self.query("line")?.unwrap_or_default().to_string(),
      self.query("set")?.unwrap_or_default().to_string(),
      self.query("prompt")?.unwrap_or_default().to_string(),
			self.query("highlight")?.unwrap_or_default().to_string(),
    ];

    Ok(output.join("\n"))
  }

  pub fn set(&mut self, opt: &str, val: &str) -> ShResult<()> {
    let mut query = opt.split('.');
    let Some(key) = query.next() else {
      return Err(sherr!(SyntaxErr, "shopt: No option given",));
    };

    let remainder = query.collect::<Vec<_>>().join(".");

    match key {
      "core" => self.core.set(&remainder, val)?,
      "line" => self.line.set(&remainder, val)?,
      "set" => self.set.set(&remainder, val)?,
      "prompt" => self.prompt.set(&remainder, val)?,
			"highlight" => self.highlight.set(&remainder, val)?,
      _ => {
        return Err(sherr!(
          SyntaxErr,
					"shopt: Unknown shopt set '{}'", key,
        ));
      }
    }
    Ok(())
  }

  pub fn get(&self, query: &str) -> ShResult<Option<String>> {
    // TODO: handle escapes?
    let mut query = query.split('.');
    let Some(key) = query.next() else {
      return Err(sherr!(SyntaxErr, "shopt: No option given",));
    };
    let remainder = query.collect::<Vec<_>>().join(".");

    match key {
      "core" => self.core.get(&remainder),
      "line" => self.line.get(&remainder),
      "set" => self.set.get(&remainder),
      "prompt" => self.prompt.get(&remainder),
			"highlight" => self.highlight.get(&remainder),
      _ => Err(sherr!(
        SyntaxErr,
        "shopt: Unknown shopt set '{}'", key,
      )),
    }
  }
}

shopt_group! {
  #[derive(Clone, Debug)]
  pub struct ShOptLine ("line") {
    /// Whether to automatically insert a newline when the input is incomplete
    linebreak_on_incomplete: bool = true,

    /// The maximum height of the line editor viewport window. Can be a positive number or a percentage of terminal height like "50%"
    viewport_height: String = "50%".to_string(),

    /// Whether to display line numbers in multiline input
    line_numbers: bool = true,

    /// The line offset from the top or bottom of the viewport to trigger scrolling
    scroll_offset: usize = 2,

    /// The number of spaces a tab character represents in the line editor
    tab_width: usize = 4,

    /// Whether to automatically indent new lines in multiline commands
    auto_indent: bool = true,

    /// Whether to suggest commands from history as commands are typed
    auto_suggest: bool = true,
  }
}

shopt_group! {
  #[derive(Clone, Debug)]
  pub struct ShOptSet ("set") {
    /// If set, the shell will remember the full path of commands and use that information to speed up command lookup
    hashall: bool = true,
    /// Enables modal line editing mode.
    vi: bool = false,
    /// If set, all variables that are assigned will be automatically exported to the environment of subsequently executed commands
    allexport: bool = false,
    /// If set, the shell will exit immediately if any command exits with a non-zero status, with some exceptions
    errexit: bool = false,
    /// If set, '>' and '>>' redirections will fail if the target file already exists
    noclobber: bool = false,
    /// If set, jobs run in their own process groups, and report status before the next prompt.
    monitor: bool = true,
    /// If set, filename expansion (globbing) is disabled
    noglob: bool = false,
    /// If set, the shell will not execute any interpreted commands. Useful for testing scripts.
    noexec: bool = false,
    /// If set, function definitions will not be written to command history.
    nolog: bool = false,
    /// If set, the shell will print job status info asynchronously when jobs exit or are stopped
    notify: bool = false,
    /// If set, attempting to expand an unset variable besides '$*' or '@' is an error
    nounset: bool = false,
    /// If set, the shell will write it's input to stderr as it is read.
    verbose: bool = false,
    /// If set, the shell will write a trace for each command after it is expanded but before it is executed.
    xtrace: bool = false,
  }
}

shopt_group! {
  #[derive(Clone, Debug)]
  pub struct ShOptCore ("core") {
    /// Include hidden files in glob patterns
    dotglob: bool = false,

    /// Allow navigation to directories by passing the directory as a command directly
    autocd: bool = false,

    /// Ignore consecutive duplicate command history entries
    hist_ignore_dupes: bool = true,

    /// Maximum number of entries in the command history file (-1 for unlimited)
    #[validate(|v: &isize| if *v < -1 {
      Err("expected a non-negative integer or -1 for max_hist value".into())
    } else {
      Ok(())
    })]
    max_hist: isize = 10_000,

    /// Whether or not to allow comments in interactive mode
    interactive_comments: bool = true,

    /// Whether or not to automatically save commands to the command history file
    auto_hist: bool = true,

    /// Whether or not to allow shed to trigger the terminal bell
    bell_enabled: bool = true,

    /// Maximum limit of recursive shell function calls
    max_recurse_depth: usize = 1000,

    /// Whether echo expands escape sequences by default
    xpg_echo: bool = false,
  }
}

shopt_group! {
  #[derive(Clone, Debug)]
  pub struct ShOptPrompt ("prompt") {
    /// Maximum number of path segments used in the '\W' prompt escape sequence
    trunc_prompt_path: usize = 4,

    /// Maximum number of completion candidates displayed upon pressing tab
    comp_limit: usize = 100,

    /// The leader key sequence used in keymap bindings
    leader: String = " ".to_string(),

    /// Command to execute as a screensaver after idle timeout
    screensaver_cmd: String = String::new(),

    /// Idle time in seconds before running screensaver_cmd (0 = disabled)
    screensaver_idle_time: usize = 0,

    /// Whether tab completion matching is case-insensitive
    completion_ignore_case: bool = false,

    /// If set, enables history concatenation with Shift+Up/Down
    hist_cat: bool = true,

		/// If set, expands aliases on the prompt instead of after submitting
		expand_aliases: bool = true,
  }
}

shopt_group! {
  #[derive(Clone, Debug)]
  pub struct ShOptHighlight ("highlight") {
		/// Whether to enable syntax highlighting in the line editor
		enable: bool = true,
		/// The color used for highlighting strings
		string: String = "yellow".into(),
		/// The color used for highlighting keywords like 'if' and 'for'
		keyword: String = "yellow".into(),
		/// The color used for highlighting valid commands
		valid_command: String = "green".into(),
		/// The color used for highlighting invalid commands
		invalid_command: String = "bold red".into(),
		/// The color used for highlighting control flow keywords like 'break' and 'return'
		control_flow_keyword: String = "magenta".into(),
		/// The color used for highlighting command arguments
		argument: String = "white".into(),
		/// The color used for highlighting arguments that refer to existing files
		argument_file: String = "underline white".into(),
		/// The color used for highlighting variables
		variable: String = "cyan".into(),
		/// The color used for highlighting operators like pipes and redirects
		operator: String = "bold".into(),
		/// The color used for highlighting comments
		comment: String = "italic bright black".into(),
		/// The color used for highlighting glob characters
		glob: String = "bright cyan".into(),
		/// The color used for highlighting the current selection
		selection: String = "black on white".into(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn all_core_fields_covered() {
    let ShOptCore {
      dotglob,
      autocd,
      hist_ignore_dupes,
      max_hist,
      interactive_comments,
      auto_hist,
      bell_enabled,
      max_recurse_depth,
      xpg_echo,
    } = ShOptCore::default();
    // If a field is added to the struct, this destructure fails to compile.
    let _ = (
      dotglob,
      autocd,
      hist_ignore_dupes,
      max_hist,
      interactive_comments,
      auto_hist,
      bell_enabled,
      max_recurse_depth,
      xpg_echo,
    );
  }

  #[test]
  fn set_and_get_core_bool() {
    let mut opts = ShOpts::default();
    assert!(!opts.core.dotglob);

    opts.set("core.dotglob", "true").unwrap();
    assert!(opts.core.dotglob);

    opts.set("core.dotglob", "false").unwrap();
    assert!(!opts.core.dotglob);
  }

  #[test]
  fn set_and_get_core_int() {
    let mut opts = ShOpts::default();
    assert_eq!(opts.core.max_hist, 10_000);

    opts.set("core.max_hist", "500").unwrap();
    assert_eq!(opts.core.max_hist, 500);

    opts.set("core.max_hist", "-1").unwrap();
    assert_eq!(opts.core.max_hist, -1);

    assert!(opts.set("core.max_hist", "-500").is_err());
  }

  #[test]
  fn set_and_get_prompt_opts() {
    let mut opts = ShOpts::default();

    opts.set("prompt.comp_limit", "50").unwrap();
    assert_eq!(opts.prompt.comp_limit, 50);

    opts.set("prompt.leader", "space").unwrap();
    assert_eq!(opts.prompt.leader, "space");
  }

  #[test]
  fn query_set_returns_none() {
    let mut opts = ShOpts::default();
    let result = opts.query("core.autocd=true").unwrap();
    assert!(result.is_none());
    assert!(opts.core.autocd);
  }

  #[test]
  fn query_get_returns_some() {
    let opts = ShOpts::default();
    let result = opts.get("core.dotglob").unwrap();
    assert!(result.is_some());
    let text = result.unwrap();
    assert!(text.contains("false"));
  }

  #[test]
  fn invalid_category_errors() {
    let mut opts = ShOpts::default();
    assert!(opts.set("bogus.dotglob", "true").is_err());
    assert!(opts.get("bogus.dotglob").is_err());
  }

  #[test]
  fn invalid_option_errors() {
    let mut opts = ShOpts::default();
    assert!(opts.set("core.nonexistent", "true").is_err());
    assert!(opts.set("prompt.nonexistent", "true").is_err());
  }

  #[test]
  fn invalid_value_errors() {
    let mut opts = ShOpts::default();
    assert!(opts.set("core.dotglob", "notabool").is_err());
    assert!(opts.set("core.max_hist", "notanint").is_err());
    assert!(opts.set("core.max_recurse_depth", "-5").is_err());
    assert!(opts.set("prompt.comp_limit", "abc").is_err());
  }

  #[test]
  fn get_category_lists_all() {
    let opts = ShOpts::default();
    let core_output = opts.get("core").unwrap().unwrap();
    assert!(core_output.contains("dotglob"));
    assert!(core_output.contains("autocd"));
    assert!(core_output.contains("max_hist"));
    assert!(core_output.contains("bell_enabled"));

    let prompt_output = opts.get("prompt").unwrap().unwrap();
    assert!(prompt_output.contains("comp_limit"));

    let line_output = opts.get("line").unwrap().unwrap();
    assert!(line_output.contains("tab_width"));
  }
}
