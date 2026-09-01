//! Miscellaneous per-run shell metadata
//!
//! Assorted runtime state: command timing ([`CmdTimer`]), the resolved-utility and
//! `$PATH` table ([`PathTable`], [`Utility`]), and loop/fork depth guards.

use std::{
  collections::{VecDeque, vec_deque},
  ffi::CString,
  fmt::Write,
  os::fd::OwnedFd,
  path::{Path, PathBuf},
  rc::Rc,
  time::{Duration, Instant},
};

use nix::{
  libc::time_t,
  poll::PollTimeout,
  sys::{
    resource::{self, Usage, UsageWho},
    time::TimeVal,
  },
};
use regex::Regex;

use crate::{
  HashMap,
  expand::{
    alias,
    glob::{GlobOpts, Pattern},
  },
  match_loop,
  readline::{Candidate, CompSpec},
  sherr,
  state::{Shed, params, paths, vars::VarStr},
  system_msg,
  util::{error::ShResult, ui},
  var,
};

use super::{
  autocmd, db,
  jobs::Job,
  keys::KeyEvent,
  logic::AutoCmdKind,
  vars::{VarFlags, VarKind},
};
#[derive(Debug)]
pub(crate) struct CmdTimer {
  wall_start: Instant,
  self_usage_start: Option<Usage>,
  child_usage_start: Option<Usage>,
  wall_end: Option<Duration>,
  self_usage_end: Option<Usage>,
  child_usage_end: Option<Usage>,
}

impl CmdTimer {
  pub(crate) fn new() -> ShResult<Self> {
    let (self_usage_start, child_usage_start) = (
      Some(resource::getrusage(UsageWho::RUSAGE_SELF)?),
      Some(resource::getrusage(UsageWho::RUSAGE_CHILDREN)?),
    );
    Ok(Self {
      wall_start: Instant::now(),
      self_usage_start,
      child_usage_start,
      wall_end: None,
      self_usage_end: None,
      child_usage_end: None,
    })
  }

  pub(crate) fn stop(&mut self) -> ShResult<()> {
    self.wall_end = Some(self.wall_start.elapsed());
    self.self_usage_end = Some(resource::getrusage(UsageWho::RUSAGE_SELF)?);
    self.child_usage_end = Some(resource::getrusage(UsageWho::RUSAGE_CHILDREN)?);
    self.report()?;
    Ok(())
  }

  pub(crate) fn still_running(&self) -> bool {
    self.wall_end.is_none() && self.self_usage_end.is_none() && self.child_usage_end.is_none()
  }

  pub(crate) fn cpu_pct(&self) -> ShResult<f64> {
    if self.still_running() {
      return Err(sherr!(
        InternalErr,
        "attempt to get CPU percentage from a CmdTimer that is still running"
      ));
    }
    let total_user_secs = self.total_user_secs()?;
    let total_sys_secs = self.total_sys_secs()?;
    let total_wall_secs = self.wall_end.unwrap().as_secs_f64();

    if total_wall_secs > 0.0 {
      Ok(((total_user_secs + total_sys_secs) / total_wall_secs) * 100.0)
    } else {
      Ok(0.0)
    }
  }

  pub(crate) fn max_rss(&self) -> ShResult<i64> {
    if self.still_running() {
      return Err(sherr!(
        InternalErr,
        "attempt to get max RSS from a CmdTimer that is still running"
      ));
    }
    let self_r_maxrss = self.self_usage_end.unwrap().max_rss();
    let child_r_maxrss = self.child_usage_end.unwrap().max_rss();
    Ok(self_r_maxrss.max(child_r_maxrss))
  }

  pub(crate) fn total_wall_ms(&self) -> ShResult<i64> {
    if self.still_running() {
      return Err(sherr!(
        InternalErr,
        "attempt to get wall time from a CmdTimer that is still running"
      ));
    }
    Ok(self.wall_end.unwrap().as_millis() as i64)
  }

  pub(crate) fn total_user_ms(&self) -> ShResult<i64> {
    if self.still_running() {
      return Err(sherr!(
        InternalErr,
        "attempt to get user time from a CmdTimer that is still running"
      ));
    }
    let self_user_delta =
      self.self_usage_end.unwrap().user_time() - self.self_usage_start.unwrap().user_time();
    let child_user_delta =
      self.child_usage_end.unwrap().user_time() - self.child_usage_start.unwrap().user_time();
    Ok(Self::tv_to_ms(self_user_delta) + Self::tv_to_ms(child_user_delta))
  }

  pub(crate) fn total_sys_ms(&self) -> ShResult<i64> {
    if self.still_running() {
      return Err(sherr!(
        InternalErr,
        "attempt to get system time from a CmdTimer that is still running"
      ));
    }
    let self_sys_delta =
      self.self_usage_end.unwrap().system_time() - self.self_usage_start.unwrap().system_time();
    let child_sys_delta =
      self.child_usage_end.unwrap().system_time() - self.child_usage_start.unwrap().system_time();
    Ok(Self::tv_to_ms(self_sys_delta) + Self::tv_to_ms(child_sys_delta))
  }

  pub(crate) fn total_user_secs(&self) -> ShResult<f64> {
    let ms = self.total_user_ms()?;
    let seconds = ms as f64 / 1000.0;

    Ok(seconds)
  }

  pub(crate) fn total_sys_secs(&self) -> ShResult<f64> {
    let ms = self.total_sys_ms()?;
    let seconds = ms as f64 / 1000.0;

    Ok(seconds)
  }

  pub(crate) fn tv_to_ms(tv: TimeVal) -> i64 {
    let sec_millis = (tv.tv_sec() * 1000) as time_t;
    let usec_millis = (tv.tv_usec() / 1000) as time_t;
    sec_millis + usec_millis
  }

  fn format_ms(total: i64) -> String {
    let millis = total % 1000;
    let total_secs = total / 1000;
    let secs = total_secs % 60;
    let total_mins = total_secs / 60;
    let mins = total_mins % 60;
    let hours = total_mins / 60;

    let mut result = String::new();
    if hours > 0 {
      write!(result, "{hours}h").unwrap();
    }
    write!(result, "{mins}m").unwrap();
    write!(result, "{secs}.{millis:03}").unwrap();
    result
  }

