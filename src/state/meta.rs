use super::*;

use std::{
  collections::{HashMap, HashSet, VecDeque},
  fmt::Write,
  os::unix::{
    fs::PermissionsExt,
    net::{UnixListener, UnixStream},
  },
  rc::Rc,
  str::FromStr,
  time::Duration,
};

use itertools::{Itertools, izip};
use nix::{
  poll::PollFd,
  sys::{
    resource::{Usage, UsageWho, getrusage},
    stat::{FchmodatFlags, fchmodat},
    time::TimeVal,
  },
};
use rusqlite::Connection;

use crate::{
  builtin::BUILTINS,
  expand::expand_keymap,
  jobs::Job,
  libsh::error::{ShErr, ShResult},
  match_loop,
  prelude::*,
  procio::borrow_fd,
  readline::{LineData, complete::CompSpec, keys::KeyEvent},
  sherr,
};

#[derive(Debug)]
pub enum StatusHeader {
  ExitCode,
  CommandName,
  Runtime,
  Pid,
  Pgid,
}

#[derive(Debug)]
pub enum QueryHeader {
  Cwd,
  Var(String),
  Status(Vec<StatusHeader>),
}

#[derive(Debug)]
pub enum SocketRequest {
  /// Posts a system message. System messages appear above the prompt, the same way that job status notifications do.
  /// Useful for important information.
  PostSystemMessage(String),
  /// Posts a status message. Status messages appear under the prompt, and are short lived. Will only survive redraws for a few seconds.
  /// Useful for quick notifications.
  PostStatusMessage(String),

  /// Requests information from the shell. The shell will respond with a SocketResponse containing the requested information, or an error if the query was invalid.
  Query(QueryHeader),

  /// Opens a subscription to the shell's event stream. The shell will send a SocketResponse for each event that occurs, until the socket or connnection is closed.
  Subscribe,

  /// Requests the shell to redraw the prompt. The shell will respond by redrawing the prompt, and sending a SocketResponse confirming the redraw.
  RefreshPrompt,

  LineGet(LineHeader),
  LineSet(LineHeader, String),
  LineSendKeys(Vec<KeyEvent>),
}

#[derive(Debug)]
pub enum LineHeader {
  Buffer,
  Cursor,
  Hint,
  Mode,
  Anchor,
}

impl FromStr for SocketRequest {
  type Err = ShErr;
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    let request_kind = s
      .chars()
      .peeking_take_while(|c| c.is_ascii_alphabetic())
      .collect::<String>()
      .to_lowercase();

    // take care of no-argument requests
    match request_kind.trim() {
      "subscribe" => return Ok(Self::Subscribe),
      "redraw" => return Ok(Self::RefreshPrompt),
      _ => {}
    }

    let rest = s[request_kind.len()..].trim();
    let mut sep = String::new();
    let mut rest_chars = rest.chars().peekable();

    // collect the separator
    while let Some(ch) = rest_chars.peek() {
      if !ch.is_ascii_alphanumeric() && ch.is_ascii_graphic() {
        sep.push(*ch);
        rest_chars.next();
      } else {
        break;
      }
    }
    let rest = rest_chars.collect::<String>();
    let mut args = rest.split(&sep);

