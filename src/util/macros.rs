/// Write to the internal Terminal buffer
///
/// The given input will be buffered, meaning it won't be sent to the terminal until `Terminal::flush()` is called
/// Note that this calls `Shed::term_mut()` internally, so don't call this inside of that function or the program explodes.
#[macro_export]
macro_rules! write_term {
  ($($arg:tt)*) => {{
    use std::io::Write;
    $crate::state::Shed::term_mut(|t| write!(t, $($arg)*))
  }};
}

/// Write to the internal Terminal buffer, and then flush it
///
/// This sends the given format args directly to the terminal.
/// Note that this calls `Shed::term()` internally.
#[macro_export]
macro_rules! flush_term {
  () => {
    use std::io::Write;
    $crate::state::Shed::term_mut(|t| t.flush())
  };
}

/// Directly execute terminal control sequences
///
/// similar to crossterm's `execute!` macro, this allows you to send terminal control sequences directly to the terminal without queuing them in the buffer first.
#[macro_export]
macro_rules! exec_term {
  ($($ctl:expr),* $(,)?) => {'exec_term: {
    use ::std::io::Write;
    if let Err(e) = $crate::queue_term!($($ctl),*) {
      break 'exec_term Err(e);
    };

    if let Err(e) = Shed::term_mut(|t| t.flush()) {
      break 'exec_term Err($crate::util::ShErr::from(e));
    }

    Ok(())
  }};
}

/// Queue terminal control sequences
///
/// Similar to crossterm's `queue!` macro, this allows you to push terminal control sequences directly to the terminal buffer without flushing immediately.
#[macro_export]
macro_rules! queue_term {
  ($($ctl:expr),* $(,)?) => {'queue_term: {
    #[allow(unused_imports)]
    use $crate::state::terminal::{
      CursorStyle::*,
      Toggle::*,
      TermCtl,
      CursorCtl::*,
      ClearCtl::*,
      Attr::*,
      Scroll,
      OscCtl::*,
      TermQuery::*
    };

    $(
      if let Err(e) = Shed::term_mut(|t| {
        t.execute_control(&$ctl)
      }) {
        break 'queue_term Err(e);
      };
    )*

    Ok(())
  }};
}

/// Shorthand for creating `VerbCmds`, like `verb!(Verb::Delete)` or `verb!(3, Verb::Change)`
/// If no count is given, the count defaults to 1.
#[macro_export]
macro_rules! verb {
  ($verb:expr) => {
    $crate::readline::editcmd::Cmd(1, $verb)
  };
  ($verb:expr,) => {
    $crate::readline::editcmd::Cmd(1, $verb)
  };
  ($count:expr, $verb:expr) => {
    $crate::readline::editcmd::Cmd($count, $verb)
  };
  ($count:expr, $verb:expr,) => {
    $crate::readline::editcmd::Cmd($count, $verb)
  };
}

/// Shorthand for creating `MotionCmds`, like `motion!(Motion::ForwardChar)` or `motion!(3, Motion::LineDown)`
/// If no count is given, the count defaults to 1.
#[macro_export]
macro_rules! motion {
  ($motion:expr) => {
    $crate::readline::editcmd::Cmd(1, $motion)
  };
  ($motion:expr,) => {
    $crate::readline::editcmd::Cmd(1, $motion)
  };
  ($count:expr, $motion:expr) => {
    $crate::readline::editcmd::Cmd($count, $motion)
  };
  ($count:expr, $motion:expr,) => {
    $crate::readline::editcmd::Cmd($count, $motion)
  };
}

