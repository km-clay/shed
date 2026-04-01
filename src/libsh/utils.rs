use std::collections::VecDeque;

use ariadne::Span as AriadneSpan;

use crate::parse::execute::exec_input;
use crate::parse::lex::{Span, Tk, TkRule};
use crate::parse::{Node, Redir, RedirType};
use crate::prelude::*;
use crate::state::AutoCmd;

#[macro_export]
/// A macro that abbreviates a loop that looks like this:
/// ```
/// while let Some(binding) = iter.next() {
///  	 match binding {
///  	   // arms...
///  	 }
///  }
///  ```
///
///  This pattern is used extensively for parsing strings char by char.
macro_rules! match_loop {
	($expr:expr => $binding:ident, { $($arms:tt)* }) => {
		while let Some($binding) = $expr {
			match $binding {
				$($arms)*
			}
		}
	};
	($expr:expr => $pat:pat => $binding:expr, { $($arms:tt)* }) => {
		while let Some($pat) = $expr {
			match $binding {
				$($arms)*
			}
		}
	};
}

#[macro_export]
/// A macro that abbreviates the creation of a ShErr, allowing you to specify the kind and a format string with arguments, and optionally a span for error location.
/// Examples:
/// ```
/// sherr!(ParseErr, "Unexpected token: {}", token);
/// sherr!(SyntaxErr, span, "Expected ';' but found '{}'", found);
/// ```
macro_rules! sherr {
	($kind:ident($($inner:tt)*)@$span:expr, $($arg:tt)*) => {
		$crate::libsh::error::ShErr::at(
			$crate::libsh::error::ShErrKind::$kind($($inner)*),
			$span, format!($($arg)*)
		)
	};
	($kind:ident($($inner:tt)*), $($arg:tt)*) => {
		$crate::libsh::error::ShErr::simple(
			$crate::libsh::error::ShErrKind::$kind($($inner)*),
			format!($($arg)*)
		)
	};
	($kind:ident@$span:expr, $($arg:tt)*) => {
		$crate::libsh::error::ShErr::at(
			$crate::libsh::error::ShErrKind::$kind,
			$span, format!($($arg)*)
		)
	};
	($kind:ident, $($arg:tt)*) => {
		$crate::libsh::error::ShErr::simple(
			$crate::libsh::error::ShErrKind::$kind,
			format!($($arg)*)
		)
	};
}

#[macro_export]
/// Defines a two-way mapping between an enum and its string representation, implementing both Display and FromStr.
macro_rules! two_way_display {
	($name:ident, $($member:ident <=> $val:expr;)*) => {
		impl Display for $name {
			fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
				match self {
					$(Self::$member => write!(f, $val),)*
				}
			}
		}

		impl FromStr for $name {
			type Err = ShErr;
			fn from_str(s: &str) -> Result<Self, Self::Err> {
				match s {
					$($val => Ok(Self::$member),)*
						_ => Err($crate::sherr!(
								ParseErr,
								"Invalid {} kind: {}",stringify!($name),s,
						)),
				}
			}
		}
	};
}

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

pub trait AutoCmdVecUtils {
  fn exec(&self);
  fn exec_with(&self, pattern: &str);
}

pub trait RedirVecUtils<Redir> {
  /// Splits the vector of redirections into two vectors
  ///
  /// One vector contains input redirs, the other contains output redirs
  fn split_by_channel(self) -> (Vec<Redir>, Vec<Redir>);
}

pub trait NodeVecUtils<Node> {
  fn get_span(&self) -> Option<Span>;
}

impl AutoCmdVecUtils for Vec<AutoCmd> {
  fn exec(&self) {
    let saved_status = crate::state::get_status();
    for cmd in self {
      let AutoCmd {
        pattern: _,
        kind: _,
        command,
      } = cmd;
      if let Err(e) = exec_input(command.clone(), None, false, Some("autocmd".into())) {
        e.print_error();
      }
    }
    crate::state::set_status(saved_status);
  }
  fn exec_with(&self, other_pattern: &str) {
    let saved_status = crate::state::get_status();
    for cmd in self {
      let AutoCmd {
        pattern,
        kind: _,
        command,
      } = cmd;
      if let Some(pat) = pattern
        && !pat.is_match(other_pattern)
      {
        log::trace!(
          "autocmd pattern '{}' did not match '{}', skipping",
          pat,
          other_pattern
        );
        continue;
      }

      if let Err(e) = exec_input(command.clone(), None, false, Some("autocmd".into())) {
        e.print_error();
      }
    }
    crate::state::set_status(saved_status);
  }
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
      self.last().map(|last_tk| {
        Span::new(
          first_tk.span.range().start..last_tk.span.range().end,
          first_tk.source(),
        )
      })
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

impl NodeVecUtils<Node> for Vec<Node> {
  fn get_span(&self) -> Option<Span> {
    if let Some(first_nd) = self.first()
      && let Some(last_nd) = self.last()
    {
      let first_start = first_nd.get_span().range().start;
      let last_end = last_nd.get_span().range().end;
      if first_start <= last_end {
        return Some(Span::new(
          first_start..last_end,
          first_nd.get_span().source().content(),
        ));
      }
    }
    None
  }
}