    match request_kind.trim() {
      "msg" => {
        let Some(msg_kind) = args.next() else {
          return Err(sherr!(ParseErr, "Missing message kind in 'msg' request",));
        };
        match msg_kind.to_lowercase().as_str() {
          "system" => {
            let Some(msg) = args.next() else {
              return Err(sherr!(ParseErr, "Missing message in system msg request",));
            };
            Ok(Self::PostSystemMessage(msg.to_string()))
          }
          "status" => {
            let Some(msg) = args.next() else {
              return Err(sherr!(ParseErr, "Missing message in status msg request",));
            };
            Ok(Self::PostStatusMessage(msg.to_string()))
          }
          _ => Err(sherr!(
            ParseErr,
            "Unknown message kind in 'msg' request: {}",
            msg_kind,
          )),
        }
      }

      "query" => {
        let Some(query_kind) = args.next() else {
          return Err(sherr!(ParseErr, "Missing query kind in 'query' request",));
        };
        match query_kind.to_lowercase().as_str() {
          "cwd" => Ok(Self::Query(QueryHeader::Cwd)),
          "status" => {
            let mut headers = vec![];
            while let Some(header) = args.next() {
              let status_header = match header.to_lowercase().as_str() {
                "code" => StatusHeader::ExitCode,
                "command" => StatusHeader::CommandName,
                "runtime" => StatusHeader::Runtime,
                "pid" => StatusHeader::Pid,
                "pgid" => StatusHeader::Pgid,
                _ => {
                  return Err(sherr!(
                    ParseErr,
                    "Unknown status header in 'query status' request: {}",
                    header,
                  ));
                }
              };
              headers.push(status_header);
            }
            if headers.is_empty() {
              headers = vec![
                StatusHeader::ExitCode,
                StatusHeader::CommandName,
                StatusHeader::Runtime,
                StatusHeader::Pid,
                StatusHeader::Pgid,
              ];
            }
            Ok(Self::Query(QueryHeader::Status(headers)))
          }
          "var" => {
            let Some(var_name) = args.next() else {
              return Err(sherr!(
                ParseErr,
                "Missing variable name in 'query var' request",
              ));
            };
            Ok(Self::Query(QueryHeader::Var(var_name.to_string())))
          }
          _ => Err(sherr!(
            ParseErr,
            "Unknown query kind in 'query' request: {}",
            query_kind,
          )),
        }
      }

      "line" => {
        let Some(header) = args.next() else {
          return Err(sherr!(ParseErr, "Missing line header in 'line' request",));
        };
        match header {
          "get" => {
            let Some(header2) = args.next() else {
              return Err(sherr!(
                ParseErr,
                "Missing line header kind in 'line get' request",
              ));
            };
            match header2 {
              "buffer" => Ok(Self::LineGet(LineHeader::Buffer)),
              "cursor" => Ok(Self::LineGet(LineHeader::Cursor)),
              "hint" => Ok(Self::LineGet(LineHeader::Hint)),
              "mode" => Ok(Self::LineGet(LineHeader::Mode)),
              "anchor" => Ok(Self::LineGet(LineHeader::Anchor)),
              _ => Err(sherr!(
                ParseErr,
                "Unknown line header kind in 'line get' request: {header2}"
              )),
            }
          }
          "set" => {
            let Some(header2) = args.next() else {
              return Err(sherr!(
                ParseErr,
                "Missing line header kind in 'line set' request",
              ));
            };
            let Some(value) = args.next() else {
              return Err(sherr!(ParseErr, "Missing value in 'line set' request",));
            };
            match header2 {
              "buffer" => Ok(Self::LineSet(LineHeader::Buffer, value.to_string())),
              "cursor" => Ok(Self::LineSet(LineHeader::Cursor, value.to_string())),
              "hint" => Ok(Self::LineSet(LineHeader::Hint, value.to_string())),
              "mode" => Ok(Self::LineSet(LineHeader::Mode, value.to_string())),
              "anchor" => Ok(Self::LineSet(LineHeader::Anchor, value.to_string())),
              _ => Err(sherr!(
                ParseErr,
                "Unknown line header kind in 'line set' request: {header2}"
              )),
            }
          }
          "keys" => {
            let Some(value) = args.next() else {
              return Err(sherr!(ParseErr, "Missing value in 'line keys' request",));
            };
            let events = expand_keymap(value);
            Ok(Self::LineSendKeys(events))
          }
          _ => Err(sherr!(
            ParseErr,
            "Unknown line request kind in 'line' request: {header}"
          )),
        }
      }
      _ => Err(sherr!(
        ParseErr,
        "Unknown socket request kind: {}",
        request_kind,
      )),
    }
  }
}

/// The socket used to expose the system/status message interface
#[derive(Debug)]
pub struct ShedSocket {
  listener: UnixListener,
  pid: Pid,
  path: PathBuf,
}