/// A macro for creating `KeyEvent` instances from a syntax like `key!(Ctrl + Shift + Enter)` or `key!(Alt + 'f')` or just `key!(Enter)`.
#[macro_export]
macro_rules! key {
  // if anyone has ideas for how to not write out each combination manually,
  // I am all ears
  (Shift + Ctrl + Alt + $key:ident) => {
    key!(Ctrl + Shift + Alt + $key)
  };
  (Shift + Alt + Ctrl + $key:ident) => {
    key!(Ctrl + Shift + Alt + $key)
  };
  (Alt + Ctrl + Shift + $key:ident) => {
    key!(Ctrl + Shift + Alt + $key)
  };
  (Alt + Shift + Ctrl + $key:ident) => {
    key!(Ctrl + Shift + Alt + $key)
  };
  (Ctrl + Alt + Shift + $key:ident) => {
    key!(Ctrl + Shift + Alt + $key)
  };
  (Ctrl + Shift + Alt + $key:ident) => {
    $crate::keys::KeyEvent(
      $crate::keys::KeyCode::$key,
      $crate::keys::ModKeys::CTRL_SHIFT_ALT,
    )
  };

  (Shift + Ctrl + $key:ident) => {
    key!(Ctrl + Shift + $key)
  };
  (Ctrl + Shift + $key:ident) => {
    $crate::keys::KeyEvent(
      $crate::keys::KeyCode::$key,
      $crate::keys::ModKeys::CTRL_SHIFT,
    )
  };
  (Alt + Ctrl + $key:ident) => {
    key!(Ctrl + Alt + $key)
  };
  (Ctrl + Alt + $key:ident) => {
    $crate::keys::KeyEvent($crate::keys::KeyCode::$key, $crate::keys::ModKeys::CTRL_ALT)
  };
  (Alt + Shift + $key:ident) => {
    key!(Shift + Alt + $key)
  };
  (Shift + Alt + $key:ident) => {
    $crate::keys::KeyEvent(
      $crate::keys::KeyCode::$key,
      $crate::keys::ModKeys::SHIFT_ALT,
    )
  };

  (Ctrl + $key:ident) => {
    $crate::keys::KeyEvent($crate::keys::KeyCode::$key, $crate::keys::ModKeys::CTRL)
  };
  (Shift + $key:ident) => {
    $crate::keys::KeyEvent($crate::keys::KeyCode::$key, $crate::keys::ModKeys::SHIFT)
  };
  (Alt + $key:ident) => {
    $crate::keys::KeyEvent($crate::keys::KeyCode::$key, $crate::keys::ModKeys::ALT)
  };

  ($key:ident) => {
    $crate::keys::KeyEvent($crate::keys::KeyCode::$key, $crate::keys::ModKeys::NONE)
  };

  (Shift + Ctrl + Alt + $ch:literal) => {
    key!(Ctrl + Shift + Alt + $ch)
  };
  (Shift + Alt + Ctrl + $ch:literal) => {
    key!(Ctrl + Shift + Alt + $ch)
  };
  (Alt + Ctrl + Shift + $ch:literal) => {
    key!(Ctrl + Shift + Alt + $ch)
  };
  (Alt + Shift + Ctrl + $ch:literal) => {
    key!(Ctrl + Shift + Alt + $ch)
  };
  (Ctrl + Alt + Shift + $ch:literal) => {
    key!(Ctrl + Shift + Alt + $ch)
  };
  (Ctrl + Shift + Alt + $ch:literal) => {
    $crate::keys::KeyEvent(
      $crate::keys::KeyCode::Char($ch),
      $crate::keys::ModKeys::CTRL_SHIFT_ALT,
    )
  };

  (Shift + Ctrl + $ch:literal) => {
    key!(Ctrl + Shift + $ch)
  };
  (Ctrl + Shift + $ch:literal) => {
    $crate::keys::KeyEvent(
      $crate::keys::KeyCode::Char($ch),
      $crate::keys::ModKeys::CTRL_SHIFT,
    )
  };
  (Alt + Ctrl + $ch:literal) => {
    key!(Ctrl + Alt + $ch)
  };
  (Ctrl + Alt + $ch:literal) => {
    $crate::keys::KeyEvent(
      $crate::keys::KeyCode::Char($ch),
      $crate::keys::ModKeys::CTRL_ALT,
    )
  };
  (Alt + Shift + $ch:literal) => {
    key!(Shift + Alt + $ch)
  };
  (Shift + Alt + $ch:literal) => {
    $crate::keys::KeyEvent(
      $crate::keys::KeyCode::Char($ch),
      $crate::keys::ModKeys::SHIFT_ALT,
    )
  };

  (Ctrl + $ch:literal) => {
    $crate::keys::KeyEvent(
      $crate::keys::KeyCode::Char($ch),
      $crate::keys::ModKeys::CTRL,
    )
  };
  (Shift + $ch:literal) => {
    $crate::keys::KeyEvent(
      $crate::keys::KeyCode::Char($ch),
      $crate::keys::ModKeys::SHIFT,
    )
  };
  (Alt + $ch:literal) => {
    $crate::keys::KeyEvent($crate::keys::KeyCode::Char($ch), $crate::keys::ModKeys::ALT)
  };

  ($ch:literal) => {
    $crate::keys::KeyEvent(
      $crate::keys::KeyCode::Char($ch),
      $crate::keys::ModKeys::NONE,
    )
  };
}

