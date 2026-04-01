use super::*;

use std::{
  collections::{HashMap, HashSet, VecDeque},
  os::unix::{
    fs::PermissionsExt,
    net::{UnixListener, UnixStream},
  },
  str::FromStr,
  time::Duration,
};

use itertools::Itertools;

use crate::{
  builtin::BUILTINS,
  expand::expand_keymap,
  jobs::Job,
  libsh::error::{ShErr, ShErrKind, ShResult},
  prelude::*,
  readline::{
    complete::{BashCompSpec, CompSpec},
    keys::KeyEvent,
  },
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
  Jobs,
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
          "jobs" => Ok(Self::Query(QueryHeader::Jobs)),
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
  pub fn new() -> ShResult<Self> {
    let pid = Pid::this();
    let runtime_dir = env::var("XDG_RUNTIME_DIR")
      .unwrap_or_else(|_| format!("/tmp/shed-{}", nix::unistd::getuid()));

    std::fs::create_dir_all(format!("{runtime_dir}/shed"))?;
    let sock_path = format!("{runtime_dir}/shed/{pid}.sock");
    std::fs::remove_file(&sock_path).ok();
    let listener = UnixListener::bind(&sock_path)?;

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
      pid,
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
  // valid command cache
  path_cache: HashSet<String>,
  cwd_cache: HashSet<String>,
  // programmable completion specs
  comp_specs: HashMap<String, Box<dyn CompSpec>>,

  // pending keys from widget function
  pending_widget_keys: Vec<KeyEvent>,
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
      path_cache: HashSet::new(),
      cwd_cache: HashSet::new(),
      comp_specs: HashMap::new(),
      pending_widget_keys: vec![],
    }
  }
}

