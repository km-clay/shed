use std::{
  fmt::{Debug, Display},
  io::Write,
  os::{
    fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, RawFd},
    unix::{
      fs::DirBuilderExt,
      net::{UnixListener, UnixStream},
    },
  },
  path::{Path, PathBuf},
  sync::LazyLock,
};

use bstr::ByteSlice;
use itertools::Itertools;
use nix::{
  fcntl::{FcntlArg, fcntl},
  sys::{
    signal::{Signal, kill},
    stat::{self, FchmodatFlags, Mode, fchmodat},
  },
  unistd::{Pid, write},
};

use crate::{
  state::vars::{VarStr, VarStrSliceExt},
  try_var,
  util::{self, ByteCursor, SliceCursor, random},
  varstr,
};

use super::{
  Hint, LineData, Lines, ReadlineEvent, ShResult, Shed, ShedLine, expand_keymap,
  keys::KeyEvent,
  procio::MIN_INTERNAL_FD,
  sherr,
  state::{
    self,
    jobs::Job,
    vars::{VarFlags, VarKind},
  },
  status_msg, system_msg,
  util::{Pos, ShErr},
  var,
};

/// Used to validate requests to the socket's private interface.
///
/// This shall never be inspected or printed anywhere. It should be a secret known
/// only to the process, all of it's threads, and it's child processes.
///
/// Used in operations that are somewhat heavy, such as tab-completion autosuggestions.
/// A thread is dispatched, and then writes it's findings to the socket via the private interface.
/// This allows us to have actual async in a mostly single-threaded program.
static PRIVATE_TOKEN: LazyLock<PrivateToken> = LazyLock::new(PrivateToken::new);

/// Generate a string that can be sent to the socket to authorize a request to the private interface.
pub(crate) fn authorize(request: impl Display) -> String {
  PRIVATE_TOKEN.authorize(request)
}

struct PrivateToken(String);
impl PrivateToken {
  fn new() -> Self {
    Self(random::Uuid::new_v4().to_string())
  }
  fn check(&self, other: &str) -> bool {
    self.0 == other
  }
  fn authorize(&self, request: impl Display) -> String {
    let token = &self.0;
    format!("PRIVATE {token} {request}")
  }
}

/// Write something to the socket as a client.
pub(crate) fn send_to_socket(msg: &str) -> ShResult<()> {
  let Some(path) = try_var!("SHED_SOCK") else {
    return Err(sherr!(
      InternalErr,
      "SHED_SOCK environment variable not set."
    ));
  };
  let mut stream = UnixStream::connect(path)?;

  let mut payload = String::with_capacity(msg.len() + 1);
  payload.push_str(msg);
  payload.push('\n');

  stream.write_all(payload.as_bytes())?;

  Ok(())
}

#[derive(Debug)]
pub(crate) enum StatusHeader {
  ExitCode,
  CommandName,
  Runtime,
  Pid,
  Pgid,
}

#[derive(Debug)]
pub(crate) enum QueryHeader {
  Cwd,
  GetVar(VarStr),
  SetVar(VarStr, VarStr, VarFlags),
  Status(Vec<StatusHeader>),
}

#[derive(Debug)]
pub(crate) enum PrivateHeader {
  /// `(req_gen, token_start, line)`. `token_start` is the byte offset
  /// where the completed token began in the buffer at request time;
  /// the receiver uses it to clear the hint when the user backspaces
  /// past the token boundary.
  SetCompletionHint(u64, usize, VarStr),

  /// Used for reporting errors from child processes.
  PostError(VarStr),
}

#[derive(Debug)]
pub(crate) enum LineHeader {
  Buffer,
  Cursor,
  Hint,
  Mode,
  Anchor,
}

#[derive(Debug)]
pub(crate) enum SocketRequest {
  /// Posts a system message. System messages appear above the prompt, the same way that job status notifications do.
  /// Useful for important information.
  PostSystemMessage(VarStr),
  /// Posts a status message. Status messages appear under the prompt, and are short lived. Will only survive redraws for a few seconds.
  /// Useful for quick notifications.
  PostStatusMessage(VarStr),

  /// Requests information from the shell. The shell will respond with a `SocketResponse` containing the requested information, or an error if the query was invalid.
  Query(QueryHeader),

  /// Opens a subscription to the shell's event stream. The shell will send a `SocketResponse` for each event that occurs, until the socket or connnection is closed.
  Subscribe,

  /// Requests the shell to redraw the prompt. The shell will respond by redrawing the prompt, and sending a `SocketResponse` confirming the redraw.
  RefreshPrompt,

  LineGet(LineHeader),
  LineSet(LineHeader, VarStr),
  LineSendKeys(Vec<KeyEvent>),

  /// Namespace used by internal async stuff.
  /// Allows for multithreading expensive stuff like tab completion autosuggestions.
  /// Background threads report results via the socket using the private namespace.
  Private(PrivateHeader),
}

impl SocketRequest {
  /// Parse a request for the socket's private interface.
  ///
  /// These take the form 'PRIVATE <token> request-kind <payload>'
  /// Intentionally deviates from the public interface's format.
  fn parse_private_request(s: &[u8]) -> Result<Self, ShErr> {
    // if this fails, we write the same error that the normal 'not found' path does
    // basically we're playing dumb instead of telling the user that this interface exists
    let err = Err(sherr!(ParseErr, "Unknown socket request kind: private"));

    let Some((token, rest)) = s.split_once_str(" ") else {
      return err;
    };

    if !PRIVATE_TOKEN.check(&token.to_str_lossy()) {
      log::warn!("Received socket request with invalid private token");
      return err;
    }

    let (kind, payload) = rest
      .trim()
      .split_once_str(" ")
      .unwrap_or((rest.trim(), b""));

    let header = match kind {
      b"set-comp-hint" => {
        let (req_gen_str, rest) = payload.split_once_str(" ").unwrap_or((payload, b""));
        let (token_start_str, line) = rest.split_once_str(" ").unwrap_or((rest, b""));
        let Some(req_gen) = util::parse_bytes::<u64>(req_gen_str) else {
          return err;
        };
        let Some(token_start) = util::parse_bytes::<usize>(token_start_str) else {
          return err;
        };
        PrivateHeader::SetCompletionHint(req_gen, token_start, line.into())
      }
      b"post-error" => PrivateHeader::PostError(payload.into()),
      _ => return err,
    };

    Ok(Self::Private(header))
  }

