use std::fmt::Display;

use crate::{
  libsh::term::{Style, Styled},
  parse::lex::Span,
  prelude::*,
};

pub type ShResult<T> = Result<T, ShErr>;

pub trait ShResultExt {
  fn blame(self, span: Span) -> Self;
  fn try_blame(self, span: Span) -> Self;
}

impl<T> ShResultExt for Result<T, ShErr> {
  /// Blame a span for an error
  fn blame(self, new_span: Span) -> Self {
    let Err(e) = self else { return self };
    match e {
      ShErr::Simple { kind, msg, notes }
      | ShErr::Full {
        kind,
        msg,
        notes,
        span: _,
      } => Err(ShErr::Full {
        kind: kind.clone(),
        msg: msg.clone(),
        notes: notes.clone(),
        span: new_span,
      }),
    }
  }
  /// Blame a span if no blame has been assigned yet
  fn try_blame(self, new_span: Span) -> Self {
    let Err(e) = &self else { return self };
    match e {
      ShErr::Simple { kind, msg, notes } => Err(ShErr::Full {
        kind: kind.clone(),
        msg: msg.clone(),
        notes: notes.clone(),
        span: new_span,
      }),
      ShErr::Full {
        kind: _,
        msg: _,
        span: _,
        notes: _,
      } => self,
    }
  }
}

#[derive(Clone, Debug)]
pub struct Note {
  main: String,
  sub_notes: Vec<Note>,
  depth: usize,
}

impl Note {
  pub fn new(main: impl Into<String>) -> Self {
    Self {
      main: main.into(),
      sub_notes: vec![],
      depth: 0,
    }
  }

  pub fn with_sub_notes(self, new_sub_notes: Vec<impl Into<String>>) -> Self {
    let Self {
      main,
      mut sub_notes,
      depth,
    } = self;
    for raw_note in new_sub_notes {
      let mut note = Note::new(raw_note);
      note.depth = self.depth + 1;
      sub_notes.push(note);
    }
    Self {
      main,
      sub_notes,
      depth,
    }
  }
}

impl Display for Note {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let note = "note".styled(Style::Green);
    let main = &self.main;
    if self.depth == 0 {
      writeln!(f, "{note}: {main}")?;
    } else {
      let bar_break = "-".styled(Style::Cyan | Style::Bold);
      let indent = "  ".repeat(self.depth);
      writeln!(f, "  {indent}{bar_break} {main}")?;
    }

    for sub_note in &self.sub_notes {
      write!(f, "{sub_note}")?;
    }
    Ok(())
  }
}

#[derive(Debug)]
pub enum ShErr {
  Simple {
    kind: ShErrKind,
    msg: String,
    notes: Vec<Note>,
  },
  Full {
    kind: ShErrKind,
    msg: String,
    notes: Vec<Note>,
    span: Span,
  },
}

impl ShErr {
  pub fn simple(kind: ShErrKind, msg: impl Into<String>) -> Self {
    let msg = msg.into();
    Self::Simple {
      kind,
      msg,
      notes: vec![],
    }
  }
  pub fn full(kind: ShErrKind, msg: impl Into<String>, span: Span) -> Self {
    let msg = msg.into();
    Self::Full {
      kind,
      msg,
      span,
      notes: vec![],
    }
  }
  pub fn unpack(self) -> (ShErrKind, String, Vec<Note>, Option<Span>) {
    match self {
      ShErr::Simple { kind, msg, notes } => (kind, msg, notes, None),
      ShErr::Full {
        kind,
        msg,
        notes,
        span,
      } => (kind, msg, notes, Some(span)),
    }
  }
  pub fn with_note(self, note: Note) -> Self {
    let (kind, msg, mut notes, span) = self.unpack();
    notes.push(note);
    if let Some(span) = span {
      Self::Full {
        kind,
        msg,
        notes,
        span,
      }
    } else {
      Self::Simple { kind, msg, notes }
    }
  }
  pub fn with_span(sherr: ShErr, span: Span) -> Self {
    let (kind, msg, notes, _) = sherr.unpack();
    Self::Full {
      kind,
      msg,
      notes,
      span,
    }
  }
  pub fn kind(&self) -> &ShErrKind {
    match self {
      ShErr::Simple {
        kind,
        msg: _,
        notes: _,
      }
      | ShErr::Full {
        kind,
        msg: _,
        notes: _,
        span: _,
      } => kind,
    }
  }
  pub fn get_window(&self) -> Vec<(usize, String)> {
    let ShErr::Full {
      kind: _,
      msg: _,
      notes: _,
      span,
    } = self
    else {
      unreachable!()
    };
    let mut total_len: usize = 0;
    let mut total_lines: usize = 1;
    let mut lines = vec![];
    let mut cur_line = String::new();

    let src = span.get_source();
    let mut chars = src.chars();

    while let Some(ch) = chars.next() {
      total_len += ch.len_utf8();
      cur_line.push(ch);
      if ch == '\n' {
        if total_len > span.start {
          let line = (total_lines, mem::take(&mut cur_line));
          lines.push(line);
        }
        if total_len >= span.end {
          break;
        }
        total_lines += 1;

        cur_line.clear();
      }
    }

    if !cur_line.is_empty() {
      let line = (total_lines, mem::take(&mut cur_line));
      lines.push(line);
    }

    lines
  }
  pub fn get_line_col(&self) -> (usize, usize) {
    let ShErr::Full {
      kind: _,
      msg: _,
      notes: _,
      span,
    } = self
    else {
      unreachable!()
    };

    let mut lineno = 1;
    let mut colno = 1;
    let src = span.get_source();
    let mut chars = src.chars().enumerate();
    while let Some((pos, ch)) = chars.next() {
      if pos >= span.start {
        break;
      }
      if ch == '\n' {
        lineno += 1;
        colno = 1;
      } else {
        colno += 1;
      }
    }
    (lineno, colno)
  }
  pub fn get_indicator_lines(&self) -> Option<Vec<String>> {
    match self {
      ShErr::Simple {
        kind: _,
        msg: _,
        notes: _,
      } => None,
      ShErr::Full {
        kind: _,
        msg: _,
        notes: _,
        span,
      } => {
        let text = span.as_str();
        let lines = text.lines();
        let mut indicator_lines = vec![];

        for line in lines {
          let indicator_line = "^"
            .repeat(line.trim().len())
            .styled(Style::Red | Style::Bold);
          indicator_lines.push(indicator_line);
        }

        Some(indicator_lines)
      }
    }
  }
}