  pub(crate) fn total_wall_formatted(&self) -> ShResult<String> {
    if self.still_running() {
      return Err(sherr!(
        InternalErr,
        "attempt to get wall time from a CmdTimer that is still running"
      ));
    }
    let total_ms = self.total_wall_ms()?;
    Ok(Self::format_ms(total_ms))
  }
  pub(crate) fn total_user_formatted(&self) -> ShResult<String> {
    if self.still_running() {
      return Err(sherr!(
        InternalErr,
        "attempt to get user time from a CmdTimer that is still running"
      ));
    }
    let total_ms = self.total_user_ms()?;
    Ok(Self::format_ms(total_ms))
  }
  pub(crate) fn total_sys_formatted(&self) -> ShResult<String> {
    if self.still_running() {
      return Err(sherr!(
        InternalErr,
        "attempt to get system time from a CmdTimer that is still running"
      ));
    }
    let total_ms = self.total_sys_ms()?;
    Ok(Self::format_ms(total_ms))
  }

  pub(crate) fn format_report(&self, fmt_str: &str) -> ShResult<String> {
    if self.still_running() {
      return Err(sherr!(
        InternalErr,
        "attempt to format a CmdTimer that is still running"
      ));
    }

    let mut output = String::new();
    let mut chars = fmt_str.chars().peekable();

    match_loop!(chars.next() => ch, {
      '\\' => {
        if let Some(esc) = chars.next() {
          output.push(esc);
        }
      }
      '%' => {
        let Some(param) = chars.next() else { break; };
        match param {
          'm' => {
            let Some(param2) = chars.next() else { break; };
            let millis = match param2 {
              'E' => self.wall_end.unwrap().as_millis() as i64,
              'U' => (self.total_user_secs()? * 1000.0) as i64,
              'S' => (self.total_sys_secs()? * 1000.0) as i64,
              _ => {
                output.push('%');
                output.push('m');
                output.push(param2);
                continue;
              }
            };

            write!(output, "{millis}").unwrap();
          }
          'u' => {
            let Some(param2) = chars.next() else { break; };
            let micros = match param2 {
              'E' => self.wall_end.unwrap().as_micros() as i64,
              'U' => (self.total_user_secs()? * 1_000_000.0).floor() as i64,
              'S' => (self.total_sys_secs()? * 1_000_000.0).floor() as i64,
              _ => {
                output.push('%');
                output.push('u');
                output.push(param2);
                continue;
              }
            };

            write!(output, "{micros}").unwrap();
          }
          '*' => {
            let Some(param2) = chars.next() else { break; };
            let millis = match param2 {
              'E' => self.wall_end.unwrap().as_millis() as i64,
              'U' => (self.total_user_secs()? * 1000.0) as i64,
              'S' => (self.total_sys_secs()? * 1000.0) as i64,
              _ => {
                output.push('%');
                output.push('*');
                output.push(param2);
                continue;
              }
            };
            output.push_str(&Self::format_ms(millis));
          }
          'E' => {
            // real seconds
            let secs = self.wall_end.unwrap().as_secs();
            write!(output, "{secs}").unwrap();
          }
          'U' => {
            // CPU user mode seconds
            let total = self.total_user_secs()?;

            write!(output, "{total}").unwrap();
          }
          'S' => {
            // CPU kernel mode seconds
            let total = self.total_sys_secs()?;

            write!(output, "{total}").unwrap();
          }
          'P' => {
            // CPU percentage ((user + sys) / real * 100)
            let total_user_secs = self.total_user_secs()?;
            let total_sys_secs = self.total_sys_secs()?;
            let total_wall_secs = self.wall_end.unwrap().as_secs_f64();

            if total_wall_secs > 0.0 {
              let percentage = ((total_user_secs + total_sys_secs) / total_wall_secs) * 100.0;

              write!(output, "{percentage:.2}%").unwrap();
            } else {
              write!(output, "0.00%").unwrap();
            }
          }
          'M' => {
            // max resident set size
            let self_r_maxrss = self.self_usage_end.unwrap().max_rss();
            let child_r_maxrss = self.child_usage_end.unwrap().max_rss();
            let maxrss = self_r_maxrss.max(child_r_maxrss);

            write!(output, "{maxrss}").unwrap();
          }
          _ => {
            output.push('%');
            output.push(param);
            break
          }
        }
      }
      _ => output.push(ch),
    });

    Ok(output)
  }
  fn report(&self) -> ShResult<()> {
    let has_autocmds = Shed::logic(|l| !l.get_autocmds(AutoCmdKind::OnTimeReport).is_empty());

    if has_autocmds {
      let vars = [
        ("TIME_REAL_MS".into(), self.total_wall_ms()?.to_string()),
        ("TIME_USER_MS".into(), self.total_user_ms()?.to_string()),
        ("TIME_SYS_MS".into(), self.total_sys_ms()?.to_string()),
        ("TIME_REAL_FMT".into(), self.total_wall_formatted()?.clone()),
        ("TIME_USER_FMT".into(), self.total_user_formatted()?.clone()),
        ("TIME_SYS_FMT".into(), self.total_sys_formatted()?.clone()),
        ("TIME_CPU_PCT".into(), self.cpu_pct()?.to_string()),
        ("TIME_RSS".into(), self.max_rss()?.to_string()),
      ];
      params::with_vars(vars, || autocmd!(OnTimeReport));
    } else {
      let fmt_str = params::get_time_fmt();
      let report = self.format_report(&fmt_str.to_str_lossy())?;
      system_msg!("{report}");
    }
    Ok(())
  }
}