  #[expect(clippy::too_many_lines)]
  pub(crate) fn parse_request(r: &[u8]) -> ShResult<Self> {
    if let Some(stripped) = r.strip_prefix(b"PRIVATE ") {
      return Self::parse_private_request(stripped);
    }

    let request_kind = r
      .iter()
      .peeking_take_while(|b| b.is_ascii_alphabetic())
      .copied()
      .collect::<Vec<u8>>()
      .to_ascii_lowercase();

    // take care of no-argument requests
    match request_kind.trim() {
      b"subscribe" => return Ok(Self::Subscribe),
      b"redraw" => return Ok(Self::RefreshPrompt),
      _ => {}
    }

    let rest = r[request_kind.len()..].trim();
    let mut sep = util::scratch_buf();
    let mut bytes = SliceCursor::new(rest);

    // collect the separator
    while let Some(b) = bytes.peek_byte() {
      if !b.is_ascii_alphanumeric() && b.is_ascii_graphic() {
        sep.push(b);
        bytes.bump();
      } else {
        break;
      }
    }
    let rest = bytes.into_slice();
    let mut args = rest.split_str(&*sep);

    match request_kind.trim() {
      b"msg" => {
        let Some(msg_kind) = args.next() else {
          return Err(sherr!(ParseErr, "Missing message kind in 'msg' request",));
        };
        match msg_kind.to_ascii_lowercase().as_slice() {
          b"system" => {
            let Some(msg) = args.next() else {
              return Err(sherr!(ParseErr, "Missing message in system msg request",));
            };
            Ok(Self::PostSystemMessage(msg.into()))
          }
          b"status" => {
            let Some(msg) = args.next() else {
              return Err(sherr!(ParseErr, "Missing message in status msg request",));
            };
            Ok(Self::PostStatusMessage(msg.into()))
          }
          _ => Err(sherr!(
            ParseErr,
            "Unknown message kind in 'msg' request: {}",
            msg_kind.to_str_lossy(),
          )),
        }
      }

      b"query" => {
        let Some(query_kind) = args.next() else {
          return Err(sherr!(ParseErr, "Missing query kind in 'query' request",));
        };
        match query_kind.to_ascii_lowercase().as_slice() {
          b"cwd" => Ok(Self::Query(QueryHeader::Cwd)),
          b"status" => {
            let mut headers = vec![];
            while let Some(header) = args.next() {
              let status_header = match header.to_ascii_lowercase().as_slice() {
                b"code" => StatusHeader::ExitCode,
                b"command" => StatusHeader::CommandName,
                b"runtime" => StatusHeader::Runtime,
                b"pid" => StatusHeader::Pid,
                b"pgid" => StatusHeader::Pgid,
                _ => {
                  return Err(sherr!(
                    ParseErr,
                    "Unknown status header in 'query status' request: {}",
                    header.to_str_lossy(),
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
          b"var" => {
            let Some(kind) = args.next() else {
              return Err(sherr!(ParseErr, "Expected 'get' or 'set' in 'var' query",));
            };
            match kind {
              b"get" => {
                let Some(var_name) = args.next() else {
                  return Err(sherr!(
                    ParseErr,
                    "Missing variable name in 'query var get' request",
                  ));
                };
                Ok(Self::Query(QueryHeader::GetVar(var_name.into())))
              }
              b"set" => {
                let Some(var_name) = args.next() else {
                  return Err(sherr!(
                    ParseErr,
                    "Missing variable name in 'query var set' request",
                  ));
                };
                let Some(value) = args.next() else {
                  return Err(sherr!(
                    ParseErr,
                    "Missing variable value in 'query var set' request",
                  ));
                };
                let mut flags = VarFlags::empty();
                while let Some(flag) = args.next() {
                  match flag.to_ascii_lowercase().as_slice() {
                    b"export" => flags |= VarFlags::EXPORT,
                    b"local" => flags |= VarFlags::LOCAL,
                    b"readonly" => flags |= VarFlags::READONLY,
                    _ => {
                      return Err(sherr!(
                        ParseErr,
                        "Unknown variable flag in 'query var set' request: {}",
                        flag.to_str_lossy(),
                      ));
                    }
                  }
                }
                Ok(Self::Query(QueryHeader::SetVar(
                  var_name.into(),
                  value.into(),
                  flags,
                )))
              }
              _ => Err(sherr!(
                ParseErr,
                "Unknown query kind in 'query var' request: {}",
                kind.to_str_lossy(),
              )),
            }
          }
          _ => Err(sherr!(
            ParseErr,
            "Unknown query kind in 'query' request: {}",
            query_kind.to_str_lossy(),
          )),
        }
      }

      b"line" => {
        let Some(header) = args.next() else {
          return Err(sherr!(ParseErr, "Missing line header in 'line' request",));
        };
        match header {
          b"get" => {
            let Some(header2) = args.next() else {
              return Err(sherr!(
                ParseErr,
                "Missing line header kind in 'line get' request",
              ));
            };
            match header2 {
              b"buffer" => Ok(Self::LineGet(LineHeader::Buffer)),
              b"cursor" => Ok(Self::LineGet(LineHeader::Cursor)),
              b"hint" => Ok(Self::LineGet(LineHeader::Hint)),
              b"mode" => Ok(Self::LineGet(LineHeader::Mode)),
              b"anchor" => Ok(Self::LineGet(LineHeader::Anchor)),
              _ => Err(sherr!(
                ParseErr,
                "Unknown line header kind in 'line get' request: {}",
                header2.to_str_lossy()
              )),
            }
          }
          b"set" => {
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
              b"buffer" => Ok(Self::LineSet(LineHeader::Buffer, value.into())),
              b"cursor" => Ok(Self::LineSet(LineHeader::Cursor, value.into())),
              b"hint" => Ok(Self::LineSet(LineHeader::Hint, value.into())),
              b"mode" => Ok(Self::LineSet(LineHeader::Mode, value.into())),
              b"anchor" => Ok(Self::LineSet(LineHeader::Anchor, value.into())),
              _ => Err(sherr!(
                ParseErr,
                "Unknown line header kind in 'line set' request: {}",
                header2.to_str_lossy()
              )),
            }
          }
          b"keys" => {
            let Some(value) = args.next() else {
              return Err(sherr!(ParseErr, "Missing value in 'line keys' request",));
            };
            let events = expand_keymap(&value.to_str_lossy());
            Ok(Self::LineSendKeys(events))
          }
          _ => Err(sherr!(
            ParseErr,
            "Unknown line request kind in 'line' request: {}",
            header.to_str_lossy()
          )),
        }
      }
      _ => Err(sherr!(
        ParseErr,
        "Unknown socket request kind: {}",
        request_kind.to_str_lossy(),
      )),
    }
  }
}

/// The socket used to expose the system/status message interface
#[derive(Debug)]
pub(crate) struct ShedSocket {
  listener: UnixListener,
  pid: Pid,
  path: PathBuf,
}

impl ShedSocket {
  pub fn path() -> String {
    let pid = Pid::this();
    state::util::xdg_runtime_dir()
      .join("shed")
      .join(format!("{pid}.sock"))
      .display()
      .to_string()
  }
  pub fn mode() -> Mode {
    var!("SHED_SOCK_MODE")
      .to_str_lossy()
      .parse::<stat::mode_t>()
      .ok()
      .and_then(Mode::from_bits)
      .unwrap_or(Mode::S_IRUSR | Mode::S_IWUSR)
  }
  pub fn new() -> ShResult<Self> {
    let sock_dir = state::util::xdg_runtime_dir().join("shed");

    std::fs::DirBuilder::new()
      .recursive(true)
      .mode(0o700)
      .create(&sock_dir)?;

    let pid = Pid::this();
    let sock_path = sock_dir.join(format!("{pid}.sock"));
    std::fs::remove_file(&sock_path).ok();

    let listener = UnixListener::bind(&sock_path)?;

    // set the permissions for the socket
    // default is read/write for user, no access for group/other
    // this can be overridden using the $SHED_SOCK_MODE env var.
    let mode = Self::mode();

    fchmodat(
      nix::fcntl::AT_FDCWD,
      Path::new(&sock_path),
      mode,
      FchmodatFlags::FollowSymlink,
    )?;

    let high_fd = {
      let old = listener; // move listener into this lower scope
      fcntl(old, FcntlArg::F_DUPFD_CLOEXEC(MIN_INTERNAL_FD))
    }?; // listener drops here, closes

    // repack the high fd here
    let listener = unsafe { UnixListener::from_raw_fd(high_fd) };
    listener.set_nonblocking(true).ok();

    Shed::vars_mut(|v| {
      v.set_var(
        "SHED_SOCK",
        VarKind::string(sock_path.to_string_lossy().into()),
        VarFlags::EXPORT,
      )
    })
    .ok();
    Ok(Self {
      listener,
      pid: Pid::this(),
      path: sock_path,
    })
  }
  pub fn listener(&self) -> &UnixListener {
    &self.listener
  }
}

impl AsRawFd for ShedSocket {
  fn as_raw_fd(&self) -> RawFd {
    self.listener.as_raw_fd()
  }
}

impl AsFd for ShedSocket {
  fn as_fd(&self) -> BorrowedFd<'_> {
    self.listener.as_fd()
  }
}

impl Drop for ShedSocket {
  fn drop(&mut self) {
    if Pid::this() == self.pid {
      std::fs::remove_file(&self.path).ok();
    }
  }
}

#[expect(clippy::too_many_lines)]
pub(super) fn handle_socket_request(
  conn: UnixStream,
  request: SocketRequest,
  readline: &mut ShedLine,
) -> ShResult<Option<ReadlineEvent>> {
  match request {
    SocketRequest::PostSystemMessage(msg) => {
      system_msg!("{msg}");
      write(&conn, b"ok\n").ok();
    }
    SocketRequest::PostStatusMessage(msg) => {
      status_msg!("{msg}");
      write(&conn, b"ok\n").ok();
    }
    SocketRequest::Subscribe => {
      Shed::push_subscriber(conn);
    }
    SocketRequest::RefreshPrompt => {
      kill(Pid::this(), Signal::SIGUSR1)?;
      write(&conn, b"ok\n").ok();
    }
    SocketRequest::LineGet(line_header) => {
      let LineData {
        buffer,
        cursor,
        anchor,
        hint,
        mode,
      } = readline.get_line_data();
      match line_header {
        LineHeader::Buffer => {
          write(&conn, buffer.as_bytes()).ok();
          write(&conn, b"\n").ok();
        }
        LineHeader::Cursor => {
          write(&conn, cursor.to_string().as_bytes()).ok();
          write(&conn, b"\n").ok();
        }
        LineHeader::Anchor => {
          if let Some(anchor) = anchor {
            write(&conn, anchor.to_string().as_bytes()).ok();
          }
          write(&conn, b"\n").ok();
        }
        LineHeader::Hint => {
          if let Some(hint) = hint {
            write(&conn, hint.as_bytes()).ok();
          }
          write(&conn, b"\n").ok();
        }
        LineHeader::Mode => {
          write(&conn, mode.clone().as_bytes()).ok();
          write(&conn, b"\n").ok();
        }
      }
    }
    SocketRequest::LineSet(line_header, value) => {
      let value = value.to_str_lossy();
      match line_header {
        LineHeader::Buffer => {
          let joined = readline.editor().to_string();
          let pos = readline.editor().cursor_to_flat();

          readline.editor_mut().edit(|this| {
            this.set_buffer(&value);
          });

          readline.history_mut().update_pending_cmd((&joined, pos));

          let hint = readline.history().get_hint();

          readline.editor_mut().set_hint(hint);
          readline.editor_mut().move_cursor_to_end();
          readline.mark_dirty();
        }
        LineHeader::Cursor => readline.editor_mut().with_hint(|this| {
          if let Some((row, col)) = value.split_once(':')
            && let Ok(row) = row.parse::<usize>()
            && let Ok(col) = col.parse::<usize>()
          {
            this.set_cursor(Pos::new(row, col));
          } else if let Ok(pos) = value.parse::<usize>() {
            this.set_cursor_from_flat(pos);
          }
        }),
        LineHeader::Hint => {
          log::debug!("Setting hint from socket: {value}");
          readline
            .editor_mut()
            .set_hint(Some(Hint::Override(Lines::to_lines(&value))));
          readline.mark_dirty();
        }
        LineHeader::Mode => {
          if !readline.try_swap_mode_from_str(&value) {
            return Ok(None);
          }
        }
        LineHeader::Anchor => {
          if let Some((row, col)) = value.split_once(':')
            && let Ok(row) = row.parse::<usize>()
            && let Ok(col) = col.parse::<usize>()
          {
            readline.editor_mut().set_anchor(Pos::new(row, col));
          } else if let Ok(pos) = value.parse::<usize>() {
            readline.editor_mut().set_anchor_from_flat(pos);
          }
        }
      }
    }
    SocketRequest::LineSendKeys(events) => {
      if let Some(event) = readline.replay_keys(events, true)? {
        return Ok(Some(event));
      }
    }
    SocketRequest::Query(query_header) => match query_header {
      QueryHeader::Cwd => {
        let cwd = std::env::current_dir()?.to_string_lossy().to_string();
        write(&conn, cwd.as_bytes()).ok();
        write(&conn, b"\n").ok();
      }
      QueryHeader::GetVar(var) => {
        let var = var!(&var.to_str_lossy());
        write(&conn, var.as_bytes()).ok();
        write(&conn, b"\n").ok();
      }
      QueryHeader::SetVar(var, val, flags) => {
        Shed::vars_mut(|v| v.set_var(&var.to_str_lossy(), VarKind::string(val), flags)).ok();
        write(&conn, b"ok\n").ok();
      }
      QueryHeader::Status(headers) => {
        let mut responses: Vec<VarStr> = vec![];
        for header in headers {
          match header {
            StatusHeader::ExitCode => responses.push(varstr!("{}", Shed::get_status())),
            StatusHeader::CommandName => {
              let Some(name) = Shed::meta(|m| m.last_job().and_then(Job::name)) else {
                responses.push(VarStr::default());
                continue;
              };

              responses.push(name.clone());
            }
            StatusHeader::Runtime => {
              let Some(dur) = Shed::meta_mut(|m| m.get_time()) else {
                responses.push(VarStr::default());
                continue;
              };
              responses.push(varstr!("{}", dur.as_millis()));
            }
            StatusHeader::Pid => {
              let job = Shed::meta_mut(|m| {
                m.last_job().map(|j| {
                  j.get_pids()
                    .first()
                    .map(|pid| varstr!("{pid}"))
                    .unwrap_or_default()
                })
              });
              let Some(job) = job else {
                responses.push(VarStr::default());
                continue;
              };
              responses.push(job);
            }
            StatusHeader::Pgid => {
              let Some(job) = Shed::meta_mut(|m| m.last_job().map(|j| varstr!("{}", j.pgid())))
              else {
                responses.push(VarStr::default());
                continue;
              };
              responses.push(job);
            }
          }
        }
        let output = responses.join_with(" ");
        write(&conn, output.as_bytes()).ok();
        write(&conn, b"\n").ok();
      }
    },

    SocketRequest::Private(req) => match req {
      PrivateHeader::PostError(err) => {
        system_msg!("{err}");
        write(&conn, b"ok\n").ok();
      }
      PrivateHeader::SetCompletionHint(req_gen, token_start, hint) => {
        let cur_gen = readline.worker_req_gen();
        if cur_gen == req_gen && !hint.is_empty() {
          readline.editor_mut().set_hint(Some(Hint::Completion {
            lines: Lines::to_lines(&hint.to_str_lossy()),
            token_start,
          }));
          readline.mark_dirty();
        }
      }
    },
  }
  Ok(None)
}

#[cfg(test)]
mod tests {
  use super::*;

  // ─── No-arg requests ─────────────────────────────────────────────────

  #[test]
  fn subscribe() {
    assert!(matches!(
      SocketRequest::parse_request(b"subscribe").unwrap(),
      SocketRequest::Subscribe
    ));
  }

  #[test]
  fn subscribe_case_insensitive() {
    assert!(matches!(
      SocketRequest::parse_request(b"Subscribe").unwrap(),
      SocketRequest::Subscribe
    ));
    assert!(matches!(
      SocketRequest::parse_request(b"SUBSCRIBE").unwrap(),
      SocketRequest::Subscribe
    ));
  }

  #[test]
  fn redraw() {
    assert!(matches!(
      SocketRequest::parse_request(b"redraw").unwrap(),
      SocketRequest::RefreshPrompt
    ));
  }

  #[test]
  fn redraw_with_trailing_garbage_ignored() {
    // No-arg requests match on the alphabetic prefix only; trailing junk after
    // the keyword shouldn't affect the result for these.
    assert!(matches!(
      SocketRequest::parse_request(b"redraw:ignored").unwrap(),
      SocketRequest::RefreshPrompt
    ));
  }

  // ─── Separator detection ─────────────────────────────────────────────

  #[test]
  fn separator_colon() {
    let req = SocketRequest::parse_request(b"msg:system:hello").unwrap();
    let SocketRequest::PostSystemMessage(s) = req else {
      panic!()
    };
    assert_eq!(s, "hello");
  }

  #[test]
  fn separator_slash() {
    let req = SocketRequest::parse_request(b"msg/system/hello").unwrap();
    let SocketRequest::PostSystemMessage(s) = req else {
      panic!()
    };
    assert_eq!(s, "hello");
  }

  #[test]
  fn separator_pipe() {
    let req = SocketRequest::parse_request(b"msg|system|hello").unwrap();
    let SocketRequest::PostSystemMessage(s) = req else {
      panic!()
    };
    assert_eq!(s, "hello");
  }

  #[test]
  fn separator_multichar() {
    // The separator-collection loop grabs a contiguous run of non-alphanumeric
    // graphic chars, so "::" and ":::" both work as long as they're consistent.
    let req = SocketRequest::parse_request(b"msg::system::hello").unwrap();
    let SocketRequest::PostSystemMessage(s) = req else {
      panic!()
    };
    assert_eq!(s, "hello");
  }

  // ─── msg ─────────────────────────────────────────────────────────────

  #[test]
  fn msg_system() {
    let req = SocketRequest::parse_request(b"msg:system:notice").unwrap();
    let SocketRequest::PostSystemMessage(s) = req else {
      panic!()
    };
    assert_eq!(s, "notice");
  }

  #[test]
  fn msg_status() {
    let req = SocketRequest::parse_request(b"msg:status:notice").unwrap();
    let SocketRequest::PostStatusMessage(s) = req else {
      panic!()
    };
    assert_eq!(s, "notice");
  }

  #[test]
  fn msg_with_spaces_in_body() {
    let req = SocketRequest::parse_request(b"msg:system:hello world").unwrap();
    let SocketRequest::PostSystemMessage(s) = req else {
      panic!()
    };
    assert_eq!(s, "hello world");
  }

  #[test]
  fn msg_missing_kind() {
    assert!(SocketRequest::parse_request(b"msg").is_err());
  }

  #[test]
  fn msg_missing_body() {
    assert!(SocketRequest::parse_request(b"msg:system").is_err());
  }

  #[test]
  fn msg_unknown_kind() {
    assert!(SocketRequest::parse_request(b"msg:loud:hi").is_err());
  }

  // ─── query ───────────────────────────────────────────────────────────

  #[test]
  fn query_cwd() {
    assert!(matches!(
      SocketRequest::parse_request(b"query:cwd").unwrap(),
      SocketRequest::Query(QueryHeader::Cwd)
    ));
  }

  #[test]
  fn query_status_defaults_when_no_headers() {
    let req = SocketRequest::parse_request(b"query:status").unwrap();
    let SocketRequest::Query(QueryHeader::Status(headers)) = req else {
      panic!()
    };
    assert_eq!(headers.len(), 5);
    assert!(matches!(headers[0], StatusHeader::ExitCode));
    assert!(matches!(headers[1], StatusHeader::CommandName));
    assert!(matches!(headers[2], StatusHeader::Runtime));
    assert!(matches!(headers[3], StatusHeader::Pid));
    assert!(matches!(headers[4], StatusHeader::Pgid));
  }

  #[test]
  fn query_status_single_header() {
    let req = SocketRequest::parse_request(b"query:status:code").unwrap();
    let SocketRequest::Query(QueryHeader::Status(headers)) = req else {
      panic!()
    };
    assert_eq!(headers.len(), 1);
    assert!(matches!(headers[0], StatusHeader::ExitCode));
  }

  #[test]
  fn query_status_multiple_headers() {
    let req = SocketRequest::parse_request(b"query:status:pid:pgid").unwrap();
    let SocketRequest::Query(QueryHeader::Status(headers)) = req else {
      panic!()
    };
    assert_eq!(headers.len(), 2);
    assert!(matches!(headers[0], StatusHeader::Pid));
    assert!(matches!(headers[1], StatusHeader::Pgid));
  }

  #[test]
  fn query_status_unknown_header() {
    assert!(SocketRequest::parse_request(b"query:status:bogus").is_err());
  }

  #[test]
  fn query_var_get() {
    let req = SocketRequest::parse_request(b"query:var:get:PATH").unwrap();
    let SocketRequest::Query(QueryHeader::GetVar(name)) = req else {
      panic!()
    };
    assert_eq!(name, "PATH");
  }

  #[test]
  fn query_var_set_no_flags() {
    let req = SocketRequest::parse_request(b"query:var:set:FOO:bar").unwrap();
    let SocketRequest::Query(QueryHeader::SetVar(name, val, flags)) = req else {
      panic!()
    };
    assert_eq!(name, "FOO");
    assert_eq!(val, "bar");
    assert!(flags.is_empty());
  }

  #[test]
  fn query_var_set_with_export_flag() {
    let req = SocketRequest::parse_request(b"query:var:set:FOO:bar:export").unwrap();
    let SocketRequest::Query(QueryHeader::SetVar(_, _, flags)) = req else {
      panic!()
    };
    assert!(flags.contains(VarFlags::EXPORT));
  }

  #[test]
  fn query_var_set_with_multiple_flags() {
    let req = SocketRequest::parse_request(b"query:var:set:FOO:bar:export:readonly").unwrap();
    let SocketRequest::Query(QueryHeader::SetVar(_, _, flags)) = req else {
      panic!()
    };
    assert!(flags.contains(VarFlags::EXPORT));
    assert!(flags.contains(VarFlags::READONLY));
  }

  #[test]
  fn query_var_set_with_unknown_flag() {
    assert!(SocketRequest::parse_request(b"query:var:set:FOO:bar:invalid").is_err());
  }

  #[test]
  fn query_var_get_missing_name() {
    assert!(SocketRequest::parse_request(b"query:var:get").is_err());
  }

  #[test]
  fn query_var_set_missing_value() {
    assert!(SocketRequest::parse_request(b"query:var:set:FOO").is_err());
  }

  #[test]
  fn query_var_unknown_subkind() {
    assert!(SocketRequest::parse_request(b"query:var:bogus:FOO").is_err());
  }

  #[test]
  fn query_unknown_kind() {
    assert!(SocketRequest::parse_request(b"query:bogus").is_err());
  }

  #[test]
  fn query_missing_kind() {
    assert!(SocketRequest::parse_request(b"query").is_err());
  }

  // ─── line ────────────────────────────────────────────────────────────

  #[test]
  fn line_get_buffer() {
    assert!(matches!(
      SocketRequest::parse_request(b"line:get:buffer").unwrap(),
      SocketRequest::LineGet(LineHeader::Buffer)
    ));
  }

  #[test]
  fn line_get_cursor() {
    assert!(matches!(
      SocketRequest::parse_request(b"line:get:cursor").unwrap(),
      SocketRequest::LineGet(LineHeader::Cursor)
    ));
  }

  #[test]
  fn line_get_hint() {
    assert!(matches!(
      SocketRequest::parse_request(b"line:get:hint").unwrap(),
      SocketRequest::LineGet(LineHeader::Hint)
    ));
  }

  #[test]
  fn line_get_mode() {
    assert!(matches!(
      SocketRequest::parse_request(b"line:get:mode").unwrap(),
      SocketRequest::LineGet(LineHeader::Mode)
    ));
  }

  #[test]
  fn line_get_anchor() {
    assert!(matches!(
      SocketRequest::parse_request(b"line:get:anchor").unwrap(),
      SocketRequest::LineGet(LineHeader::Anchor)
    ));
  }

  #[test]
  fn line_get_unknown_header() {
    assert!(SocketRequest::parse_request(b"line:get:bogus").is_err());
  }

  #[test]
  fn line_get_missing_header() {
    assert!(SocketRequest::parse_request(b"line:get").is_err());
  }

  #[test]
  fn line_set_buffer() {
    let req = SocketRequest::parse_request(b"line:set:buffer:hello").unwrap();
    let SocketRequest::LineSet(LineHeader::Buffer, val) = req else {
      panic!()
    };
    assert_eq!(val, "hello");
  }

  #[test]
  fn line_set_cursor() {
    let req = SocketRequest::parse_request(b"line:set:cursor:5").unwrap();
    let SocketRequest::LineSet(LineHeader::Cursor, val) = req else {
      panic!()
    };
    assert_eq!(val, "5");
  }

  #[test]
  fn line_set_unknown_header() {
    assert!(SocketRequest::parse_request(b"line:set:bogus:val").is_err());
  }

  #[test]
  fn line_set_missing_value() {
    assert!(SocketRequest::parse_request(b"line:set:buffer").is_err());
  }

  #[test]
  fn line_keys() {
    let req = SocketRequest::parse_request(b"line:keys:foo").unwrap();
    let SocketRequest::LineSendKeys(events) = req else {
      panic!()
    };
    // expand_keymap("foo") should produce some events; just confirm we got a vec
    assert!(!events.is_empty());
  }

  #[test]
  fn line_keys_missing_value() {
    assert!(SocketRequest::parse_request(b"line:keys").is_err());
  }

  #[test]
  fn line_unknown_subkind() {
    assert!(SocketRequest::parse_request(b"line:bogus:foo").is_err());
  }

  #[test]
  fn line_missing_header() {
    assert!(SocketRequest::parse_request(b"line").is_err());
  }

  // ─── Top-level errors ────────────────────────────────────────────────

  #[test]
  fn unknown_request_kind() {
    assert!(SocketRequest::parse_request(b"notarequest:foo").is_err());
  }

  #[test]
  fn empty_input() {
    assert!(SocketRequest::parse_request(b"").is_err());
  }

  #[test]
  fn only_whitespace() {
    assert!(SocketRequest::parse_request(b"   ").is_err());
  }

  // ─── handle_socket_request ───────────────────────────────────────────
  //
  // These tests exercise the side of the socket protocol that the
  // parser tests above don't reach: what shed actually *does* once a
  // request is decoded.

  use std::io::Read;
  use std::time::Duration;

  use crate::Prompt;
  use crate::tests::testutil::TestGuard;

  /// Run a request against a fresh `ShedLine` and return the bytes the
  /// handler wrote to its end of the socket. For requests that don't
  /// store the conn (everything except Subscribe), tx is dropped on
  /// function return so the read terminates cleanly on EOF.
  fn run_handler(
    req: SocketRequest,
    readline: &mut ShedLine,
  ) -> (Vec<u8>, ShResult<Option<ReadlineEvent>>) {
    let (tx, mut rx) = UnixStream::pair().unwrap();
    rx.set_read_timeout(Some(Duration::from_millis(200))).ok();
    let result = handle_socket_request(tx, req, readline);
    let mut buf = Vec::new();
    rx.read_to_end(&mut buf).ok();
    (buf, result)
  }

  fn fresh_readline() -> ShedLine {
    ShedLine::new_no_hist(Prompt::default()).unwrap()
  }

  // PostSystemMessage / PostStatusMessage

  #[test]
  fn handler_post_system_message_writes_ok() {
    let _g = TestGuard::new();
    let mut rl = fresh_readline();
    let (resp, _) = run_handler(SocketRequest::PostSystemMessage("hi".into()), &mut rl);
    assert_eq!(resp, b"ok\n");
  }

  #[test]
  fn handler_post_system_message_queues_msg() {
    let g = TestGuard::new();
    let mut rl = fresh_readline();
    let (_, _) = run_handler(SocketRequest::PostSystemMessage("queued".into()), &mut rl);
    let out = g.read_output();
    assert!(out.contains("queued"), "got: {out:?}");
  }

  #[test]
  fn handler_post_status_message_writes_ok() {
    let _g = TestGuard::new();
    let mut rl = fresh_readline();
    let (resp, _) = run_handler(SocketRequest::PostStatusMessage("hi".into()), &mut rl);
    assert_eq!(resp, b"ok\n");
  }

  #[test]
  fn handler_post_status_message_queues_msg() {
    let _g = TestGuard::new();
    let mut rl = fresh_readline();
    let (_, _) = run_handler(SocketRequest::PostStatusMessage("queued".into()), &mut rl);
    let popped = Shed::pop_status_msg();
    assert!(popped.is_some());
    assert!(popped.unwrap().contains("queued"));
  }

  // LineGet — verifies handler reads ShedLine state back to the socket

  #[test]
  fn handler_line_get_buffer_returns_current_buffer() {
    let _g = TestGuard::new();
    let mut rl = fresh_readline();
    rl.editor_mut().edit(|e| e.set_buffer("hello world"));
    let (resp, _) = run_handler(SocketRequest::LineGet(LineHeader::Buffer), &mut rl);
    assert_eq!(resp, b"hello world\n");
  }

  #[test]
  fn handler_line_get_buffer_empty() {
    let _g = TestGuard::new();
    let mut rl = fresh_readline();
    let (resp, _) = run_handler(SocketRequest::LineGet(LineHeader::Buffer), &mut rl);
    assert_eq!(resp, b"\n");
  }

  #[test]
  fn handler_line_get_mode_returns_mode_string() {
    let _g = TestGuard::new();
    let mut rl = fresh_readline();
    let (resp, _) = run_handler(SocketRequest::LineGet(LineHeader::Mode), &mut rl);
    // exact string depends on default mode; just check it's non-empty
    // and ends with a newline.
    assert!(resp.ends_with(b"\n"));
    assert!(resp.len() > 1);
  }

  // LineSet — verifies handler mutates ShedLine state from the socket

  #[test]
  fn handler_line_set_buffer_replaces_buffer() {
    let _g = TestGuard::new();
    let mut rl = fresh_readline();
    let (_, _) = run_handler(
      SocketRequest::LineSet(LineHeader::Buffer, "set-from-socket".into()),
      &mut rl,
    );
    assert_eq!(rl.editor().to_string(), "set-from-socket");
  }

  #[test]
  fn handler_line_set_buffer_round_trip_via_get() {
    let _g = TestGuard::new();
    let mut rl = fresh_readline();
    let _ = run_handler(
      SocketRequest::LineSet(LineHeader::Buffer, "abc".into()),
      &mut rl,
    );
    let (resp, _) = run_handler(SocketRequest::LineGet(LineHeader::Buffer), &mut rl);
    assert_eq!(resp, b"abc\n");
  }

  // Query: Cwd / GetVar / SetVar / Status

  #[test]
  fn handler_query_cwd_returns_current_dir() {
    let _g = TestGuard::new();
    let mut rl = fresh_readline();
    let (resp, _) = run_handler(SocketRequest::Query(QueryHeader::Cwd), &mut rl);
    let expected = std::env::current_dir()
      .unwrap()
      .to_string_lossy()
      .to_string();
    assert_eq!(resp, format!("{expected}\n").as_bytes());
  }

  #[test]
  fn handler_query_get_var_returns_value() {
    let _g = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("XYZZY", VarKind::Str("magic".into()), VarFlags::empty()))
      .unwrap();
    let mut rl = fresh_readline();
    let (resp, _) = run_handler(
      SocketRequest::Query(QueryHeader::GetVar("XYZZY".into())),
      &mut rl,
    );
    assert_eq!(resp, b"magic\n");
  }

  #[test]
  fn handler_query_get_var_unknown_returns_blank_line() {
    let _g = TestGuard::new();
    let mut rl = fresh_readline();
    let (resp, _) = run_handler(
      SocketRequest::Query(QueryHeader::GetVar("DEFINITELY_NOT_SET_zzz".into())),
      &mut rl,
    );
    assert_eq!(resp, b"\n");
  }

  #[test]
  fn handler_query_set_var_persists_and_ok() {
    let _g = TestGuard::new();
    let mut rl = fresh_readline();
    let (resp, _) = run_handler(
      SocketRequest::Query(QueryHeader::SetVar(
        "VIA_SOCKET".into(),
        "value-set".into(),
        VarFlags::empty(),
      )),
      &mut rl,
    );
    assert_eq!(resp, b"ok\n");
    assert_eq!(var!("VIA_SOCKET"), "value-set");
  }

  #[test]
  fn handler_query_status_exit_code() {
    let _g = TestGuard::new();
    Shed::set_status(42);
    let mut rl = fresh_readline();
    let (resp, _) = run_handler(
      SocketRequest::Query(QueryHeader::Status(vec![StatusHeader::ExitCode])),
      &mut rl,
    );
    assert_eq!(resp, b"42\n");
  }

  #[test]
  fn handler_query_status_multiple_headers_joined_with_space() {
    let _g = TestGuard::new();
    Shed::set_status(7);
    let mut rl = fresh_readline();
    let (resp, _) = run_handler(
      SocketRequest::Query(QueryHeader::Status(vec![
        StatusHeader::ExitCode,
        StatusHeader::CommandName,
      ])),
      &mut rl,
    );
    // ExitCode="7", CommandName="" (no last_job) → "7 \n"
    assert_eq!(resp, b"7 \n");
  }

  // LineGet — Cursor / Anchor / Hint

  #[test]
  fn handler_line_get_cursor_returns_flat_position() {
    let _g = TestGuard::new();
    let mut rl = fresh_readline();
    rl.editor_mut().edit(|e| e.set_buffer("abcdef"));
    rl.editor_mut().set_cursor_from_flat(3);
    let (resp, _) = run_handler(SocketRequest::LineGet(LineHeader::Cursor), &mut rl);
    assert_eq!(resp, b"3\n");
  }

  #[test]
  fn handler_line_get_anchor_returns_blank_when_unset() {
    let _g = TestGuard::new();
    let mut rl = fresh_readline();
    let (resp, _) = run_handler(SocketRequest::LineGet(LineHeader::Anchor), &mut rl);
    assert_eq!(resp, b"\n");
  }

  #[test]
  fn handler_line_get_anchor_returns_set_position() {
    let _g = TestGuard::new();
    let mut rl = fresh_readline();
    rl.editor_mut().edit(|e| {
      e.set_buffer("abcdef");
      e.start_char_select(); // anchor requires an active select mode
    });
    rl.editor_mut().set_anchor_from_flat(2);
    let (resp, _) = run_handler(SocketRequest::LineGet(LineHeader::Anchor), &mut rl);
    assert_eq!(resp, b"2\n");
  }

  #[test]
  fn handler_line_get_hint_returns_blank_when_unset() {
    let _g = TestGuard::new();
    let mut rl = fresh_readline();
    let (resp, _) = run_handler(SocketRequest::LineGet(LineHeader::Hint), &mut rl);
    assert_eq!(resp, b"\n");
  }

  #[test]
  fn handler_line_get_hint_returns_hint_text() {
    let _g = TestGuard::new();
    let mut rl = fresh_readline();
    rl.editor_mut()
      .set_hint(Some(Hint::Override(Lines::to_lines("suggestion"))));
    let (resp, _) = run_handler(SocketRequest::LineGet(LineHeader::Hint), &mut rl);
    assert_eq!(resp, b"suggestion\n");
  }

  #[test]
  fn handler_line_set_hint_applies_even_with_empty_buffer() {
    // Override hints bypass the empty-buffer gate that suppresses
    // auto-suggested (History) hints.
    let _g = TestGuard::new();
    let mut rl = fresh_readline();
    let _ = run_handler(
      SocketRequest::LineSet(LineHeader::Hint, "external".into()),
      &mut rl,
    );
    let (resp, _) = run_handler(SocketRequest::LineGet(LineHeader::Hint), &mut rl);
    assert_eq!(resp, b"external\n");
  }

  #[test]
  fn handler_line_set_hint_applies_when_auto_suggest_disabled() {
    // Socket-driven hints are explicit, so the auto_suggest shopt
    // shouldn't affect them.
    let _g = TestGuard::new();
    Shed::shopts_mut(|o| o.line.auto_suggest = false);
    let mut rl = fresh_readline();
    rl.editor_mut().edit(|e| e.set_buffer("ab"));
    let _ = run_handler(
      SocketRequest::LineSet(LineHeader::Hint, "external".into()),
      &mut rl,
    );
    let (resp, _) = run_handler(SocketRequest::LineGet(LineHeader::Hint), &mut rl);
    assert_eq!(resp, b"external\n");
  }

  // LineSet — Cursor / Anchor / Hint / Mode

  #[test]
  fn handler_line_set_cursor_flat_position() {
    let _g = TestGuard::new();
    let mut rl = fresh_readline();
    rl.editor_mut().edit(|e| e.set_buffer("abcdef"));
    let _ = run_handler(
      SocketRequest::LineSet(LineHeader::Cursor, "4".into()),
      &mut rl,
    );
    assert_eq!(rl.editor().cursor_to_flat(), 4);
  }

  #[test]
  fn handler_line_set_cursor_row_col() {
    let _g = TestGuard::new();
    let mut rl = fresh_readline();
    rl.editor_mut().edit(|e| e.set_buffer("ab\ncd\nef"));
    let _ = run_handler(
      SocketRequest::LineSet(LineHeader::Cursor, "1:1".into()),
      &mut rl,
    );
    // row 1 col 1 in "ab\ncd\nef" → flat position 4 ('d' if 0-indexed past 'c')
    // Just verify cursor moved off origin; exact mapping depends on grapheme accounting.
    assert_ne!(rl.editor().cursor_to_flat(), 0);
  }

  #[test]
  fn handler_line_set_cursor_garbage_value_is_noop() {
    let _g = TestGuard::new();
    let mut rl = fresh_readline();
    rl.editor_mut().edit(|e| e.set_buffer("abcdef"));
    rl.editor_mut().set_cursor_from_flat(2);
    let _ = run_handler(
      SocketRequest::LineSet(LineHeader::Cursor, "not-a-number".into()),
      &mut rl,
    );
    assert_eq!(rl.editor().cursor_to_flat(), 2);
  }

  #[test]
  fn handler_line_set_anchor_flat_position() {
    let _g = TestGuard::new();
    let mut rl = fresh_readline();
    rl.editor_mut().edit(|e| {
      e.set_buffer("abcdef");
      e.start_char_select(); // anchor requires an active select mode
    });
    let _ = run_handler(
      SocketRequest::LineSet(LineHeader::Anchor, "3".into()),
      &mut rl,
    );
    assert_eq!(rl.editor().anchor_to_flat(), Some(3));
  }

  #[test]
  fn handler_line_set_hint_round_trip() {
    let _g = TestGuard::new();
    let mut rl = fresh_readline();
    let _ = run_handler(
      SocketRequest::LineSet(LineHeader::Hint, "hinttext".into()),
      &mut rl,
    );
    let (resp, _) = run_handler(SocketRequest::LineGet(LineHeader::Hint), &mut rl);
    assert_eq!(resp, b"hinttext\n");
  }

  #[test]
  fn handler_line_set_mode_unknown_returns_early_with_none() {
    let _g = TestGuard::new();
    let mut rl = fresh_readline();
    let (_, result) = run_handler(
      SocketRequest::LineSet(LineHeader::Mode, "not-a-real-mode".into()),
      &mut rl,
    );
    // Unknown mode causes the handler to return Ok(None) early via the
    // try_swap_mode_from_str false branch.
    assert!(matches!(result, Ok(None)));
  }

  // LineSendKeys

  #[test]
  fn handler_line_send_keys_types_into_buffer() {
    let _g = TestGuard::new();
    let mut rl = fresh_readline();
    let keys = vec![
      KeyEvent(
        crate::keys::KeyCode::Char('h'),
        crate::keys::ModKeys::empty(),
      ),
      KeyEvent(
        crate::keys::KeyCode::Char('i'),
        crate::keys::ModKeys::empty(),
      ),
    ];
    let (_, result) = run_handler(SocketRequest::LineSendKeys(keys), &mut rl);
    // Plain chars don't emit a ReadlineEvent — they just modify the buffer.
    assert!(matches!(result, Ok(None)));
    assert_eq!(rl.editor().to_string(), "hi");
  }

  // Query::Status — branches that need a last_job

  fn make_last_job(cmd: &str) -> crate::state::jobs::Job {
    use crate::state::jobs::{ChildProc, JobBldr};
    use nix::unistd::Pid;
    let mut bldr = JobBldr::new();
    bldr.set_pgid(Pid::this());
    let child = ChildProc::new(Pid::this(), Some(cmd.as_bytes()), Some(Pid::this()), None);
    bldr.push_child(child);
    bldr.build()
  }

  #[test]
  fn handler_query_status_command_name_with_job() {
    let _g = TestGuard::new();
    Shed::meta_mut(|m| m.set_last_job(Some(make_last_job("mycmd"))));
    let mut rl = fresh_readline();
    let (resp, _) = run_handler(
      SocketRequest::Query(QueryHeader::Status(vec![StatusHeader::CommandName])),
      &mut rl,
    );
    assert_eq!(resp, b"mycmd\n");
  }

  #[test]
  fn handler_query_status_command_name_without_job() {
    let _g = TestGuard::new();
    let mut rl = fresh_readline();
    let (resp, _) = run_handler(
      SocketRequest::Query(QueryHeader::Status(vec![StatusHeader::CommandName])),
      &mut rl,
    );
    assert_eq!(resp, b"\n");
  }

  #[test]
  fn handler_query_status_pid_with_job() {
    let _g = TestGuard::new();
    Shed::meta_mut(|m| m.set_last_job(Some(make_last_job("c"))));
    let mut rl = fresh_readline();
    let (resp, _) = run_handler(
      SocketRequest::Query(QueryHeader::Status(vec![StatusHeader::Pid])),
      &mut rl,
    );
    let expected = format!("{}\n", nix::unistd::Pid::this());
    assert_eq!(resp, expected.as_bytes());
  }

  #[test]
  fn handler_query_status_pid_without_job() {
    let _g = TestGuard::new();
    let mut rl = fresh_readline();
    let (resp, _) = run_handler(
      SocketRequest::Query(QueryHeader::Status(vec![StatusHeader::Pid])),
      &mut rl,
    );
    assert_eq!(resp, b"\n");
  }

  #[test]
  fn handler_query_status_pgid_with_job() {
    let _g = TestGuard::new();
    Shed::meta_mut(|m| m.set_last_job(Some(make_last_job("c"))));
    let mut rl = fresh_readline();
    let (resp, _) = run_handler(
      SocketRequest::Query(QueryHeader::Status(vec![StatusHeader::Pgid])),
      &mut rl,
    );
    let expected = format!("{}\n", nix::unistd::Pid::this());
    assert_eq!(resp, expected.as_bytes());
  }

  #[test]
  fn handler_query_status_runtime_empty_when_no_timing() {
    let _g = TestGuard::new();
    let mut rl = fresh_readline();
    let (resp, _) = run_handler(
      SocketRequest::Query(QueryHeader::Status(vec![StatusHeader::Runtime])),
      &mut rl,
    );
    assert_eq!(resp, b"\n");
  }
}