impl Display for ShErr {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Simple {
        msg,
        kind: _,
        notes,
      } => {
        let mut all_strings = vec![msg.to_string()];
        let mut notes_fmt = vec![];
        for note in notes {
          let fmt = format!("{note}");
          notes_fmt.push(fmt);
        }
        all_strings.append(&mut notes_fmt);
        let mut output = all_strings.join("\n");
        output.push('\n');

        writeln!(f, "{}", output)
      }

      Self::Full {
        msg,
        kind,
        notes,
        span: _,
      } => {
        let window = self.get_window();
        let mut indicator_lines = self.get_indicator_lines().unwrap().into_iter();
        let mut lineno_pad_count = 0;
        for (lineno, _) in window.clone() {
          if lineno.to_string().len() > lineno_pad_count {
            lineno_pad_count = lineno.to_string().len() + 1
          }
        }
        let padding = " ".repeat(lineno_pad_count);
        writeln!(f)?;

        let (line, col) = self.get_line_col();
        let line_fmt = line.styled(Style::Cyan | Style::Bold);
        let col_fmt = col.styled(Style::Cyan | Style::Bold);
        let kind = kind.styled(Style::Red | Style::Bold);
        let arrow = "->".styled(Style::Cyan | Style::Bold);
        writeln!(f, "{kind} - {msg}",)?;
        writeln!(f, "{padding}{arrow} [{line_fmt};{col_fmt}]",)?;

        let bar = format!("{padding}|").styled(Style::Cyan | Style::Bold);
        writeln!(f, "{bar}")?;

        let mut first_ind_ln = true;
        for (lineno, line) in window {
          let lineno = lineno.to_string();
          let line = line.trim();
          let mut prefix = format!("{padding}|");
          prefix.replace_range(0..lineno.len(), &lineno);
          prefix = prefix.styled(Style::Cyan | Style::Bold);
          writeln!(f, "{prefix} {line}")?;

          if let Some(ind_ln) = indicator_lines.next() {
            if first_ind_ln {
              let ind_ln_padding = " ".repeat(col);
              let ind_ln = format!("{ind_ln_padding}{ind_ln}");
              writeln!(f, "{bar}{ind_ln}")?;
              first_ind_ln = false;
            } else {
              writeln!(f, "{bar} {ind_ln}")?;
            }
          }
        }

        write!(f, "{bar}")?;

        let bar_break = "-".styled(Style::Cyan | Style::Bold);
        if !notes.is_empty() {
          writeln!(f)?;
        }
        for note in notes {
          write!(f, "{padding}{bar_break} {note}")?;
        }
        Ok(())
      }
    }
  }
}

impl From<std::io::Error> for ShErr {
  fn from(e: std::io::Error) -> Self {
    let msg = std::io::Error::last_os_error();
    ShErr::simple(ShErrKind::IoErr(e.kind()), msg.to_string())
  }
}

impl From<std::env::VarError> for ShErr {
  fn from(value: std::env::VarError) -> Self {
    ShErr::simple(ShErrKind::InternalErr, value.to_string())
  }
}

impl From<Errno> for ShErr {
  fn from(value: Errno) -> Self {
    ShErr::simple(ShErrKind::Errno(value), value.to_string())
  }
}

#[derive(Debug, Clone)]
pub enum ShErrKind {
  IoErr(io::ErrorKind),
  SyntaxErr,
  ParseErr,
  InternalErr,
  ExecFail,
  HistoryReadErr,
  ResourceLimitExceeded,
  BadPermission,
  Errno(Errno),
  FileNotFound(String),
  CmdNotFound(String),
  ReadlineIntr(String),
  ReadlineErr,

  // Not really errors, more like internal signals
  CleanExit(i32),
  FuncReturn(i32),
  LoopContinue(i32),
  LoopBreak(i32),
  ClearReadline,
  Null,
}

impl Display for ShErrKind {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let output = match self {
      Self::IoErr(e) => &format!("I/O Error: {e}"),
      Self::SyntaxErr => "Syntax Error",
      Self::ParseErr => "Parse Error",
      Self::InternalErr => "Internal Error",
      Self::HistoryReadErr => "History Parse Error",
      Self::ExecFail => "Execution Failed",
      Self::ResourceLimitExceeded => "Resource Limit Exceeded",
      Self::BadPermission => "Bad Permissions",
      Self::Errno(e) => &format!("Errno: {}", e.desc()),
      Self::FileNotFound(file) => &format!("File not found: {file}"),
      Self::CmdNotFound(cmd) => &format!("Command not found: {cmd}"),
      Self::CleanExit(_) => "",
      Self::FuncReturn(_) => "Syntax Error",
      Self::LoopContinue(_) => "Syntax Error",
      Self::LoopBreak(_) => "Syntax Error",
      Self::ReadlineIntr(_) => "",
      Self::ReadlineErr => "Readline Error",
      Self::ClearReadline => "",
      Self::Null => "",
    };
    write!(f, "{output}")
  }
}