impl MetaTab {
  pub fn new() -> Self {
    Self {
      comp_specs: Self::get_builtin_comp_specs(),
      ..Default::default()
    }
  }
  pub fn shell_time(&self) -> Instant {
    self.shell_time
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
  pub fn get_builtin_comp_specs() -> HashMap<String, Box<dyn CompSpec>> {
    let mut map = HashMap::new();

    map.insert(
      "cd".into(),
      Box::new(BashCompSpec::new().dirs(true)) as Box<dyn CompSpec>,
    );
    map.insert(
      "pushd".into(),
      Box::new(BashCompSpec::new().dirs(true)) as Box<dyn CompSpec>,
    );
    map.insert(
      "popd".into(),
      Box::new(BashCompSpec::new().dirs(true)) as Box<dyn CompSpec>,
    );
    map.insert(
      "source".into(),
      Box::new(BashCompSpec::new().files(true)) as Box<dyn CompSpec>,
    );
    map.insert(
      "bg".into(),
      Box::new(BashCompSpec::new().jobs(true)) as Box<dyn CompSpec>,
    );
    map.insert(
      "fg".into(),
      Box::new(BashCompSpec::new().jobs(true)) as Box<dyn CompSpec>,
    );
    map.insert(
      "disown".into(),
      Box::new(BashCompSpec::new().jobs(true)) as Box<dyn CompSpec>,
    );
    map.insert(
      "alias".into(),
      Box::new(BashCompSpec::new().aliases(true)) as Box<dyn CompSpec>,
    );
    map.insert(
      "trap".into(),
      Box::new(BashCompSpec::new().signals(true)) as Box<dyn CompSpec>,
    );

    map
  }
  pub fn cached_cmds(&self) -> &HashSet<String> {
    &self.path_cache
  }
  pub fn cwd_cache(&self) -> &HashSet<String> {
    &self.cwd_cache
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
  pub fn get_cmds_in_path() -> Vec<String> {
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
            cmds.push(name.to_string());
          }
        }
      }
    }
    cmds
  }
  pub fn create_socket(&mut self) -> ShResult<()> {
    let sock = ShedSocket::new()?;
    self.socket = Some(sock.into());
    Ok(())
  }
  pub fn get_socket(&self) -> Option<Arc<ShedSocket>> {
    self.socket.as_ref().cloned()
  }
  pub fn read_socket(&mut self) -> ShResult<()> {
    if let Some(sock) = &self.socket
      && let Ok((conn, _)) = sock.listener().accept()
    {
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
            eprintln!("error reading from message socket: {e}");
            break;
          }
        }
      }
      let input = String::from_utf8_lossy(&bytes).to_string();
      let request = match SocketRequest::from_str(&input) {
        Ok(req) => req,
        Err(e) => {
          write(&conn, format!("error parsing request: {e}\n").as_bytes()).ok();
          return Ok(());
        }
      };

      self.handle_socket_request(conn, request)?;
    }

    Ok(())
  }
  pub fn handle_socket_request(
    &mut self,
    conn: UnixStream,
    request: SocketRequest,
  ) -> ShResult<()> {
    match request {
      SocketRequest::PostSystemMessage(msg) => {
        log::debug!("Posting system message: {}", msg);
        self.post_system_message(msg);
        write(&conn, b"ok\n").ok();
      }
      SocketRequest::PostStatusMessage(msg) => {
        log::debug!("Posting status message: {}", msg);
        self.post_status_message(msg);
        write(&conn, b"ok\n").ok();
      }
      SocketRequest::Subscribe => {
        log::debug!("New subscriber to event stream");
        let conn = Arc::new(conn);
        self.subscribers.push(conn.clone());
      }
      SocketRequest::Query(query_header) => {
        log::debug!("Received query: {:?}", query_header);
        match query_header {
          QueryHeader::Cwd => {
            let cwd = env::current_dir()?.to_string_lossy().to_string();
            write(&conn, cwd.as_bytes()).ok();
            write(&conn, b"\n").ok();
          }
          QueryHeader::Var(var) => {
            let var = read_vars(|v| v.get_var(&var));
            write(&conn, var.as_bytes()).ok();
            write(&conn, b"\n").ok();
          }
          QueryHeader::Status(headers) => {
            let mut responses = vec![];
            for header in headers {
              match header {
                StatusHeader::ExitCode => responses.push(get_status().to_string()),
                StatusHeader::CommandName => {
                  if let Some(job) = self.last_job()
                    && let Some(cmd) = job.name()
                  {
                    responses.push(cmd.to_string());
                  } else {
                    responses.push("".to_string());
                  }
                }
                StatusHeader::Runtime => {
                  let Some(dur) = self.get_time() else {
                    responses.push("".to_string());
                    continue;
                  };
                  responses.push(format!("{}", dur.as_millis()));
                }
                StatusHeader::Pid => {
                  let Some(job) = self.last_job() else {
                    responses.push("".to_string());
                    continue;
                  };
                  responses.push(
                    job
                      .get_pids()
                      .first()
                      .map(|p| p.to_string())
                      .unwrap_or_default(),
                  );
                }
                StatusHeader::Pgid => {
                  let Some(job) = self.last_job() else {
                    responses.push("".to_string());
                    continue;
                  };
                  responses.push(job.pgid().to_string());
                }
              }
            }
            let output = responses.join(" ");
            write(&conn, output.as_bytes()).ok();
            write(&conn, b"\n").ok();
          }
          QueryHeader::Jobs => todo!(),
        }
      }
      SocketRequest::RefreshPrompt => {
        log::debug!("Received prompt refresh request");
        kill(Pid::this(), Signal::SIGUSR1)?;
        write(&conn, b"ok\n").ok();
      }
    }
    Ok(())
  }
  pub fn notify_autocmd(&self, kind: AutoCmdKind) -> ShResult<()> {
    for subscriber in &self.subscribers {
      write(subscriber, format!("autocmd_event>> {kind}\n").as_bytes()).ok();
    }

    Ok(())
  }
  pub fn rehash_commands(&mut self) {
    let path = env::var("PATH").unwrap_or_default();
    let cwd = env::var("PWD").unwrap_or_default();
    log::trace!("Rehashing commands for PATH: '{}' and PWD: '{}'", path, cwd);

    self.path_cache.clear();
    self.old_path = Some(path.clone());
    self.old_pwd = Some(cwd.clone());
    let cmds_in_path = Self::get_cmds_in_path();
    for cmd in cmds_in_path {
      self.path_cache.insert(cmd);
    }
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
          self.path_cache.insert(format!("./{}", name));
        }
      }
    }

    read_logic(|l| {
      let funcs = l.funcs();
      let aliases = l.aliases();
      for func in funcs.keys() {
        self.path_cache.insert(func.clone());
      }
      for alias in aliases.keys() {
        self.path_cache.insert(alias.clone());
      }
    });

    for cmd in BUILTINS {
      self.path_cache.insert(cmd.to_string());
    }
  }
  pub fn try_rehash_commands(&mut self) {
    let path = env::var("PATH").unwrap_or_default();
    let cwd = env::var("PWD").unwrap_or_default();
    if self.old_path.as_ref().is_some_and(|old| *old == path)
      && self.old_pwd.as_ref().is_some_and(|old| *old == cwd)
    {
      log::trace!("PATH and PWD unchanged, skipping rehash");
      return;
    }

    self.rehash_commands();
  }
  pub fn try_rehash_cwd_listing(&mut self) {
    let cwd = env::var("PWD").unwrap_or_default();
    if self.old_pwd.as_ref().is_some_and(|old| *old == cwd) {
      log::trace!("PWD unchanged, skipping rehash of cwd listing");
      return;
    }

    log::debug!("Rehashing cwd listing for PWD: '{}'", cwd);

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
          self.cwd_cache.insert(name.to_string());
        }
      }
    }
  }
  pub fn start_timer(&mut self) {
    self.runtime_start = Some(Instant::now());
  }
  pub fn stop_timer(&mut self) {
    self.runtime_stop = Some(Instant::now());
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