impl ShedSocket {
  pub fn dir() -> String {
    env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| format!("/tmp/shed-{}", nix::unistd::getuid()))
  }
  pub fn path() -> String {
    let pid = Pid::this();
    let runtime_dir = Self::dir();
    format!("{runtime_dir}/shed/{pid}.sock")
  }
  pub fn mode() -> Mode {
    read_vars(|v| v.get_var("SHED_SOCK_MODE"))
      .parse::<u32>()
      .ok()
      .and_then(Mode::from_bits)
      .unwrap_or(Mode::S_IRUSR | Mode::S_IWUSR)
  }
  pub fn new() -> ShResult<Self> {
    let runtime_dir = Self::dir();
    std::fs::create_dir_all(format!("{runtime_dir}/shed"))?;

    let sock_path = Self::path();
    std::fs::remove_file(&sock_path).ok();

    let listener = UnixListener::bind(&sock_path)?;

    // set the permissions for the socket
    // default is read/write for user, no access for group/other
    // this can be overridden using the $SHED_SOCK_MODE env var.
    let mode = Self::mode();

    fchmodat(
      None,
      Path::new(&sock_path),
      mode,
      FchmodatFlags::FollowSymlink,
    )?;

    let raw_fd = listener.into_raw_fd();
    let high_fd = fcntl(raw_fd, FcntlArg::F_DUPFD_CLOEXEC(10))?;
    close(raw_fd)?;

    let listener = unsafe { UnixListener::from_raw_fd(high_fd) };
    listener.set_nonblocking(true).ok();

    write_vars(|v| {
      v.set_var(
        "SHED_SOCK",
        VarKind::Str(sock_path.clone()),
        VarFlags::EXPORT,
      )
    })
    .ok();
    Ok(Self {
      listener,
      pid: Pid::this(),
      path: PathBuf::from(sock_path),
    })
  }
  pub fn listener(&self) -> &UnixListener {
    &self.listener
  }
  pub fn as_raw_fd(&self) -> RawFd {
    self.listener.as_raw_fd()
  }
}

impl Drop for ShedSocket {
  fn drop(&mut self) {
    if Pid::this() == self.pid {
      std::fs::remove_file(&self.path).ok();
    }
  }
}

#[derive(Debug, Clone)]
pub struct CmdTimer {
  command: String,
  wall_start: Instant,
  self_usage_start: Usage,
  child_usage_start: Usage,
  wall_end: Option<Duration>,
  self_usage_end: Option<Usage>,
  child_usage_end: Option<Usage>,
  report_time: bool,
}

impl CmdTimer {
  pub fn new(command: String, report_time: bool) -> ShResult<Self> {
    Ok(Self {
      command,
      wall_start: Instant::now(),
      self_usage_start: getrusage(UsageWho::RUSAGE_SELF)?,
      child_usage_start: getrusage(UsageWho::RUSAGE_CHILDREN)?,
      wall_end: None,
      self_usage_end: None,
      child_usage_end: None,
      report_time,
    })
  }

  pub fn stop(&mut self) -> ShResult<()> {
    self.wall_end = Some(self.wall_start.elapsed());
    self.self_usage_end = Some(getrusage(UsageWho::RUSAGE_SELF)?);
    self.child_usage_end = Some(getrusage(UsageWho::RUSAGE_CHILDREN)?);
    Ok(())
  }

  pub fn still_running(&self) -> bool {
    self.wall_end.is_none() && self.self_usage_end.is_none() && self.child_usage_end.is_none()
  }

  pub fn should_report(&self) -> bool {
    self.report_time
  }

  pub fn cpu_pct(&self) -> ShResult<f64> {
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

  pub fn max_rss(&self) -> ShResult<i64> {
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

  pub fn command(&self) -> &str {
    &self.command
  }

  pub fn total_wall_ms(&self) -> ShResult<i64> {
    if self.still_running() {
      return Err(sherr!(
        InternalErr,
        "attempt to get wall time from a CmdTimer that is still running"
      ));
    }
    Ok(self.wall_end.unwrap().as_millis() as i64)
  }

  pub fn total_user_ms(&self) -> ShResult<i64> {
    if self.still_running() {
      return Err(sherr!(
        InternalErr,
        "attempt to get user time from a CmdTimer that is still running"
      ));
    }
    let self_user_delta =
      self.self_usage_end.unwrap().user_time() - self.self_usage_start.user_time();
    let child_user_delta =
      self.child_usage_end.unwrap().user_time() - self.child_usage_start.user_time();
    Ok(Self::tv_to_ms(self_user_delta) + Self::tv_to_ms(child_user_delta))
  }

  pub fn total_sys_ms(&self) -> ShResult<i64> {
    if self.still_running() {
      return Err(sherr!(
        InternalErr,
        "attempt to get system time from a CmdTimer that is still running"
      ));
    }
    let self_sys_delta =
      self.self_usage_end.unwrap().system_time() - self.self_usage_start.system_time();
    let child_sys_delta =
      self.child_usage_end.unwrap().system_time() - self.child_usage_start.system_time();
    Ok(Self::tv_to_ms(self_sys_delta) + Self::tv_to_ms(child_sys_delta))
  }

  pub fn total_user_secs(&self) -> ShResult<f64> {
    let ms = self.total_user_ms()?;
    let seconds = ms as f64 / 1000.0;

    Ok(seconds)
  }

  pub fn total_sys_secs(&self) -> ShResult<f64> {
    let ms = self.total_sys_ms()?;
    let seconds = ms as f64 / 1000.0;

    Ok(seconds)
  }

  pub fn tv_to_ms(tv: TimeVal) -> i64 {
    let sec_millis = tv.tv_sec() * 1000;
    let usec_millis = tv.tv_usec() / 1000;
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

  pub fn total_wall_formatted(&self) -> ShResult<String> {
    if self.still_running() {
      return Err(sherr!(
        InternalErr,
        "attempt to get wall time from a CmdTimer that is still running"
      ));
    }
    let total_ms = self.total_wall_ms()?;
    Ok(Self::format_ms(total_ms))
  }
  pub fn total_user_formatted(&self) -> ShResult<String> {
    if self.still_running() {
      return Err(sherr!(
        InternalErr,
        "attempt to get user time from a CmdTimer that is still running"
      ));
    }
    let total_ms = self.total_user_ms()?;
    Ok(Self::format_ms(total_ms))
  }
  pub fn total_sys_formatted(&self) -> ShResult<String> {
    if self.still_running() {
      return Err(sherr!(
        InternalErr,
        "attempt to get system time from a CmdTimer that is still running"
      ));
    }
    let total_ms = self.total_sys_ms()?;
    Ok(Self::format_ms(total_ms))
  }

  pub fn format_report(&self, fmt_str: &str) -> ShResult<String> {
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
          'J' => {
            // command name
            output.push_str(&self.command);
          }
          _ => {
            output.push('%');
            output.push(param);
            break
          }
        };
      }
      _ => output.push(ch),
    });

