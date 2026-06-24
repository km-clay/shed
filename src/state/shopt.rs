#![expect(clippy::struct_excessive_bools, clippy::trivially_copy_pass_by_ref)]

use std::{fmt::Display, str::FromStr, time::Duration};

use nix::unistd::write;

use shed_macros::ShOptGroup;

use super::{
  ShErr, ShResult, Shed,
  crate_util::{ansi_from_description, format_time},
  eval::lex::Span,
  expand::expand_keymap,
  procio::stderr_fileno,
  scopes::ScopeStack,
  sherr, two_way_display,
};
use crate::shopt;

pub(crate) fn xtrace_print(argv: &[(String, Span)]) {
  if shopt!(set.xtrace) {
    let words = argv.iter().map(|(s, _)| s.clone()).collect::<Vec<String>>();

    let stderr = stderr_fileno();
    let depth = Shed::vars(ScopeStack::depth);
    let prefix = "+".repeat((depth as usize) + 1);
    let output = format!("{prefix} {}", words.join(" "));
    log::debug!("xtrace: {output:?}");
    write(stderr, output.trim().as_bytes()).ok();
    write(stderr, b"\n").ok();
  }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum CompleteStyle {
  Grid,
  Fuzzy,
}

two_way_display! {CompleteStyle,
  Grid <=> "grid";
  Fuzzy <=> "fuzzy";
}

#[expect(dead_code)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum ShedBellStyle {
  Audible,
  Visible,
  Disable,
}

two_way_display! {ShedBellStyle,
  Audible <=> "audible";
  Visible <=> "visible";
  Disable <=> "disable";
}

#[derive(Clone, Debug)]
pub(crate) struct ShOpts {
  pub core: ShOptCore,
  pub line: ShOptLine,
  pub set: ShOptSet,
  pub prompt: ShOptPrompt,
  pub highlight: ShOptHighlight,
  pub statline: ShOptStatLine,
}

impl Default for ShOpts {
  fn default() -> Self {
    let core = ShOptCore::default();
    let line = ShOptLine::default();
    let set = ShOptSet::default();
    let prompt = ShOptPrompt::default();
    let highlight = ShOptHighlight::default();
    let statline = ShOptStatLine::default();

    Self {
      core,
      line,
      set,
      prompt,
      highlight,
      statline,
    }
  }
}

/// Where shopt values come from when composing rc output.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ShoptSource {
  /// Values from `Self::default()` — the factory rc.
  Defaults,
  /// Values from the live `ShOpts` instance — used to regenerate the
  /// rc file from the user's current configuration.
  Current,
}