impl Drop for CmdTimer {
  /// Calls `CmdTimer::stop()` internally
  ///
  /// This allows `CmdTimer` to also be used as an RAII guard
  fn drop(&mut self) {
    self.stop().ok();
  }
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub(crate) enum UtilKind {
  Alias,
  Function,
  Builtin,
  Command(PathBuf),
  File(PathBuf),
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub(crate) struct Utility {
  name: VarStr,
  kind: UtilKind,
}

impl Utility {
  pub(crate) fn alias(name: VarStr) -> Self {
    Self {
      name,
      kind: UtilKind::Alias,
    }
  }
  pub(crate) fn function(name: VarStr) -> Self {
    Self {
      name,
      kind: UtilKind::Function,
    }
  }
  pub(crate) fn builtin(name: VarStr) -> Self {
    Self {
      name,
      kind: UtilKind::Builtin,
    }
  }
  pub(crate) fn command(name: VarStr, path: PathBuf) -> Self {
    Self {
      name,
      kind: UtilKind::Command(path),
    }
  }
  pub(crate) fn file(name: VarStr, path: PathBuf) -> Self {
    Self {
      name,
      kind: UtilKind::File(path),
    }
  }
  pub(crate) fn name(&self) -> VarStr {
    self.name.clone()
  }
  pub(crate) fn kind(&self) -> &UtilKind {
    &self.kind
  }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct PathTable {
  index: HashMap<String, PathBuf>,
}

impl PathTable {
  pub(crate) fn new() -> Self {
    Self::default()
  }
  pub(crate) fn hash_path_list(&mut self, path_list: &str) {
    self.index.clear();
    for entry in paths::path_list_entries(path_list) {
      if !paths::is_executable_file(&entry) {
        continue;
      }
      let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
        continue;
      };
      self.index.entry(name).or_insert_with(|| entry.path());
    }
  }
  pub(crate) fn lookup(&self, cmd: &str) -> Option<&Path> {
    self.index.get(cmd).map(PathBuf::as_path)
  }
  pub(crate) fn insert(&mut self, name: String, path: PathBuf) {
    self.index.insert(name, path);
  }
  pub(crate) fn entries(&self) -> impl Iterator<Item = (&String, &PathBuf)> {
    self.index.iter()
  }
  pub(crate) fn clear(&mut self) {
    self.index.clear();
  }
}

/// Automatically manages loop depth in the meta table.
///
/// When dropped, decrements the loop depth in the meta table.
pub(crate) struct LoopGuard;
impl Drop for LoopGuard {
  fn drop(&mut self) {
    Shed::meta_mut(MetaTab::leave_loop);
  }
}

pub(crate) struct ForkGuard(bool);
impl Drop for ForkGuard {
  fn drop(&mut self) {
    Shed::meta_mut(|m| m.restore_fork(self.0));
  }
}

/// Automatically manages function depth in the meta table.
///
/// When dropped, decrements the function depth in the meta table.
pub(crate) struct FuncGuard;
impl Drop for FuncGuard {
  fn drop(&mut self) {
    Shed::meta_mut(MetaTab::leave_func);
  }
}

pub(crate) struct XtraceGuard;
impl Drop for XtraceGuard {
  fn drop(&mut self) {
    Shed::meta_mut(MetaTab::xtrace_ascend);
  }
}

#[derive(Debug, Clone, Default)]
struct RegexCache {
  regexes: HashMap<String, Rc<Regex>>,
  globs: HashMap<Rc<[u8]>, Rc<Pattern>>,
}

impl RegexCache {
  pub(crate) fn new() -> Self {
    Self::default()
  }
  pub(crate) fn get_regex(&mut self, pat: &str) -> Result<Rc<Regex>, String> {
    if let Some(rx) = self.regexes.get(pat) {
      return Ok(Rc::clone(rx));
    }
    let rx = Rc::new(Regex::new(pat).map_err(|e| e.to_string())?);
    self.regexes.insert(pat.to_string(), Rc::clone(&rx));
    Ok(rx)
  }
  pub(crate) fn get_glob(&mut self, pat: &[u8]) -> Rc<Pattern> {
    if let Some(p) = self.globs.get(pat) {
      return p.clone();
    }
    // `case`/`[[ == ]]` matching is case-sensitive (no `nocasematch` shopt).
    let p = Rc::from(Pattern::compile(pat, GlobOpts::new()));
    self.globs.insert(p.orig().clone(), p.clone());
    p
  }
}

/// Directory jump table used by `prevd`/`nextd`
#[derive(Debug, Clone, Default)]
struct JumpTable {
  table: VecDeque<Rc<PathBuf>>,
  cursor: usize,
}

impl JumpTable {
  fn new() -> Self {
    let mut new = Self::default();
    new.new_dir(std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")));
    new
  }
  pub(crate) fn new_dir(&mut self, path: PathBuf) {
    self.table.truncate(self.cursor + 1);
    self.table.push_back(path.into());
    if self.table.len() > 50 {
      self.table.pop_front();
    }
    self.cursor = self.table.len() - 1;
  }
  pub(crate) fn peek_fwd(&self) -> Option<Rc<PathBuf>> {
    self.table.get(self.cursor + 1).map(Rc::clone)
  }
  pub(crate) fn peek_back(&self) -> Option<Rc<PathBuf>> {
    (self.cursor > 0).then(|| Rc::clone(&self.table[self.cursor - 1]))
  }
  pub(crate) fn commit_fwd(&mut self) {
    if self.cursor + 1 < self.table.len() {
      self.cursor += 1;
    }
  }
  pub(crate) fn commit_back(&mut self) {
    if self.cursor > 0 {
      self.cursor -= 1;
    }
  }
  pub(crate) fn fwd_dirs(&self) -> JumpTableDirs<'_> {
    let start = (self.cursor + 1).min(self.table.len());
    self.table.range(start..).cloned()
  }
  pub(crate) fn back_dirs(&self) -> JumpTableDirs<'_> {
    self.table.range(..self.cursor).cloned()
  }
}
pub(crate) type JumpTableDirs<'a> = std::iter::Cloned<vec_deque::Iter<'a, Rc<PathBuf>>>;

/// Miscellaneous global data storage
#[derive(Debug)]
#[expect(clippy::struct_excessive_bools)]
pub(crate) struct MetaTab {
  // Time when the shell was started, used for calculating shell uptime
  shell_time: Instant,
  // whether or not we initially started as an interactive shell
  // not to be confused with interactive context guarding with Terminal and TermGuard
  interactive_shell: bool,

  // command running duration
  runtime_start: Option<Instant>,
  runtime_stop: Option<Instant>,

  last_job: Option<Job>,

  // pushd/popd stack
  dir_stack: VecDeque<PathBuf>,
  // getopts char offset for opts like -abc
  getopts_offset: usize,

  old_path: Option<VarStr>,
  // utility cache - commands, functions, aliases, etc
  path_cache: PathTable,

  // Regex cache
  // Vanilla regexes
  regexes: RegexCache,

  // envp cache - environment variables for execve
  envp_cache: Option<Rc<[CString]>>,
  // programmable completion specs
  comp_specs: HashMap<VarStr, Box<dyn CompSpec>>,

  // stack of currently open procsubs
  procsub_stack: Vec<Vec<OwnedFd>>,

  // pending keys from widget function
  pending_widget_keys: Vec<KeyEvent>,

  func_depth: usize,
  loop_depth: usize,
  xtrace_depth: usize,
  fork_builtins: bool,

  // completion candidates given by compadd
  comp_add_candidates: Vec<Candidate>,

  // whether or not the last command had a function definition
  last_was_func_def: bool,

  main_loop_timeout: Option<PollTimeout>,

  ignore_hist: bool,

  /// True while a top-level command the REPL recorded in history is executing.
  /// Lets `fc` skip its own just-recorded entry so it targets the previous
  /// command instead of looping on itself.
  current_cmd_recorded: bool,

  /// The exit status of the most recent command substitution
  last_cmdsub_status: Option<i32>,