/// A macro that abbreviates a loop that looks like this:
/// ```
/// while let Some(binding) = iter.next() {
///  	 match binding {
///  	   // arms...
///  	 }
///  }
///  ```
///
///  This macro is used extensively for parsing strings one character at a time.
///
///  > "The input language to the shell shall first be recognized at the character level"
///  >
///  > -- POSIX 1003.1-2024, 2.10.1 Shell Grammar Lexical Conventions
///
///  This macro comes in two forms.
///  The basic case, `expr => binding`:
///  ```
///  let input = String::from("bar");
///  let mut chars = input.chars();
///
///	 // expression => binding
///  match_loop!(chars.next() => ch, {
///  	'b' | 'a' | 'r' => {
///  		// some logic
///  	}
///  	_ => panic!()
///  })
///  ```
///
///  and the pattern matching case, `expr => pat => binding`
///  ```
///  let input = String::from("bar");
///  let mut chars = input.chars().peekable();
///
///	 // expression => pattern => binding
///  match_loop!(char_indices.peek() => (i,ch) => ch, {
///  	'b' | 'a' | 'r' => {
///  		// some logic
///  	}
///  	_ => panic!()
///  })
///  ```
#[macro_export]
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

/// A macro that abbreviates the creation of a `ShErr`, allowing you to specify the kind and a format string with arguments, and optionally a span for error location.
/// Providing a span will automatically make the printed error point at the offending text referred to by the span.
/// Examples:
/// ```
/// sherr!(ParseErr, "Unexpected token: {token}");
/// sherr!(SyntaxErr @ span, "Expected ';' but found '{found}'");
/// ```
#[macro_export]
macro_rules! sherr {
	($kind:ident($($inner:tt)*)@$span:expr, $($arg:tt)*) => {
		$crate::util::ShErr::at(
			$crate::util::ShErrKind::$kind($($inner)*),
			$span, ::shed_macros::styled_format!($($arg)*).into()
		)
	};
	($kind:ident($($inner:tt)*), $($arg:tt)*) => {
		$crate::util::ShErr::simple(
			$crate::util::ShErrKind::$kind($($inner)*),
			::shed_macros::styled_format!($($arg)*).into()
		)
	};
	($kind:ident@$span:expr, $($arg:tt)*) => {
		$crate::util::ShErr::at(
			$crate::util::ShErrKind::$kind,
			$span, ::shed_macros::styled_format!($($arg)*).into()
		)
	};
	($kind:ident, $($arg:tt)*) => {
		$crate::util::ShErr::simple(
			$crate::util::ShErrKind::$kind,
			$crate::state::vars::VarStr::from(::shed_macros::styled_format!($($arg)*))
		)
	};
}