/// One rc entry: `(fully-qualified key, group label, "shopt key=val"
/// line, optional doc string)`. The composer in `state::util` joins these
/// into final rc text and decides whether to render the doc as a trailing
/// comment based on `GenRcConfig::include_comments`.
pub(crate) type ShoptRcEntry = (String, &'static str, String, Option<String>);

type RcEntries = Vec<(String, String, Option<String>)>;
impl ShOpts {
  /// All rc entries for every shopt group, in stable group order. The
  /// `group` field is the human-readable section header used when
  /// rendering with comments.
  pub fn rc_entries(&self, source: ShoptSource) -> Vec<ShoptRcEntry> {
    let group_entries: [(&'static str, RcEntries); 6] = match source {
      ShoptSource::Defaults => [
        ("Core", ShOptCore::rc_entries_default()),
        ("Line Editor", ShOptLine::rc_entries_default()),
        ("Prompt", ShOptPrompt::rc_entries_default()),
        ("POSIX Set Options", ShOptSet::rc_entries_default()),
        ("Syntax Highlighting", ShOptHighlight::rc_entries_default()),
        ("Status Line", ShOptStatLine::rc_entries_default()),
      ],
      ShoptSource::Current => [
        ("Core", self.core.rc_entries_current()),
        ("Line Editor", self.line.rc_entries_current()),
        ("Prompt", self.prompt.rc_entries_current()),
        ("POSIX Set Options", self.set.rc_entries_current()),
        ("Syntax Highlighting", self.highlight.rc_entries_current()),
        ("Status Line", self.statline.rc_entries_current()),
      ],
    };

    group_entries
      .into_iter()
      .flat_map(|(group, entries)| {
        entries
          .into_iter()
          .map(move |(key, line, doc)| (key, group, line, doc))
      })
      .collect()
  }

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
      self.query("core")?.unwrap_or_default().clone(),
      self.query("line")?.unwrap_or_default().clone(),
      self.query("set")?.unwrap_or_default().clone(),
      self.query("prompt")?.unwrap_or_default().clone(),
      self.query("highlight")?.unwrap_or_default().clone(),
      self.query("statline")?.unwrap_or_default().clone(),
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
      "statline" => self.statline.set(&remainder, val)?,
      _ => {
        return Err(sherr!(SyntaxErr, "shopt: Unknown shopt set '{}'", key,));
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
      "statline" => self.statline.get(&remainder),
      _ => Err(sherr!(SyntaxErr, "shopt: Unknown shopt set '{}'", key,)),
    }
  }
}

#[expect(clippy::ptr_arg)]
fn validate_viewport_height(v: &String) -> Result<(), String> {
  if v.ends_with('%') {
    let num_part = &v[..v.len() - 1];
    match num_part.parse::<usize>() {
      Ok(num) if num > 0 && num <= 100 => Ok(()),
      _ => Err("viewport_height percentage must be a number between 1% and 100%".into()),
    }
  } else {
    match v.parse::<usize>() {
      Ok(num) if num > 0 => Ok(()),
      _ => Err("viewport_height must be a positive integer or a percentage".into()),
    }
  }
}

#[derive(Clone, Debug, ShOptGroup)]
#[group_name = "line"]
pub(crate) struct ShOptLine {
  /// Whether to automatically insert a newline when the input is incomplete
  #[default(true)]
  pub linebreak_on_incomplete: bool,

  /// The maximum height of the line editor viewport window. Can be a positive number or a percentage of terminal height like "50%"
  #[validate(validate_viewport_height)]
  #[default("50%".to_string())]
  pub viewport_height: String,

  /// If enabled, trims leading/trailing whitespace on submitting a command
  #[default(true)]
  pub trim_on_submit: bool,

  /// Whether to display line numbers in multiline input
  #[default(true)]
  pub line_numbers: bool,

  /// The line offset from the top or bottom of the viewport to trigger scrolling
  #[default(2)]
  pub scroll_offset: usize,

  /// The number of spaces a tab character represents in the line editor
  #[default(4)]
  pub tab_width: usize,

  /// Whether to automatically indent new lines in multiline commands
  #[default(true)]
  pub auto_indent: bool,

  /// Whether to suggest commands from history as commands are typed
  #[default(true)]
  pub auto_suggest: bool,
}

#[derive(Clone, Debug, ShOptGroup)]
#[group_name = "set"]
pub(crate) struct ShOptSet {
  /// If set, the shell will remember the full path of commands and use that information to speed up command lookup
  #[default(true)]
  pub hashall: bool,

  /// Enables modal line editing mode.
  #[default(false)]
  pub vi: bool,

  /// If set, all variables that are assigned will be automatically exported to the environment of subsequently executed commands
  #[default(false)]
  pub allexport: bool,

  /// If set, the shell will exit immediately if any command exits with a non-zero status, with some exceptions
  #[default(false)]
  pub errexit: bool,

  /// If set, '>' and '>>' redirections will fail if the target file already exists
  #[default(false)]
  pub noclobber: bool,

  /// If set, jobs run in their own process groups, and report status before the next prompt.
  #[default(true)]
  pub monitor: bool,

  /// If set, filename expansion (globbing) is disabled
  #[default(false)]
  pub noglob: bool,

  /// If set, the shell will not execute any interpreted commands. Useful for testing scripts.
  #[default(false)]
  pub noexec: bool,

  /// If set, function definitions will not be written to command history.
  #[default(false)]
  pub nolog: bool,

  /// If set, the shell will print job status info asynchronously when jobs exit or are stopped
  #[default(false)]
  pub notify: bool,

  /// If set, attempting to expand an unset variable besides '$*' or '@' is an error
  #[default(false)]
  pub nounset: bool,

  /// If set, the shell will write it's input to stderr as it is read.
  #[default(false)]
  pub verbose: bool,

  /// If set, the shell will write a trace for each command after it is expanded but before it is executed.
  #[default(false)]
  pub xtrace: bool,

  /// If set, a pipeline's status is its last non-zero status, instead of the status of the last command
  #[default(false)]
  pub pipefail: bool,
}

fn validate_max_hist(v: &isize) -> Result<(), String> {
  if *v < -1 {
    Err("expected a non-negative integer or -1 for max_hist value".into())
  } else {
    Ok(())
  }
}

#[expect(clippy::ptr_arg)]
fn validate_bell_style(v: &String) -> Result<(), String> {
  match v.as_str() {
    "audible" | "visible" | "both" => Ok(()),
    _ => Err("bell_style must be 'audible', 'visible', or 'both'".into()),
  }
}

#[derive(Clone, Debug, ShOptGroup)]
#[group_name = "core"]
pub(crate) struct ShOptCore {
  /// Include hidden files in glob patterns
  #[default(false)]
  pub dotglob: bool,

  /// Globs with no matches expand to nothing instead of the original string
  #[default(false)]
  pub nullglob: bool,

  /// Allow navigation to directories by passing the directory as a command directly
  #[default(false)]
  pub autocd: bool,

  /// Ignore consecutive duplicate command history entries
  #[default(true)]
  pub hist_ignore_dupes: bool,

  /// Maximum number of entries in the command history file (-1 for unlimited)
  #[validate(validate_max_hist)]
  #[default(10_000isize)]
  pub max_hist: isize,

  /// Whether or not to allow comments in interactive mode
  #[default(true)]
  pub interactive_comments: bool,

  /// Whether or not to automatically save commands to the command history file
  #[default(true)]
  pub auto_hist: bool,

  /// Whether or not to allow shed to trigger the terminal bell
  #[default(true)]
  pub bell_enabled: bool,

  /// Maximum limit of recursive shell function calls
  #[default(1000usize)]
  pub max_recurse_depth: usize,

  /// Whether echo expands escape sequences by default
  #[default(false)]
  pub xpg_echo: bool,

  /// Whether to use a visible or audible bell
  #[validate(validate_bell_style)]
  #[default("audible".to_string())]
  pub bell_style: String,

  /// Collapses error reporting stack traces
  #[default(false)]
  pub compact_errors: bool,
}

fn validate_leader(v: &String) -> Result<(), String> {
  if expand_keymap(v).is_empty() {
    Err(format!("invalid leader key sequence '{v}'"))
  } else {
    Ok(())
  }
}

#[derive(Clone, Debug, Copy)]
pub(crate) struct IdleTime(Duration);

impl IdleTime {
  pub fn is_zero(&self) -> bool {
    self.0.is_zero()
  }
  pub fn duration(&self) -> Duration {
    self.0
  }
  pub fn zero() -> Self {
    IdleTime(Duration::from_secs(0))
  }
}

impl Default for IdleTime {
  fn default() -> Self {
    Self::zero()
  }
}

impl From<i32> for IdleTime {
  fn from(value: i32) -> Self {
    IdleTime(Duration::from_secs(value as u64))
  }
}

impl From<f64> for IdleTime {
  fn from(value: f64) -> Self {
    IdleTime(Duration::from_secs_f64(value))
  }
}

impl Display for IdleTime {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", format_time(self.0))
  }
}