  jump_table: JumpTable,
}

impl Clone for MetaTab {
  fn clone(&self) -> Self {
    Self {
      shell_time: self.shell_time,
      interactive_shell: self.interactive_shell,
      runtime_start: self.runtime_start,
      runtime_stop: self.runtime_stop,
      dir_stack: self.dir_stack.clone(),
      getopts_offset: self.getopts_offset,
      old_path: self.old_path.clone(),
      loop_depth: self.loop_depth,
      func_depth: self.func_depth,
      xtrace_depth: self.xtrace_depth,
      fork_builtins: self.fork_builtins,
      envp_cache: self.envp_cache.clone(),
      comp_add_candidates: self.comp_add_candidates.clone(),
      regexes: self.regexes.clone(),
      path_cache: self.path_cache.clone(),
      comp_specs: self.comp_specs.clone(),
      pending_widget_keys: self.pending_widget_keys.clone(),
      last_was_func_def: self.last_was_func_def,
      main_loop_timeout: self.main_loop_timeout,
      ignore_hist: self.ignore_hist,
      current_cmd_recorded: self.current_cmd_recorded,
      last_cmdsub_status: self.last_cmdsub_status,
      jump_table: self.jump_table.clone(),

      last_job: None,
      procsub_stack: vec![],
    }
  }
}

impl Default for MetaTab {
  fn default() -> Self {
    Self {
      shell_time: Instant::now(),
      interactive_shell: false,
      runtime_start: None,
      runtime_stop: None,
      last_job: None,
      dir_stack: VecDeque::new(),
      getopts_offset: 0,
      old_path: None,
      loop_depth: 0,
      func_depth: 0,
      xtrace_depth: 0,
      fork_builtins: false,
      envp_cache: None,
      procsub_stack: vec![],
      comp_add_candidates: vec![],
      regexes: RegexCache::new(),
      path_cache: PathTable::new(),
      comp_specs: HashMap::default(),
      pending_widget_keys: vec![],
      last_was_func_def: false,
      main_loop_timeout: None,
      ignore_hist: false,
      current_cmd_recorded: false,
      last_cmdsub_status: None,
      jump_table: JumpTable::new(),
    }
  }
}

pub(crate) struct ProcSubGuard;
impl Drop for ProcSubGuard {
  fn drop(&mut self) {
    Shed::meta_mut(MetaTab::pop_procsub_frame);
  }
}

impl MetaTab {
  pub(crate) fn new() -> Self {
    Self::default()
  }

  /// Set a poll timeout for the main loop to use
  ///
  /// This is used mainly for managing status message lifetimes.
  /// If a status message is showing below the prompt, the timeout
  /// will trigger a redraw and clear it.
  pub(crate) fn set_poll_timeout(&mut self, timeout: Option<PollTimeout>) {
    self.main_loop_timeout = timeout;
  }
  pub(crate) fn take_poll_timeout(&mut self) -> Option<PollTimeout> {
    self.main_loop_timeout.take()
  }

  pub(crate) fn set_last_cmdsub_status(&mut self, status: i32) {
    self.last_cmdsub_status = Some(status);
  }