/// Defines a two-way mapping between an enum and its string representation, implementing both Display and `FromStr`.
/// Example:
///
/// ```
/// enum Foobars {
/// 	Foo,
/// 	Bar
/// }
/// two_way_display! {Foobars,
/// 	Foo <=> "foo",
/// 	Bar <=> "bar",
/// }
/// ```
#[macro_export]
macro_rules! two_way_display {
	($name:ident, $($member:ident <=> $val:expr;)*) => {
		impl ::std::fmt::Display for $name {
			fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
				match self {
					$(Self::$member => write!(f, $val),)*
				}
			}
		}

		impl ::std::str::FromStr for $name {
			type Err = $crate::util::ShErr;
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

/*
 * Below are our primitives for writing to the shell's output channels.
 * It is basically a re-implementation of println! and eprintln! and the others.
 * I may just be superstitious, but writing bytes directly to the file descriptor
 * feels better than calling println!() and praying, when we are constantly performing I/O redirection.
 *
 * Anyway we are ignoring io errors on these. If that becomes a problem later, we can change it
 */

#[macro_export]
macro_rules! outln {
  ($($arg:tt)*) => {{
    $crate::_write_inner!(out, writeln, $crate::procio::stdout_fileno(), $($arg)*)
  }};
}
#[macro_export]
macro_rules! out {
  ($($arg:tt)*) => {{
    $crate::_write_inner!(out, write, $crate::procio::stdout_fileno(), $($arg)*)
  }};
}
#[macro_export]
macro_rules! errln {
  ($($arg:tt)*) => {{
    $crate::_write_inner!(err, writeln, $crate::procio::stderr_fileno(), $($arg)*)
  }};
}
#[macro_export]
macro_rules! err {
  ($($arg:tt)*) => {{
    $crate::_write_inner!(err, write, $crate::procio::stderr_fileno(), $($arg)*)
  }};
}

#[macro_export]
macro_rules! _write_inner {
  (out, $macro:tt, $fd:expr, $($arg:tt)*) => {{
    use ::std::io::Write;
    $crate::state::Shed::sinks(|s| { $macro!(s, $($arg)*).ok(); });
  }};
  (err, $macro:tt, $fd:expr, $($arg:tt)*) => {{
    use ::std::io::Write;
    $macro!($crate::util::FdWriter($fd), $($arg)*).ok();
  }};
}

#[macro_export]
macro_rules! varstr {
  ($($arg:tt)*) => {
    $crate::state::vars::VarStr::from(smol_str::format_smolstr!($($arg)*))
  };
}

// not ignoring io errors on this one since it takes an arbitrary fd
#[macro_export]
macro_rules! writefd {
    ($fd:expr, $($arg:tt)*) => {{
      use std::io::Write;
      write!($crate::util::FdWriter($fd.as_fd()), $($arg)*)
    }};
}

/// Post a status message to the shell's status line.
///
/// This is intended for transient messages that should be visible to the user but not take up space in the terminal output, such as "File saved" or "Syntax error on line 3".
#[macro_export]
macro_rules! status_msg {
  ($($arg:tt)*) => {{
    $crate::state::Shed::post_status_msg(format!($($arg)*))
  }};
}

/// Post a system message.
///
/// System messages appear above the prompt, and as such survive redraws. This mechanism is used for things like job status notifications. Good for important messages that the user shouldn't miss.
#[macro_export]
macro_rules! system_msg {
  ($($arg:tt)*) => {{
    $crate::state::Shed::post_system_msg(format!($($arg)*))
  }};
}

/// Broadcast a message over the socket.
///
/// Socket messages are sent to every current socket subscriber. Each line is prefixed with `msg>>`
#[macro_export]
macro_rules! socket_msg {
  ($($arg:tt)*) => {{
    $crate::state::Shed::broadcast_msg(&format!($($arg)*));
  }};
}

/// Execute autocmds
#[macro_export]
macro_rules! autocmd {
  ($kind:ident) => {{
    let post_cmds =
      $crate::state::Shed::logic(|l| l.get_autocmds($crate::state::logic::AutoCmdKind::$kind));

    $crate::util::with_saved_status(|| {
      $crate::state::Shed::notify_autocmd($crate::state::logic::AutoCmdKind::$kind);
      for cmd in post_cmds {
        if let Err(e) = $crate::eval::execute::exec_nonint(cmd.command(), Some("autocmd".into())) {
          e.print_error();
        }
      }
    })
  }};
}

/// Get a shell variable from `Shed::vars()`. Returns an empty string if unset. Checks env vars too.
#[macro_export]
macro_rules! var {
  ($name:expr) => {
    $crate::state::Shed::vars(|v| v.get_var($name))
  };
}

/// Try to get a shell variable from `Shed::vars()`. Returns None if unset. Checks env vars too.
/// Useful if you need to match on whether a variable exists or not.
#[macro_export]
macro_rules! try_var {
  ($name:expr) => {
    $crate::state::Shed::vars(|v| v.try_get_var($name))
  };
}

/// Get a shell option from `Shed::shopts()`.
#[macro_export]
macro_rules! shopt {
  ($($path:tt)*) => {
    $crate::state::Shed::shopts(|o| o.$($path)*)
  };
}

/// Get a mutable shell option from `Shed::shopts_mut()`.
/// You can use this to alter shopt values inline.
#[macro_export]
macro_rules! shopt_mut {
  ($($path:tt)*) => {
    $crate::state::Shed::shopts_mut(|o| o.$($path)*)
  };
}

#[macro_export]
macro_rules! assert_sorted {
  ($arr:expr) => {
    // credit goes to fish shell for this idea. very nice pattern
    // at compile time, checks to see if the given const array is sorted alphabetically
    // if not, compiler error
    const _: () = {
      let mut i = 1;
      while i < $arr.len() {
        let prev = $arr[i - 1].as_bytes();
        let curr = $arr[i].as_bytes();
        let len = if prev.len() < curr.len() {
          prev.len()
        } else {
          curr.len()
        };
        let mut j = 0;
        while j < len {
          if prev[j] > curr[j] {
            panic!("Array must be in alphabetical order");
          }
          if prev[j] < curr[j] {
            break;
          }
          j += 1;
        }

        if j == len && prev.len() >= curr.len() {
          panic!("Array must be in alphabetical order");
        }

        i += 1;
      }
    };
  };
}