impl FromStr for IdleTime {
  type Err = ShErr;
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    if s.trim().is_empty() {
      return Ok(IdleTime::zero());
    }
    if let Ok(n) = s.parse::<u64>() {
      return Ok(IdleTime(Duration::from_secs(n)));
    }
    if let Ok(n) = s.parse::<f64>() {
      return Duration::try_from_secs_f64(n)
        .map(IdleTime)
        .map_err(|_| sherr!(SyntaxErr, "invalid idle time value '{s}'"));
    }
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (num_str, unit) = s.split_at(split);
    let n: u64 = num_str
      .parse()
      .map_err(|_| sherr!(SyntaxErr, "invalid idle time value '{s}'"))?;
    let dur = match unit {
      "ms" => Duration::from_millis(n),
      "s" => Duration::from_secs(n),
      "m" => Duration::from_secs(n.saturating_mul(60)),
      "h" => Duration::from_secs(n.saturating_mul(3600)),
      _ => return Err(sherr!(SyntaxErr, "invalid idle time unit in '{s}'")),
    };
    Ok(IdleTime(dur))
  }
}

#[derive(Clone, Debug, ShOptGroup)]
#[group_name = "prompt"]
pub(crate) struct ShOptPrompt {
  /// Maximum number of path segments used in the '\W' prompt escape sequence
  #[default(4usize)]
  pub trunc_prompt_path: usize,

  /// Maximum number of completion candidates displayed upon pressing tab
  #[default(100usize)]
  pub comp_limit: usize,

  /// The leader key sequence used in keymap bindings
  #[validate(validate_leader)]
  #[default("<Space>".to_string())]
  pub leader: String,

  /// Idle time in seconds before firing on-idle-timeout autocmds (0 = disabled)
  #[default(IdleTime::default())]
  pub idle_timeout: IdleTime,