  pub(crate) fn peek_fwd(&self) -> Option<Rc<PathBuf>> {
    self.jump_table.peek_fwd()
  }
  pub(crate) fn peek_back(&self) -> Option<Rc<PathBuf>> {
    self.jump_table.peek_back()
  }
  pub(crate) fn commit_fwd(&mut self) {
    self.jump_table.commit_fwd();
  }
  pub(crate) fn commit_back(&mut self) {
    self.jump_table.commit_back();
  }
  pub(crate) fn new_dir(&mut self, path: PathBuf) {
    self.jump_table.new_dir(path);
  }
  pub(crate) fn fwd_dirs(&self) -> JumpTableDirs<'_> {
    self.jump_table.fwd_dirs()
  }
  pub(crate) fn back_dirs(&self) -> JumpTableDirs<'_> {
    self.jump_table.back_dirs()
  }

  pub(crate) fn take_last_cmdsub_status(&mut self) -> Option<i32> {
    self.last_cmdsub_status.take()
  }

  pub(crate) fn last_cmdsub_status(&self) -> Option<i32> {
    self.last_cmdsub_status
  }

  pub(crate) fn push_procsub_frame(&mut self) -> ProcSubGuard {
    self.procsub_stack.push(vec![]);
    ProcSubGuard
  }
  pub(crate) fn set_no_hist_save(&mut self) {
    self.ignore_hist = true;
  }

  pub(crate) fn set_current_cmd_recorded(&mut self, recorded: bool) {
    self.current_cmd_recorded = recorded;
  }

  pub(crate) fn current_cmd_recorded(&self) -> bool {
    self.current_cmd_recorded
  }

  pub(crate) fn no_hist_save(&mut self) -> bool {
    std::mem::take(&mut self.ignore_hist)
  }

  pub(crate) fn pop_procsub_frame(&mut self) {
    self.procsub_stack.pop();
  }

  pub(crate) fn save_procsub_fd(&mut self, fd: OwnedFd) {
    if self.procsub_stack.is_empty() {
      self.procsub_stack.push(vec![]);
    }
    if let Some(frame) = self.procsub_stack.last_mut() {
      frame.push(fd);
    }
  }

  pub(crate) fn shell_time(&self) -> Instant {
    self.shell_time
  }
  pub(crate) fn ensure_meta_table() -> ShResult<()> {
    db::query_db(|conn| {
      conn.execute(
        "CREATE TABLE IF NOT EXISTS meta (
					key TEXT PRIMARY KEY,
					value TEXT NOT NULL
				)",
        [],
      )?;
      Ok(())
    })?;
    Ok(())
  }
  pub(crate) fn disable_welcome_message() -> ShResult<()> {
    db::query_db(|conn| {
      conn.execute(
        "INSERT INTO meta (key, value) VALUES ('show_welcome', '0')
				ON CONFLICT(key) DO UPDATE SET value='0' WHERE key='welcome_message'",
        [],
      )?;
      Ok(())
    })?;
    Ok(())
  }
  pub(crate) fn enter_loop(&mut self) -> LoopGuard {
    self.loop_depth += 1;

    LoopGuard
  }
  pub(crate) fn xtrace_descend(&mut self) -> XtraceGuard {
    self.xtrace_depth += 1;

    XtraceGuard
  }
  pub(crate) fn take_fork(&mut self) -> bool {
    std::mem::take(&mut self.fork_builtins)
  }
  pub(crate) fn enter_fork(&mut self, fork: bool) -> ForkGuard {
    let prev = std::mem::replace(&mut self.fork_builtins, fork);
    ForkGuard(prev)
  }
  pub(crate) fn restore_fork(&mut self, prev: bool) {
    self.fork_builtins = prev;
  }
  pub(crate) fn enter_func(&mut self) -> FuncGuard {
    self.func_depth += 1;

    FuncGuard
  }
  // these are private, so that depth can only be managed
  // by the guard struct Drop impls
  fn leave_loop(&mut self) {
    if self.loop_depth > 0 {
      self.loop_depth -= 1;
    }
  }
  fn xtrace_ascend(&mut self) {
    if self.xtrace_depth > 0 {
      self.xtrace_depth -= 1;
    }
  }
  fn leave_func(&mut self) {
    if self.func_depth > 0 {
      self.func_depth -= 1;
    }
  }
  pub(crate) fn xtrace_depth(&self) -> usize {
    self.xtrace_depth
  }
  pub(crate) fn in_loop(&self) -> bool {
    self.loop_depth > 0
  }
  pub(crate) fn loop_depth(&self) -> usize {
    self.loop_depth
  }
  pub(crate) fn in_func(&self) -> bool {
    self.func_depth > 0
  }
  pub(crate) fn func_depth(&self) -> usize {
    self.func_depth
  }
  pub(crate) fn welcome_message(force: bool) -> Option<String> {
    let res = db::query_db(|conn| {
      let result = conn.query_row(
        "SELECT value FROM meta WHERE key='show_welcome'",
        [],
        |row| row.get::<_, String>(0),
      );
      match result {
        Ok(val) => Ok(Some(val)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
      }
    })
    .ok()
    .flatten()
    .flatten();

    if res.is_some_and(|r| r == "0") && !force {
      return None;
    }

    let content_lines = [
      "",
      "\x1b[1mWelcome to shed!\x1b[0m",
      "",
      "Type \x1b[33mhelp\x1b[0m to get started.",
      "",
    ];

    let mut longest = -1;
    for l in &content_lines {
      if longest < (l.len() as i32) {
        longest = l.len() as i32;
      }
    }
    let longest = longest as usize;

    let version = env!("CARGO_PKG_VERSION");

    let mut buf = String::new();

    // ╭─ shed v0.xx.x ───────────╮
    let title = format!(
      "{}{} \x1b[1;35mshed\x1b[0m v{} ",
      ui::TOP_LEFT,
      ui::HOR_LINE,
      version
    );
    ui::pad_line_into(&mut buf, &title, ui::HOR_LINE, ui::TOP_RIGHT, longest);
    buf.push('\n');

    for line in &content_lines {
      let row = format!("{} {}", ui::VERT_LINE, line);
      ui::pad_line_into(&mut buf, &row, " ", ui::VERT_LINE, longest);
      buf.push('\n');
    }

    // ╰──────────────────────────╯
    write!(
      buf,
      "{}{}{}",
      ui::BOT_LEFT,
      ui::HOR_LINE.repeat(longest.saturating_sub(2)),
      ui::BOT_RIGHT
    )
    .unwrap();

    Some(buf)
  }
  pub(crate) fn set_pending_widget_keys(&mut self, keys: &str) {
    let exp = alias::expand_keymap(keys);
    self.pending_widget_keys = exp;
  }
  pub(crate) fn get_regex(&mut self, pat: &str) -> Result<Rc<Regex>, String> {
    self.regexes.get_regex(pat)
  }
  pub(crate) fn get_glob(&mut self, pat: &[u8]) -> Rc<Pattern> {
    self.regexes.get_glob(pat)
  }
  pub(crate) fn take_pending_widget_keys(&mut self) -> Option<Vec<KeyEvent>> {
    if self.pending_widget_keys.is_empty() {
      None
    } else {
      Some(std::mem::take(&mut self.pending_widget_keys))
    }
  }
  pub(crate) fn set_last_job(&mut self, job: Option<Job>) {
    self.last_job = job;
  }
  pub(crate) fn last_job(&self) -> Option<&Job> {
    self.last_job.as_ref()
  }
  pub(crate) fn getopts_char_offset(&self) -> usize {
    self.getopts_offset
  }
  pub(crate) fn inc_getopts_char_offset(&mut self) -> usize {
    let offset = self.getopts_offset;
    self.getopts_offset += 1;
    offset
  }
  pub(crate) fn reset_getopts_char_offset(&mut self) {
    self.getopts_offset = 0;
  }
  pub(crate) fn comp_specs(&self) -> &HashMap<VarStr, Box<dyn CompSpec>> {
    &self.comp_specs
  }
  pub(crate) fn get_comp_spec(&self, cmd: &str) -> Option<Box<dyn CompSpec>> {
    let var_str = VarStr::from(cmd);
    self.comp_specs.get(&var_str).cloned()
  }
  pub(crate) fn set_comp_spec(&mut self, cmd: VarStr, spec: Box<dyn CompSpec>) {
    self.comp_specs.insert(cmd, spec);
  }
  pub(crate) fn remove_comp_spec(&mut self, cmd: &str) -> bool {
    let var_str = VarStr::from(cmd);
    self.comp_specs.remove(&var_str).is_some()
  }
  pub(crate) fn set_last_was_func_def(&mut self, was_func_def: bool) {
    self.last_was_func_def = was_func_def;
  }
  pub(crate) fn take_last_was_func_def(&mut self) -> bool {
    std::mem::take(&mut self.last_was_func_def)
  }
  pub(crate) fn get_exec_files_in_cwd() -> Vec<Rc<Utility>> {
    let cwd = var!("PWD");
    let mut files = vec![];
    if let Ok(entries) = Path::new(&cwd).read_dir() {
      for entry in entries.flatten() {
        let is_exec = paths::is_executable_file(&entry);

        if is_exec && let Some(name) = entry.file_name().to_str() {
          let util = Utility::file(name.into(), entry.path());
          files.push(util.into());
        }
      }
    }
    files
  }
  pub(crate) fn clear_envp(&mut self) {
    self.envp_cache = None;
  }
  pub(crate) fn get_envp(&mut self) -> Rc<[CString]> {
    if let Some(envp) = &self.envp_cache {
      return Rc::clone(envp);
    }

    // Walk scopes outermost-to-innermost so inner bindings shadow outer
    // ones in the flat map. Libc env is not consulted, so shell writes
    // outside this builder can't desync the env children inherit.
    let mut flat: HashMap<String, String> = HashMap::default();
    Shed::vars(|v| {
      for scope in v.scopes_iter() {
        for (name, var) in scope.vars() {
          if var.flags().contains(VarFlags::EXPORT)
            && let VarKind::Str(s) = var.kind()
          {
            flat.insert(name.clone(), s.to_string());
          }
        }
      }
    });

    let envp: Vec<CString> = flat
      .into_iter()
      .map(|(k, v)| {
        let mut bytes = Vec::with_capacity(k.len() + v.len() + 2);
        bytes.extend_from_slice(k.as_bytes());
        bytes.push(b'=');
        bytes.extend_from_slice(v.as_bytes());
        unsafe { CString::from_vec_unchecked(bytes) }
      })
      .collect();

    self.envp_cache = Some(Rc::from(envp.as_slice()));

    self.get_envp()
  }

  /// Look up an external command in the PATH cache. Returns `None` for a
  /// cache miss; callers that want to populate the cache on miss should
  /// call [`try_rehash_path_cache`](Self::try_rehash_path_cache) first.
  pub(crate) fn lookup_cached_cmd(&self, cmd: &str) -> Option<&Path> {
    self.path_cache.lookup(cmd)
  }

  pub(crate) fn path_cache(&self) -> &PathTable {
    &self.path_cache
  }

  pub(crate) fn rehash_path_cache(&mut self) {
    let path = var!("PATH");
    self.old_path = Some(path.clone());
    self.path_cache.hash_path_list(&path.to_str_lossy());
  }

  pub(crate) fn clear_path_cache(&mut self) {
    self.old_path = None;
    self.path_cache.clear();
  }

  pub(crate) fn try_rehash_path_cache(&mut self) {
    let path = var!("PATH");
    if self.old_path.as_ref().is_none_or(|old| *old != path) {
      self.old_path = Some(path.clone());
      self.path_cache.hash_path_list(&path.to_str_lossy());
    }
  }
  pub(crate) fn invalidate_path_cache_if_stale(&mut self) {
    let path = var!("PATH");
    if self.old_path.as_ref().is_none_or(|old| *old != path) {
      self.old_path = Some(path);
      self.path_cache.clear();
    }
  }

  pub(crate) fn cache_cmd(&mut self, name: String, path: PathBuf) {
    self.path_cache.insert(name, path);
  }
  pub(crate) fn start_timer(&mut self) {
    self.runtime_start = Some(Instant::now());
  }
  pub(crate) fn stop_timer(&mut self) -> Option<Duration> {
    self.runtime_stop = Some(Instant::now());
    self.get_time()
  }
  pub(crate) fn get_time(&self) -> Option<Duration> {
    if let (Some(start), Some(stop)) = (self.runtime_start, self.runtime_stop) {
      Some(stop.duration_since(start))
    } else {
      None
    }
  }
  pub(crate) fn comp_add(&mut self, candidate: Candidate) {
    self.comp_add_candidates.push(candidate);
  }
  pub(crate) fn take_comp_candidates(&mut self) -> Vec<Candidate> {
    std::mem::take(&mut self.comp_add_candidates)
  }
  pub(crate) fn set_interactive_shell(&mut self, interactive: bool) {
    self.interactive_shell = interactive;
  }
  /// Returns true if the shell started in interactive mode
  pub(crate) fn interactive_shell(&self) -> bool {
    self.interactive_shell
  }
  pub(crate) fn push_dir(&mut self, path: PathBuf) {
    self.dir_stack.push_front(path);
  }
  pub(crate) fn pop_dir(&mut self) -> Option<PathBuf> {
    self.dir_stack.pop_front()
  }
  pub(crate) fn dirs(&self) -> &VecDeque<PathBuf> {
    &self.dir_stack
  }
  pub(crate) fn dirs_mut(&mut self) -> &mut VecDeque<PathBuf> {
    &mut self.dir_stack
  }
  pub(crate) fn get_cmds_in_path() -> Vec<Rc<Utility>> {
    let path = var!("PATH");
    let path = path.to_str_lossy();
    let paths = paths::path_list_entries(&path);

    let mut seen = crate::HashSet::default();
    let mut cmds = vec![];

    for entry in paths {
      let is_exec = paths::is_executable_file(&entry);

      if is_exec
        && let Some(name) = entry.file_name().to_str()
        && seen.insert(name.to_string())
      {
        let util = Utility::command(name.into(), entry.path());
        cmds.push(util.into());
      }
    }

    cmds
  }
}