    Ok(output)
  }
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum UtilKind {
  Alias,
  Function,
  Builtin,
  Command(PathBuf),
  File(PathBuf),
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Utility {
  name: String,
  kind: UtilKind,
}

impl Utility {
  pub fn alias(name: String) -> Self {
    Self {
      name,
      kind: UtilKind::Alias,
    }
  }
  pub fn function(name: String) -> Self {
    Self {
      name,
      kind: UtilKind::Function,
    }
  }
  pub fn builtin(name: String) -> Self {
    Self {
      name,
      kind: UtilKind::Builtin,
    }
  }
  pub fn command(name: String, path: PathBuf) -> Self {
    Self {
      name,
      kind: UtilKind::Command(path),
    }
  }
  pub fn file(name: String, path: PathBuf) -> Self {
    Self {
      name,
      kind: UtilKind::File(path),
    }
  }
  pub fn name(&self) -> &str {
    &self.name
  }
  pub fn kind(&self) -> &UtilKind {
    &self.kind
  }
  pub fn path(&self) -> Option<&Path> {
    match &self.kind {
      UtilKind::Alias | UtilKind::Function | UtilKind::Builtin => None,
      UtilKind::Command(path_buf) | UtilKind::File(path_buf) => Some(path_buf),
    }
  }
}

/// A table of metadata for the shell
#[derive(Clone, Debug)]
pub struct MetaTab {
  // Time when the shell was started, used for calculating shell uptime
  shell_time: Instant,

  // command running duration
  runtime_start: Option<Instant>,
  runtime_stop: Option<Instant>,

  socket: Option<Arc<ShedSocket>>,
  subscribers: Vec<Arc<UnixStream>>,
  last_job: Option<Job>,

  // pending system messages
  // are drawn above the prompt and survive redraws
  system_msg: VecDeque<String>,

  // same as system messages,
  // but they appear under the prompt and are erased on redraw
  status_msg: VecDeque<String>,

  // pushd/popd stack
  dir_stack: VecDeque<PathBuf>,
  // getopts char offset for opts like -abc
  getopts_offset: usize,

  old_path: Option<String>,
  old_pwd: Option<String>,
  // utility cache - commands, functions, aliases, etc
  util_cache: HashSet<Rc<Utility>>,
  // programmable completion specs
  comp_specs: HashMap<String, Box<dyn CompSpec>>,

  // pending keys from widget function
  pending_widget_keys: Vec<KeyEvent>,

  // whether or not the last command had a function definition
  last_was_func_def: bool,
}

impl Default for MetaTab {
  fn default() -> Self {
    Self {
      shell_time: Instant::now(),
      runtime_start: None,
      runtime_stop: None,
      socket: None,
      subscribers: vec![],
      last_job: None,
      system_msg: VecDeque::new(),
      status_msg: VecDeque::new(),
      dir_stack: VecDeque::new(),
      getopts_offset: 0,
      old_path: None,
      old_pwd: None,
      util_cache: HashSet::new(),
      comp_specs: HashMap::new(),
      pending_widget_keys: vec![],
      last_was_func_def: false,
    }
  }
}

impl MetaTab {
  pub fn new() -> Self {
    Self::default()
  }
  pub fn shell_time(&self) -> Instant {
    self.shell_time
  }
	pub fn ensure_meta_table(&self) -> ShResult<()> {
		query_db(|conn| {
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
	pub fn disable_welcome_message(&self) -> ShResult<()> {
		query_db(|conn| {
			conn.execute(
				"INSERT INTO meta (key, value) VALUES ('show_welcome', '0')
				ON CONFLICT(key) DO UPDATE SET value='0' WHERE key='welcome_message'",
				[],
			)?;
			Ok(())
		})?;
		Ok(())
	}
	pub fn welcome_message(&self) -> Option<String> {
		let res = query_db(|conn| {
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
		}).ok().flatten().flatten();

		if res.is_some_and(|r| r == "0") {
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
		content_lines.iter().for_each(|l| {
			if longest < (l.len() as i32) {
				longest = l.len() as i32;
			}
		});
		let longest = longest as usize;


		let version = env!("CARGO_PKG_VERSION");

		use crate::libsh::ui;
		let mut buf = String::new();

		// ╭─ shed v0.xx.x ───────────╮
		let title = format!(
			"{}{} \x1b[1;35mshed\x1b[0m \x1b[90mv{}\x1b[0m ",
			ui::TOP_LEFT, ui::HOR_LINE, version
		);
		ui::pad_line(&mut buf, &title, ui::HOR_LINE, ui::TOP_RIGHT, longest);
		buf.push('\n');

		for line in &content_lines {
			let row = format!("{} {}", ui::VERT_LINE, line);
			ui::pad_line(&mut buf, &row, " ", ui::VERT_LINE, longest);
			buf.push('\n');
		}

		// ╰──────────────────────────╯
		write!(
			buf, "{}{}{}",
			ui::BOT_LEFT,
			ui::HOR_LINE.repeat(longest.saturating_sub(2)),
			ui::BOT_RIGHT
		).unwrap();

		Some(buf)
	}
  pub fn set_pending_widget_keys(&mut self, keys: &str) {
    let exp = expand_keymap(keys);
    self.pending_widget_keys = exp;
  }
  pub fn take_pending_widget_keys(&mut self) -> Option<Vec<KeyEvent>> {
    if self.pending_widget_keys.is_empty() {
      None
    } else {
      Some(std::mem::take(&mut self.pending_widget_keys))
    }
  }
  pub fn set_last_job(&mut self, job: Option<Job>) {
    self.last_job = job;
  }
  pub fn last_job(&self) -> Option<&Job> {
    self.last_job.as_ref()
  }
  pub fn getopts_char_offset(&self) -> usize {
    self.getopts_offset
  }
  pub fn inc_getopts_char_offset(&mut self) -> usize {
    let offset = self.getopts_offset;
    self.getopts_offset += 1;
    offset
  }
  pub fn reset_getopts_char_offset(&mut self) {
    self.getopts_offset = 0;
  }
  pub fn cached_utils(&self) -> impl Iterator<Item = Rc<Utility>> {
    self.util_cache.iter().cloned()
  }
  pub fn cached_cmds(&self) -> impl Iterator<Item = Rc<Utility>> {
    self
      .util_cache
      .iter()
      .filter(|util| matches!(util.kind(), UtilKind::Command(_)))
      .cloned()
  }
  pub fn cached_files(&self) -> impl Iterator<Item = Rc<Utility>> {
    self
      .util_cache
      .iter()
      .filter(|util| matches!(util.kind(), UtilKind::File(_)))
      .cloned()
  }
  pub fn cached_aliases(&self) -> impl Iterator<Item = Rc<Utility>> {
    self
      .util_cache
      .iter()
      .filter(|util| matches!(util.kind(), UtilKind::Alias))
      .cloned()
  }
  pub fn cached_functions(&self) -> impl Iterator<Item = Rc<Utility>> {
    self
      .util_cache
      .iter()
      .filter(|util| matches!(util.kind(), UtilKind::Function))
      .cloned()
  }
  pub fn cached_builtins(&self) -> impl Iterator<Item = Rc<Utility>> {
    self
      .util_cache
      .iter()
      .filter(|util| matches!(util.kind(), UtilKind::Builtin))
      .cloned()
  }
  pub fn comp_specs(&self) -> &HashMap<String, Box<dyn CompSpec>> {
    &self.comp_specs
  }
  pub fn comp_specs_mut(&mut self) -> &mut HashMap<String, Box<dyn CompSpec>> {
    &mut self.comp_specs
  }
  pub fn get_comp_spec(&self, cmd: &str) -> Option<Box<dyn CompSpec>> {
    self.comp_specs.get(cmd).cloned()
  }
  pub fn set_comp_spec(&mut self, cmd: String, spec: Box<dyn CompSpec>) {
    self.comp_specs.insert(cmd, spec);
  }
  pub fn remove_comp_spec(&mut self, cmd: &str) -> bool {
    self.comp_specs.remove(cmd).is_some()
  }
  pub fn cache_contains(&self, cmd: &str) -> bool {
    self.util_cache.iter().any(|util| util.name() == cmd)
  }
  pub fn get_cached_cmd(&self, cmd: &str) -> Option<Rc<Utility>> {
    // used when the hashall option is set
    // and we use cached command paths for the execve system call
    self
      .util_cache
      .iter()
      .find(|util| util.name() == cmd && matches!(util.kind(), UtilKind::Command(_)))
      .cloned()
  }
  pub fn get_cached_util(&self, util: &str) -> Option<Rc<Utility>> {
    self.util_cache.iter().find(|u| u.name() == util).cloned()
  }
  pub fn last_was_func_def(&self) -> bool {
    self.last_was_func_def
  }
  pub fn set_last_was_func_def(&mut self, was_func_def: bool) {
    self.last_was_func_def = was_func_def;
  }
  pub fn take_last_was_func_def(&mut self) -> bool {
    std::mem::take(&mut self.last_was_func_def)
  }
  pub fn get_cmds_in_path() -> Vec<Rc<Utility>> {
    let path = env::var("PATH").unwrap_or_default();
    let paths = path.split(":").map(PathBuf::from);
    let mut cmds = vec![];
    for path in paths {
      if let Ok(entries) = path.read_dir() {
        for entry in entries.flatten() {
          let Ok(meta) = std::fs::metadata(entry.path()) else {
            continue;
          };
          let is_exec = meta.permissions().mode() & 0o111 != 0;

          if meta.is_file()
            && is_exec
            && let Some(name) = entry.file_name().to_str()
          {
            let util = Utility::command(name.to_string(), entry.path());
            cmds.push(util.into());
          }
        }
      }
    }
    cmds
  }
  pub fn get_exec_files_in_cwd() -> Vec<Rc<Utility>> {
    let cwd = env::var("PWD").unwrap_or_default();
    let mut files = vec![];
    if let Ok(entries) = Path::new(&cwd).read_dir() {
      for entry in entries.flatten() {
        let Ok(meta) = std::fs::metadata(entry.path()) else {
          continue;
        };
        let is_exec = meta.permissions().mode() & 0o111 != 0;

        if meta.is_file()
          && is_exec
          && let Some(name) = entry.file_name().to_str()
        {
          let util = Utility::file(name.to_string(), entry.path());
          files.push(util.into());
        }
      }
    }
    files
  }
  pub fn create_socket(&mut self) -> ShResult<()> {
    let sock = ShedSocket::new()?;
    self.socket = Some(sock.into());
    Ok(())
  }
  pub fn get_socket(&self) -> Option<Arc<ShedSocket>> {
    self.socket.as_ref().cloned()
  }
  pub fn get_socket_pollfd(&self) -> Option<PollFd<'_>> {
    self
      .socket
      .as_ref()
      .map(|sock| PollFd::new(borrow_fd(sock.as_raw_fd()), nix::poll::PollFlags::POLLIN))
  }
  pub fn read_socket(&mut self) -> ShResult<Vec<(UnixStream, SocketRequest)>> {
    let mut requests = vec![];
    let Some(listener) = self.get_socket() else {
      return Ok(requests);
    };

    while let Ok((conn, _)) = listener.listener().accept()
      && let Some(req) = self.read_request(&conn)
    {
      requests.push((conn, req));
    }

    Ok(requests)
  }
  pub fn read_request(&self, conn: &UnixStream) -> Option<SocketRequest> {
    conn.set_nonblocking(false).ok();
    let mut bytes = vec![];
    loop {
      let mut buffer = [0u8; 1024];
      match read(conn.as_raw_fd(), &mut buffer) {
        Ok(0) => break,
        Ok(n) => {
          if let Some(pos) = buffer[..n].iter().position(|&b| b == b'\n') {
            bytes.extend_from_slice(&buffer[..pos]);
            break;
          }
          bytes.extend_from_slice(&buffer[..n]);
        }
        Err(Errno::EINTR) => continue,
        Err(e) => {
          write(
            conn,
            format!("error>> failed to parse request: {e}\n").as_bytes(),
          )
          .ok();
          break;
        }
      }
    }
    let input = String::from_utf8_lossy(&bytes).to_string();
    let request = match SocketRequest::from_str(&input) {
      Ok(req) => req,
      Err(e) => {
        write(
          conn,
          format!("error>> failed to parse request: {e}\n").as_bytes(),
        )
        .ok();
        return None;
      }
    };

    Some(request)
  }
  pub fn push_subscriber(&mut self, subscriber: UnixStream) {
    self.subscribers.push(Arc::new(subscriber));
  }
  pub fn notify_autocmd(&self, kind: AutoCmdKind) -> ShResult<()> {
    for subscriber in &self.subscribers {
      write(subscriber, format!("autocmd_event>>{kind}\n").as_bytes()).ok();
    }

    Ok(())
  }
  pub fn num_subscribers(&self) -> usize {
    self.subscribers.len()
  }
  fn broadcast<F: FnMut(&Arc<UnixStream>) -> std::io::Result<()>>(&mut self, mut f: F) {
    let mut dead = vec![];
    for (i, subscriber) in self.subscribers.iter().enumerate() {
      if f(subscriber).is_err() {
        dead.push(i);
      }
    }

    for i in dead.into_iter().rev() {
      self.subscribers.remove(i);
    }
  }
  pub fn notify_job_complete(&mut self, job: &Job) -> ShResult<()> {
    let id = job.tabid().map(|i| (i + 1).to_string()).unwrap_or_default();
    let pids = job.get_pids();
    let stats = job.get_stats();
    let cmds = job.get_cmds();

    self.broadcast(|sub| {
      let mut buf = format!("job>>begin>>{id} {}\n", pids.len());
      for (pid, stat, cmd) in izip!(&pids, &stats, &cmds) {
        let stat_str = match stat {
          WtStat::Exited(_, 0) => "done".to_string(),
          WtStat::Exited(_, n) => format!("failed:{n}"),
          WtStat::Signaled(_, sig, _) => format!("signaled:{sig:?}"),
          other => format!("{other:?}"),
        };
        buf.push_str(&format!("job>>child>>{pid} {stat_str} {cmd}\n"));
      }
      write(sub, buf.as_bytes())?;
      Ok(())
    });
    Ok(())
  }
  pub fn notify_line_edit(&mut self, data: LineData) -> ShResult<()> {
    let LineData {
      buffer,
      cursor,
      anchor,
      hint,
      mode,
    } = data;

    self.broadcast(|sub| {
      let mut buf = String::new();
      buf.push_str(&format!("line>>buffer>>{buffer}\n"));
      buf.push_str(&format!("line>>cursor>>{cursor}\n"));
      if let Some(anchor) = anchor {
        buf.push_str(&format!("line>>anchor>>{anchor}\n"));
      }
      if let Some(hint) = &hint {
        buf.push_str(&format!("line>>hint>>{hint}\n"));
      }
      buf.push_str(&format!("line>>mode>>{mode}\n"));

      write(sub, buf.as_bytes())?;
      Ok(())
    });

    Ok(())
  }
  pub fn notify_key_event(&mut self, event: KeyEvent) -> ShResult<()> {
    let seq = event.as_vim_seq()?;

    self.broadcast(|sub| {
      let buf = format!("line>>key_event>>{seq}\n");
      write(sub, buf.as_bytes())?;
      Ok(())
    });

    Ok(())
  }
  pub fn cache_util(&mut self, util: Rc<Utility>) {
    self.util_cache.insert(util);
  }
  pub fn clear_cached_files(&mut self) {
    self
      .util_cache
      .retain(|util| !matches!(util.kind(), UtilKind::File(_)));
  }
  pub fn clear_cached_cmds(&mut self) {
    self
      .util_cache
      .retain(|util| !matches!(util.kind(), UtilKind::Command(_)));
  }
  pub fn clear_cached_aliases(&mut self) {
    self
      .util_cache
      .retain(|util| !matches!(util.kind(), UtilKind::Alias));
  }
  pub fn clear_cached_functions(&mut self) {
    self
      .util_cache
      .retain(|util| !matches!(util.kind(), UtilKind::Function));
  }
  pub fn clear_cached_builtins(&mut self) {
    self
      .util_cache
      .retain(|util| !matches!(util.kind(), UtilKind::Builtin));
  }
  pub fn clear_cache(&mut self) {
    self.util_cache.clear();
  }
  pub fn rehash_path(&mut self) {
    let path = env::var("PATH").unwrap_or_default();
    self.clear_cached_cmds();
    self.old_path = Some(path.clone());
    let cmds_in_path = Self::get_cmds_in_path();
    for cmd in cmds_in_path {
      self.cache_util(cmd);
    }
  }
  pub fn rehash_cwd(&mut self) {
    let cwd = env::var("PWD").unwrap_or_default();
    self.clear_cached_files();
    self.old_pwd = Some(cwd.clone());
    let exec_files_in_cwd = Self::get_exec_files_in_cwd();
    for file in exec_files_in_cwd {
      self.cache_util(file);
    }
  }
  pub fn rehash_internals(&mut self) {
    write_logic(|l| {
      if !l.dirty {
        return;
      }
      self.clear_cached_aliases();
      self.clear_cached_functions();
      self.clear_cached_builtins();
      let funcs = l.funcs();
      let aliases = l.aliases();
      for func in funcs.keys() {
        let util = Utility::function(func.to_string());
        self.cache_util(util.into());
      }
      for alias in aliases.keys() {
        let util = Utility::alias(alias.to_string());
        self.cache_util(util.into());
      }
      l.dirty = false;
    });

    for cmd in BUILTINS {
      let util = Utility::builtin(cmd.to_string());
      self.cache_util(util.into());
    }
  }
  pub fn rehash(&mut self) {
    self.rehash_path();
    self.rehash_cwd();
    self.rehash_internals();
  }
  pub fn try_rehash_utils(&mut self) {
    let path = env::var("PATH").unwrap_or_default();
    let cwd = env::var("PWD").unwrap_or_default();
    if self.old_path.as_ref().is_none_or(|old| *old != path) {
      self.rehash_path();
    }
    if self.old_pwd.as_ref().is_none_or(|old| *old != cwd) {
      self.rehash_cwd();
    }
    self.rehash_internals();
  }
  pub fn start_timer(&mut self) {
    self.runtime_start = Some(Instant::now());
  }
  pub fn stop_timer(&mut self) -> Option<Duration> {
    self.runtime_stop = Some(Instant::now());
    self.get_time()
  }
  pub fn get_time(&self) -> Option<Duration> {
    if let (Some(start), Some(stop)) = (self.runtime_start, self.runtime_stop) {
      Some(stop.duration_since(start))
    } else {
      None
    }
  }
  pub fn post_system_message(&mut self, message: String) {
    self.system_msg.push_back(message);
  }
  pub fn pop_system_message(&mut self) -> Option<String> {
    self.system_msg.pop_front()
  }
  pub fn system_msg_pending(&self) -> bool {
    !self.system_msg.is_empty()
  }
  pub fn post_status_message(&mut self, message: String) {
    self.status_msg.push_back(message);
  }
  pub fn pop_status_message(&mut self) -> Option<String> {
    self.status_msg.pop_front()
  }
  pub fn status_msg_pending(&self) -> bool {
    !self.status_msg.is_empty()
  }
  pub fn dir_stack_top(&self) -> Option<&PathBuf> {
    self.dir_stack.front()
  }
  pub fn push_dir(&mut self, path: PathBuf) {
    self.dir_stack.push_front(path);
  }
  pub fn pop_dir(&mut self) -> Option<PathBuf> {
    self.dir_stack.pop_front()
  }
  pub fn remove_dir(&mut self, idx: i32) -> Option<PathBuf> {
    if idx < 0 {
      let neg_idx = (self.dir_stack.len() - 1).saturating_sub((-idx) as usize);
      self.dir_stack.remove(neg_idx)
    } else {
      self.dir_stack.remove((idx - 1) as usize)
    }
  }
  pub fn rotate_dirs_fwd(&mut self, steps: usize) {
    self.dir_stack.rotate_left(steps);
  }
  pub fn rotate_dirs_bkwd(&mut self, steps: usize) {
    self.dir_stack.rotate_right(steps);
  }
  pub fn dirs(&self) -> &VecDeque<PathBuf> {
    &self.dir_stack
  }
  pub fn dirs_mut(&mut self) -> &mut VecDeque<PathBuf> {
    &mut self.dir_stack
  }
}