  /// Choose how completion candidates are presented ('fuzzy' or 'grid')
  #[default(CompleteStyle::Grid)]
  pub complete_style: CompleteStyle,

  /// Whether tab completion matching is case-insensitive
  #[default(false)]
  pub completion_ignore_case: bool,

  /// If set, enables history concatenation with Shift+Up/Down
  #[default(true)]
  pub hist_cat: bool,

  /// If set, expands aliases on the prompt instead of after submitting
  #[default(true)]
  pub expand_aliases: bool,

  /// If set, performs substitution (variable, command, etc.) after expanding prompt sequences
  #[default(true)]
  pub substitute: bool,
}

#[derive(Clone, Debug, ShOptGroup)]
#[group_name = "statline"]
pub(crate) struct ShOptStatLine {
  /// Whether to enable the status line
  #[default(false)]
  pub enable: bool,

  /// The raw string used for the left side of the status line.
  #[default("\\e[7;1m-- $SHED_EDIT_MODE -- | \\T -> [${?:-?}]".into())]
  pub left_string: String,

  /// The raw string used for the middle of the status line.
  #[default(String::new())]
  pub middle_string: String,

  /// The raw string used for the right side of the status line.
  #[default(" ln:$EDITOR_LINE/$EDITOR_LINES   SHLVL [$SHLVL]        ".into())]
  pub right_string: String,
}

fn validate_color(v: &String) -> Result<(), String> {
  if ansi_from_description(v).is_err() {
    Err(format!("invalid color description '{v}'"))
  } else {
    Ok(())
  }
}

#[derive(Clone, Debug, ShOptGroup)]
#[group_name = "highlight"]
pub(crate) struct ShOptHighlight {
  /// Whether to enable syntax highlighting in the line editor
  #[default(true)]
  pub enable: bool,

  /// Whether to underline valid paths. Can be slow on network mounts.
  #[default(true)]
  pub check_files: bool,

  /// The color used for highlighting strings
  #[validate(validate_color)]
  #[default("yellow".to_string())]
  pub string: String,

  /// The color used for highlighting keywords like 'if' and 'for'
  #[validate(validate_color)]
  #[default("yellow".to_string())]
  pub keyword: String,

  /// The color used for highlighting external commands
  #[validate(validate_color)]
  #[default("green".to_string())]
  pub external_command: String,

  /// The color used for highlighting builtin commands
  #[validate(validate_color)]
  #[default("green".to_string())]
  pub builtin: String,

  /// The color used for highlighting shell functions
  #[validate(validate_color)]
  #[default("green".to_string())]
  pub function: String,

  /// The color used for highlighting shell aliases
  #[validate(validate_color)]
  #[default("green".to_string())]
  pub alias: String,

  /// The color used for highlighting directories when core.autocd is enabled
  #[validate(validate_color)]
  #[default("green".to_string())]
  pub directory: String,

  /// The color used for highlighting invalid commands
  #[validate(validate_color)]
  #[default("bold red".to_string())]
  pub invalid_command: String,

  /// The color used for highlighting control flow keywords like 'break' and 'return'
  #[validate(validate_color)]
  #[default("magenta".to_string())]
  pub control_flow_keyword: String,

  /// The color used for highlighting command arguments
  #[validate(validate_color)]
  #[default("white".to_string())]
  pub argument: String,

  /// The color used for highlighting arguments that refer to existing files
  #[validate(validate_color)]
  #[default("underline white".to_string())]
  pub argument_file: String,

  /// The color used for highlighting variables
  #[validate(validate_color)]
  #[default("cyan".to_string())]
  pub variable: String,

  /// The color used for highlighting operators like pipes and redirects
  #[validate(validate_color)]
  #[default("bold magenta".to_string())]
  pub operator: String,

  /// The color used for highlighting comments
  #[validate(validate_color)]
  #[default("italic bright black".to_string())]
  pub comment: String,

  /// The color used for highlighting glob characters
  #[validate(validate_color)]
  #[default("bright cyan".to_string())]
  pub glob: String,
}

#[cfg(test)]
mod tests {
  use crate::{assert_status_ne, state};

  use super::*;

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