#[cfg(test)]
mod cmd_timer_tests {
  //! Coverage targets the cold parts of `CmdTimer`: the `still_running`
  //! Err returns on every reporting method, the `hours > 0` branch in
  //! `format_ms`, and the `format_report` %-spec branches.

  use super::*;
  use crate::tests::testutil::TestGuard;

  fn running_timer() -> CmdTimer {
    CmdTimer::new().unwrap()
  }

  fn stopped_timer() -> CmdTimer {
    let mut t = CmdTimer::new().unwrap();
    t.stop().unwrap();
    t
  }

  // ===================== still_running guards =====================

  #[test]
  fn cpu_pct_errors_when_still_running() {
    let _g = TestGuard::new();
    assert!(running_timer().cpu_pct().is_err());
  }

  #[test]
  fn max_rss_errors_when_still_running() {
    let _g = TestGuard::new();
    assert!(running_timer().max_rss().is_err());
  }

  #[test]
  fn total_wall_ms_errors_when_still_running() {
    let _g = TestGuard::new();
    assert!(running_timer().total_wall_ms().is_err());
  }

  #[test]
  fn total_user_ms_errors_when_still_running() {
    let _g = TestGuard::new();
    assert!(running_timer().total_user_ms().is_err());
  }

  #[test]
  fn total_sys_ms_errors_when_still_running() {
    let _g = TestGuard::new();
    assert!(running_timer().total_sys_ms().is_err());
  }

  #[test]
  fn total_wall_formatted_errors_when_still_running() {
    let _g = TestGuard::new();
    assert!(running_timer().total_wall_formatted().is_err());
  }

  #[test]
  fn total_user_formatted_errors_when_still_running() {
    let _g = TestGuard::new();
    assert!(running_timer().total_user_formatted().is_err());
  }

  #[test]
  fn total_sys_formatted_errors_when_still_running() {
    let _g = TestGuard::new();
    assert!(running_timer().total_sys_formatted().is_err());
  }

  #[test]
  fn format_report_errors_when_still_running() {
    let _g = TestGuard::new();
    assert!(running_timer().format_report("%E").is_err());
  }

  // ===================== format_ms =====================

  #[test]
  fn format_ms_zero() {
    assert_eq!(CmdTimer::format_ms(0), "0m0.000");
  }

