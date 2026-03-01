use std::collections::VecDeque;

use crate::parse::lex::{Span, Tk, TkRule};
use crate::parse::{Redir, RedirType};
use crate::prelude::*;

pub trait VecDequeExt<T> {
  fn to_vec(self) -> Vec<T>;
}

pub trait CharDequeUtils {
  fn to_string(self) -> String;
  fn ends_with(&self, pat: &str) -> bool;
  fn starts_with(&self, pat: &str) -> bool;
}

pub trait TkVecUtils<Tk> {
  fn get_span(&self) -> Option<Span>;
  fn debug_tokens(&self);
  fn split_at_separators(&self) -> Vec<Vec<Tk>>;
}

pub trait RedirVecUtils<Redir> {
  /// Splits the vector of redirections into two vectors
  ///
  /// One vector contains input redirs, the other contains output redirs
  fn split_by_channel(self) -> (Vec<Redir>, Vec<Redir>);
}

impl<T> VecDequeExt<T> for VecDeque<T> {
  fn to_vec(self) -> Vec<T> {
    self.into_iter().collect::<Vec<T>>()
  }
}

impl CharDequeUtils for VecDeque<char> {
  fn to_string(mut self) -> String {
    let mut result = String::with_capacity(self.len());
    while let Some(ch) = self.pop_front() {
      result.push(ch);
    }
    result
  }

  fn ends_with(&self, pat: &str) -> bool {
    let pat_chars = pat.chars();
    let self_len = self.len();

    // If pattern is longer than self, return false
    if pat_chars.clone().count() > self_len {
      return false;
    }

    // Compare from the back
    self
      .iter()
      .rev()
      .zip(pat_chars.rev())
      .all(|(c1, c2)| c1 == &c2)
  }

  fn starts_with(&self, pat: &str) -> bool {
    let pat_chars = pat.chars();
    let self_len = self.len();

    // If pattern is longer than self, return false
    if pat_chars.clone().count() > self_len {
      return false;
    }

    // Compare from the front
    self.iter().zip(pat_chars).all(|(c1, c2)| c1 == &c2)
  }
}

impl TkVecUtils<Tk> for Vec<Tk> {
  fn get_span(&self) -> Option<Span> {
    if let Some(first_tk) = self.first() {
      self
        .last()
        .map(|last_tk| Span::new(first_tk.span.range().start..last_tk.span.range().end, first_tk.source()))
    } else {
      None
    }
  }
  fn debug_tokens(&self) {
    for _token in self {}
  }
  fn split_at_separators(&self) -> Vec<Vec<Tk>> {
    let mut splits = vec![];
    let mut cur_split = vec![];
    for tk in self {
      match tk.class {
        TkRule::Pipe | TkRule::ErrPipe | TkRule::And | TkRule::Or | TkRule::Bg | TkRule::Sep => {
          splits.push(std::mem::take(&mut cur_split));
        }
        _ => cur_split.push(tk.clone()),
      }
    }

    if !cur_split.is_empty() {
      splits.push(cur_split);
    }

    splits
  }
}

impl RedirVecUtils<Redir> for Vec<Redir> {
  fn split_by_channel(self) -> (Vec<Redir>, Vec<Redir>) {
    let mut input = vec![];
    let mut output = vec![];
    for redir in self {
      match redir.class {
        RedirType::Input => input.push(redir),
        RedirType::Pipe => match redir.io_mode.tgt_fd() {
          STDIN_FILENO => input.push(redir),
          STDOUT_FILENO | STDERR_FILENO => output.push(redir),
          _ => unreachable!(),
        },
        _ => output.push(redir),
      }
    }
    (input, output)
  }
}