    opts.set("core.max_hist", "-500").unwrap_err();
    assert_status_ne!(0);
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
    opts.set("bogus.dotglob", "true").unwrap_err();
    opts.get("bogus.dotglob").unwrap_err();
  }

  #[test]
  fn invalid_option_errors() {
    let mut opts = ShOpts::default();
    opts.set("core.nonexistent", "true").unwrap_err();
    opts.set("prompt.nonexistent", "true").unwrap_err();
  }

  #[test]
  fn invalid_value_errors() {
    let mut opts = ShOpts::default();
    opts.set("core.dotglob", "notabool").unwrap_err();
    opts.set("core.max_hist", "notanint").unwrap_err();
    opts.set("core.max_recurse_depth", "-5").unwrap_err();
    opts.set("prompt.comp_limit", "abc").unwrap_err();
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

  // ===================== IdleTime::from_str =====================

  use std::str::FromStr;

  #[test]
  fn empty_string_parses_as_zero() {
    let t: IdleTime = "".parse().unwrap();
    assert!(t.is_zero());
  }

  #[test]
  fn idle_time_parses_bare_integer_as_seconds() {
    let t: IdleTime = "30".parse().unwrap();
    assert_eq!(t.0, Duration::from_secs(30));
  }

  #[test]
  fn idle_time_parses_float_as_seconds() {
    let t: IdleTime = "1.5".parse().unwrap();
    // Use sub-second precision via from_secs_f64.
    let expected = Duration::from_secs_f64(1.5);
    assert_eq!(t.0, expected);
  }

  #[test]
  fn idle_time_ms_suffix() {
    let t: IdleTime = "500ms".parse().unwrap();
    assert_eq!(t.0, Duration::from_millis(500));
  }

  #[test]
  fn idle_time_s_suffix() {
    let t: IdleTime = "45s".parse().unwrap();
    assert_eq!(t.0, Duration::from_secs(45));
  }

  #[test]
  fn idle_time_m_suffix() {
    let t: IdleTime = "5m".parse().unwrap();
    assert_eq!(t.0, Duration::from_mins(5));
  }

  #[test]
  fn idle_time_h_suffix() {
    let t: IdleTime = "2h".parse().unwrap();
    assert_eq!(t.0, Duration::from_hours(2));
  }

  #[test]
  fn idle_time_unknown_unit_errors() {
    assert!(IdleTime::from_str("5d").is_err());
    assert!(IdleTime::from_str("10x").is_err());
  }

  #[test]
  fn idle_time_nonsense_errors() {
    assert!(IdleTime::from_str("abc").is_err());
  }

  #[test]
  fn idle_time_negative_value_errors() {
    // Negative parses as f64 -1.0 but try_from_secs_f64 rejects it.
    assert!(IdleTime::from_str("-1").is_err());
  }

  #[test]
  fn idle_time_unit_with_no_digits_errors() {
    // "ms" alone — num_str is empty → parse fails.
    assert!(IdleTime::from_str("ms").is_err());
  }

  // ===================== validate_viewport_height =====================

  #[test]
  fn viewport_height_accepts_valid_percent() {
    assert!(validate_viewport_height(&"50%".to_string()).is_ok());
    assert!(validate_viewport_height(&"1%".to_string()).is_ok());
    assert!(validate_viewport_height(&"100%".to_string()).is_ok());
  }

  #[test]
  fn viewport_height_rejects_zero_percent() {
    assert!(validate_viewport_height(&"0%".to_string()).is_err());
  }

  #[test]
  fn viewport_height_rejects_over_100_percent() {
    assert!(validate_viewport_height(&"101%".to_string()).is_err());
    assert!(validate_viewport_height(&"200%".to_string()).is_err());
  }

  #[test]
  fn viewport_height_rejects_non_numeric_percent() {
    assert!(validate_viewport_height(&"abc%".to_string()).is_err());
    assert!(validate_viewport_height(&"%".to_string()).is_err());
    assert!(validate_viewport_height(&"-5%".to_string()).is_err());
  }

  #[test]
  fn viewport_height_accepts_positive_integer() {
    assert!(validate_viewport_height(&"50".to_string()).is_ok());
    assert!(validate_viewport_height(&"1".to_string()).is_ok());
    assert!(validate_viewport_height(&"1000".to_string()).is_ok());
  }

  #[test]
  fn viewport_height_rejects_zero_integer() {
    assert!(validate_viewport_height(&"0".to_string()).is_err());
  }

  #[test]
  fn viewport_height_rejects_negative_integer() {
    assert!(validate_viewport_height(&"-5".to_string()).is_err());
  }

  #[test]
  fn viewport_height_rejects_non_numeric() {
    assert!(validate_viewport_height(&"abc".to_string()).is_err());
    assert!(validate_viewport_height(&String::new()).is_err());
    assert!(validate_viewport_height(&"5.5".to_string()).is_err());
  }
}