  #[test]
  fn format_ms_sub_second_pads_millis() {
    assert_eq!(CmdTimer::format_ms(7), "0m0.007");
    assert_eq!(CmdTimer::format_ms(123), "0m0.123");
  }

  #[test]
  fn format_ms_seconds_only() {
    assert_eq!(CmdTimer::format_ms(45_000), "0m45.000");
  }

  #[test]
  fn format_ms_with_minutes_and_seconds() {
    // 5 min 30.250s
    assert_eq!(CmdTimer::format_ms(5 * 60_000 + 30_250), "5m30.250");
  }

  #[test]
  fn format_ms_includes_hours_when_over_one_hour() {
    // Exercises the `if hours > 0 { write!(result, "{hours}h") }` branch
    // that was uncovered. 2h 15m 7.500s.
    let total = 2 * 3_600_000 + 15 * 60_000 + 7_500;
    assert_eq!(CmdTimer::format_ms(total), "2h15m7.500");
  }

  // ===================== format_report happy paths =====================

  #[test]
  fn format_report_literal_text_passes_through() {
    let _g = TestGuard::new();
    let t = stopped_timer();
    assert_eq!(t.format_report("hello world").unwrap(), "hello world");
  }

  #[test]
  fn format_report_backslash_escapes_next_char() {
    // `\X` consumes the backslash and pushes X verbatim — no special
    // interpretation (so \n is the literal char 'n').
    let _g = TestGuard::new();
    let t = stopped_timer();
    assert_eq!(t.format_report("\\n").unwrap(), "n");
    assert_eq!(t.format_report("a\\\\b").unwrap(), "a\\b");
  }

  #[test]
  fn format_report_e_emits_wall_seconds() {
    let _g = TestGuard::new();
    let t = stopped_timer();
    let out = t.format_report("%E").unwrap();
    assert!(out.chars().all(|c| c.is_ascii_digit()), "got: {out:?}");
  }

  #[test]
  fn format_report_u_and_s_emit_seconds() {
    let _g = TestGuard::new();
    let t = stopped_timer();
    assert!(!t.format_report("%U").unwrap().is_empty());
    assert!(!t.format_report("%S").unwrap().is_empty());
  }

  #[test]
  fn format_report_p_emits_percentage_with_trailing_pct() {
    let _g = TestGuard::new();
    let t = stopped_timer();
    let out = t.format_report("%P").unwrap();
    assert!(out.ends_with('%'), "got: {out:?}");
  }

  #[test]
  fn format_report_m_emits_maxrss() {
    let _g = TestGuard::new();
    let t = stopped_timer();
    let out = t.format_report("%M").unwrap();
    // Just digits (or possibly a sign on weird platforms).
    assert!(
      out.chars().all(|c| c.is_ascii_digit() || c == '-'),
      "got: {out:?}"
    );
  }

  #[test]
  fn format_report_ms_and_us_subspecs_emit_integer_strings() {
    // %mE / %mU / %mS — wall/user/sys in milliseconds.
    // %uE / %uU / %uS — wall/user/sys in microseconds.
    let _g = TestGuard::new();
    let t = stopped_timer();
    for spec in ["%mE", "%mU", "%mS", "%uE", "%uU", "%uS"] {
      let out = t.format_report(spec).unwrap();
      assert!(
        out.chars().all(|c| c.is_ascii_digit() || c == '-'),
        "{spec} → {out:?}"
      );
    }
  }

  #[test]
  fn format_report_star_routes_through_format_ms() {
    // %*E / %*U / %*S all run their ms value through CmdTimer::format_ms.
    // We pinned format_ms's shape above ("Xm" + "Y.ZZZ"), so the output
    // here must contain at least an 'm' and a '.'.
    let _g = TestGuard::new();
    let t = stopped_timer();
    for spec in ["%*E", "%*U", "%*S"] {
      let out = t.format_report(spec).unwrap();
      assert!(out.contains('m') && out.contains('.'), "{spec} → {out:?}");
    }
  }

  // ===================== format_report fallthrough / edge =====================

  #[test]
  fn format_report_unknown_m_subspec_passes_through_literally() {
    let _g = TestGuard::new();
    let t = stopped_timer();
    assert_eq!(t.format_report("%mZ").unwrap(), "%mZ");
  }

  #[test]
  fn format_report_unknown_u_subspec_passes_through_literally() {
    let _g = TestGuard::new();
    let t = stopped_timer();
    assert_eq!(t.format_report("%uZ").unwrap(), "%uZ");
  }

  #[test]
  fn format_report_unknown_star_subspec_passes_through_literally() {
    let _g = TestGuard::new();
    let t = stopped_timer();
    assert_eq!(t.format_report("%*Z").unwrap(), "%*Z");
  }

  #[test]
  fn format_report_unknown_top_level_spec_breaks_loop() {
    // The catchall `_` arm in the %-dispatch pushes %{param} and breaks,
    // so anything after the unknown spec is silently dropped.
    let _g = TestGuard::new();
    let t = stopped_timer();
    let out = t.format_report("%Q extra").unwrap();
    assert!(out.contains("%Q"), "got: {out:?}");
    assert!(!out.contains("extra"), "got: {out:?}");
  }

  #[test]
  fn format_report_trailing_percent_terminates_cleanly() {
    let _g = TestGuard::new();
    let t = stopped_timer();
    assert_eq!(t.format_report("hello%").unwrap(), "hello");
  }

  #[test]
  fn format_report_trailing_backslash_terminates_cleanly() {
    let _g = TestGuard::new();
    let t = stopped_timer();
    assert_eq!(t.format_report("hello\\").unwrap(), "hello");
  }

  #[test]
  fn format_report_trailing_m_with_no_subspec_breaks() {
    // `%m` with nothing after — the inner `let Some(param2) = chars.next() else { break; };`
    // fires on the missing subspec.
    let _g = TestGuard::new();
    let t = stopped_timer();
    let out = t.format_report("ms=%m").unwrap();
    assert_eq!(out, "ms=");
  }
}

#[cfg(test)]
mod jump_table_tests {
  use super::JumpTable;
  use std::path::PathBuf;

  fn seeded() -> JumpTable {
    let mut jt = JumpTable::default();
    jt.new_dir(PathBuf::from("/home"));
    jt
  }

  fn cur(jt: &JumpTable) -> Option<String> {
    jt.table
      .get(jt.cursor)
      .map(|p| p.to_string_lossy().into_owned())
  }

  fn opt(p: Option<std::rc::Rc<PathBuf>>) -> Option<String> {
    p.map(|p| p.to_string_lossy().into_owned())
  }

  fn go_back(jt: &mut JumpTable) -> Option<String> {
    let t = jt.peek_back();
    if t.is_some() {
      jt.commit_back();
    }
    t.map(|p| p.to_string_lossy().into_owned())
  }

  fn go_fwd(jt: &mut JumpTable) -> Option<String> {
    let t = jt.peek_fwd();
    if t.is_some() {
      jt.commit_fwd();
    }
    t.map(|p| p.to_string_lossy().into_owned())
  }

  #[test]
  fn record_advances_and_back_forward_walk() {
    let mut jt = seeded();
    jt.new_dir("/a".into());
    jt.new_dir("/b".into());
    assert_eq!(cur(&jt).as_deref(), Some("/b"));

    assert_eq!(go_back(&mut jt).as_deref(), Some("/a"));
    assert_eq!(go_back(&mut jt).as_deref(), Some("/home"));
    assert_eq!(go_back(&mut jt), None, "seed is the floor");

    assert_eq!(go_fwd(&mut jt).as_deref(), Some("/a"));
    assert_eq!(go_fwd(&mut jt).as_deref(), Some("/b"));
    assert_eq!(go_fwd(&mut jt), None, "no forward past the tip");
  }

  #[test]
  fn new_dir_truncates_forward_history() {
    let mut jt = seeded();
    jt.new_dir("/a".into());
    jt.new_dir("/b".into());
    go_back(&mut jt); // now at /a, forward = [/b]

    jt.new_dir("/x".into());
    assert_eq!(cur(&jt).as_deref(), Some("/x"));
    assert_eq!(
      go_fwd(&mut jt),
      None,
      "branch dropped the old forward entry"
    );
    assert_eq!(
      jt.back_dirs()
        .map(|p| p.to_string_lossy().into_owned())
        .collect::<Vec<_>>(),
      vec!["/home", "/a"]
    );
  }

  #[test]
  fn peek_does_not_move_cursor() {
    let mut jt = seeded();
    jt.new_dir("/a".into());
    assert_eq!(opt(jt.peek_back()).as_deref(), Some("/home"));
    assert_eq!(
      opt(jt.peek_back()).as_deref(),
      Some("/home"),
      "peek is idempotent"
    );
    assert_eq!(cur(&jt).as_deref(), Some("/a"), "cursor unmoved by peek");
  }

  #[test]
  fn cd_records_into_live_jump_table() {
    use crate::tests::testutil::{TestGuard, test_input};
    let _g = TestGuard::new();
    test_input("cd /").unwrap();
    test_input("cd /tmp").unwrap();
    let back = super::Shed::meta(|m| m.back_dirs().count());
    assert!(
      back > 0,
      "cd did not record into the jump table (back={back})"
    );
  }

  #[test]
  fn failed_navigation_leaves_table_and_cwd_intact() {
    use crate::tests::testutil::{TestGuard, test_input};
    use std::fs;
    use tempfile::TempDir;

    let _g = TestGuard::new();
    let root = TempDir::new().unwrap();
    let (a, b, c) = (
      root.path().join("a"),
      root.path().join("b"),
      root.path().join("c"),
    );
    for d in [&a, &b, &c] {
      fs::create_dir(d).unwrap();
    }

    test_input(format!("cd {}", a.display())).unwrap();
    test_input(format!("cd {}", b.display())).unwrap();
    test_input(format!("cd {}", c.display())).unwrap();

    // pull the back target out from under the jump table
    fs::remove_dir(&b).unwrap();

    let cwd_before = std::env::current_dir().unwrap();
    let back_before = super::Shed::meta(|m| m.back_dirs().count());

    // prevd into the now-missing dir must fail…
    test_input("prevd").ok();
    assert_ne!(
      super::Shed::get_status(),
      0,
      "prevd into removed dir should fail"
    );

    // …without changing cwd or corrupting the table (commit-on-success).
    assert_eq!(
      std::env::current_dir().unwrap(),
      cwd_before,
      "cwd moved on a failed prevd"
    );
    assert_eq!(
      super::Shed::meta(|m| m.back_dirs().count()),
      back_before,
      "jump table mutated on a failed prevd"
    );
  }

  #[test]
  fn empty_table_never_panics() {
    let mut jt = JumpTable::default();
    assert_eq!(jt.peek_fwd(), None);
    assert_eq!(jt.peek_back(), None);
    jt.commit_fwd();
    jt.commit_back();
    assert!(jt.fwd_dirs().count() == 0);
    assert!(jt.back_dirs().count() == 0);
  }
}

#[cfg(test)]
mod pattern_tests {
  use crate::expand::glob::{GlobOpts, Pattern};

  fn matches(pat: &str, text: &str) -> bool {
    Pattern::compile(pat.as_bytes(), GlobOpts::new()).is_match(text.as_bytes())
  }

  #[test]
  fn interior_star_matches_like_glob() {
    // Regression (#142): a `*` between two literals used to compile to a literal
    // `Equal`/`StartsWith`/`EndsWith`, so `a*c` never matched `abc`.
    assert!(matches("a*c", "abc"));
    assert!(matches("a*c", "ac")); // star matches empty
    assert!(matches("a*c", "aXXXc"));
    assert!(!matches("a*c", "ab"));
    assert!(!matches("a*c", "bc"));
  }

  #[test]
  fn double_sided_prefix_suffix_must_not_overlap() {
    // `ab*bc` needs len("ab") + len("bc") chars, so it can't match `abc`.
    assert!(!matches("ab*bc", "abc"));
    assert!(matches("ab*bc", "abbc"));
    assert!(matches("ab*bc", "abXbc"));
  }

  #[test]
  fn boundary_star_combined_with_interior_star() {
    assert!(matches("*a*c", "XabYc"));
    assert!(matches("a*c*", "abcX"));
    assert!(matches("a*b*c", "aXbYc"));
    assert!(!matches("a*b*c", "acb"));
  }

  #[test]
  fn many_interior_segments_delegate_to_glob() {
    assert!(matches("*foo*ba*biz*buzz*", "XfooYbaZbizWbuzzV"));
    assert!(!matches("*foo*ba*biz*buzz*", "foobabizbuz"));
  }

  #[test]
  fn escaped_star_is_literal() {
    assert!(matches(r"a\*c", "a*c"));
    assert!(!matches(r"a\*c", "abc"));
  }

  #[test]
  fn plain_boundary_and_exact_shapes() {
    assert!(matches("*", "anything"));
    assert!(matches("foo*", "foobar"));
    assert!(matches("*bar", "foobar"));
    assert!(matches("*oob*", "foobar")); // Contains
    assert!(matches("foo", "foo")); // Equal
    assert!(!matches("foo", "foobar"));
  }
}
