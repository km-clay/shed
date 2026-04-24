/// Write to the internal Terminal buffer
///
/// The given input will be buffered, meaning it won't be sent to the terminal until Terminal::flush() is called
/// Note that this calls with_term() internally.
/// DO NOT call this from within any of the state module accessors (e.g. read_logic, write_meta, etc) as that will cause a deadlock.
#[macro_export]
macro_rules! write_term {
  ($($arg:tt)*) => {{
    use std::io::Write;
    $crate::state::with_term(|t| write!(t, $($arg)*))
  }};
}

/// Write to the internal Terminal buffer, and then flush it
///
/// This sends the given format args directly to the terminal.
/// Note that this calls with_term() internally.
/// DO NOT call this from within any of the state module accessors (e.g. read_logic, write_meta, etc) as that will cause a deadlock.
#[macro_export]
macro_rules! flush_term {
  () => {
    use std::fmt::Write as FmtWrite;
    $crate::state::with_term(|t| t.flush())
  };
  ($($arg:tt)*) => {{
    use std::fmt::Write as FmtWrite;
    $crate::state::with_term(|t| {
      write!(t, $($arg)*)
        t.flush()?;
    })
  }};
}

/// Shorthand for creating VerbCmds, like `verb!(Verb::Delete)` or `verb!(3, Verb::Change)`
/// If no count is given, the count defaults to 1.
#[macro_export]
macro_rules! verb {
  ($verb:expr) => {
    VerbCmd(1, $verb)
  };
  ($verb:expr,) => {
    VerbCmd(1, $verb)
  };
  ($count:expr, $verb:expr) => {
    VerbCmd($count, $verb)
  };
  ($count:expr, $verb:expr,) => {
    VerbCmd($count, $verb)
  };
}

/// Shorthand for creating MotionCmds, like `motion!(Motion::ForwardChar)` or `motion!(3, Motion::LineDown)`
/// If no count is given, the count defaults to 1.
#[macro_export]
macro_rules! motion {
  ($motion:expr) => {
    MotionCmd(1, $motion)
  };
  ($motion:expr,) => {
    MotionCmd(1, $motion)
  };
  ($count:expr, $motion:expr) => {
    MotionCmd($count, $motion)
  };
  ($count:expr, $motion:expr,) => {
    MotionCmd($count, $motion)
  };
}

/// A macro for creating KeyEvent instances from a syntax like `key!(Ctrl + Shift + Enter)` or `key!(Alt + 'f')` or just `key!(Enter)`.
#[macro_export]
macro_rules! key {
  (Shift + Ctrl + Alt + $key:ident) => { key!(Ctrl + Shift + Alt + $key) };
  (Shift + Alt + Ctrl + $key:ident) => { key!(Ctrl + Shift + Alt + $key) };
  (Alt + Ctrl + Shift + $key:ident) => { key!(Ctrl + Shift + Alt + $key) };
  (Alt + Shift + Ctrl + $key:ident) => { key!(Ctrl + Shift + Alt + $key) };
  (Ctrl + Alt + Shift + $key:ident) => { key!(Ctrl + Shift + Alt + $key) };
  (Ctrl + Shift + Alt + $key:ident) => {
    $crate::readline::keys::KeyEvent(
      $crate::readline::keys::KeyCode::$key,
      $crate::readline::keys::ModKeys::CTRL_SHIFT_ALT
    )
  };

  (Shift + Ctrl + $key:ident) => { key!(Ctrl + Shift + $key) };
  (Ctrl + Shift + $key:ident) => {
    $crate::readline::keys::KeyEvent(
      $crate::readline::keys::KeyCode::$key,
      $crate::readline::keys::ModKeys::CTRL_SHIFT
    )
  };
  (Alt + Ctrl + $key:ident) => { key!(Ctrl + Alt + $key) };
  (Ctrl + Alt + $key:ident) => {
    $crate::readline::keys::KeyEvent(
      $crate::readline::keys::KeyCode::$key,
      $crate::readline::keys::ModKeys::CTRL_ALT
    )
  };
  (Alt + Shift + $key:ident) => { key!(Shift + Alt + $key) };
  (Shift + Alt + $key:ident) => {
    $crate::readline::keys::KeyEvent(
      $crate::readline::keys::KeyCode::$key,
      $crate::readline::keys::ModKeys::SHIFT_ALT
    )
  };

  (Ctrl + $key:ident) => {
    $crate::readline::keys::KeyEvent(
      $crate::readline::keys::KeyCode::$key,
      $crate::readline::keys::ModKeys::CTRL
    )
  };
  (Shift + $key:ident) => {
    $crate::readline::keys::KeyEvent(
      $crate::readline::keys::KeyCode::$key,
      $crate::readline::keys::ModKeys::SHIFT
    )
  };
  (Alt + $key:ident) => {
    $crate::readline::keys::KeyEvent(
      $crate::readline::keys::KeyCode::$key,
      $crate::readline::keys::ModKeys::ALT
    )
  };

  ($key:ident) => {
    $crate::readline::keys::KeyEvent(
      $crate::readline::keys::KeyCode::$key,
      $crate::readline::keys::ModKeys::NONE
    )
  };

  (Shift + Ctrl + Alt + $ch:literal) => { key!(Ctrl + Shift + Alt + $ch) };
  (Shift + Alt + Ctrl + $ch:literal) => { key!(Ctrl + Shift + Alt + $ch) };
  (Alt + Ctrl + Shift + $ch:literal) => { key!(Ctrl + Shift + Alt + $ch) };
  (Alt + Shift + Ctrl + $ch:literal) => { key!(Ctrl + Shift + Alt + $ch) };
  (Ctrl + Alt + Shift + $ch:literal) => { key!(Ctrl + Shift + Alt + $ch) };
  (Ctrl + Shift + Alt + $ch:literal) => {
    $crate::readline::keys::KeyEvent(
      $crate::readline::keys::KeyCode::Char($ch),
      $crate::readline::keys::ModKeys::CTRL_SHIFT_ALT
    )
  };

  (Shift + Ctrl + $ch:literal) => { key!(Ctrl + Shift + $ch) };
  (Ctrl + Shift + $ch:literal) => {
    $crate::readline::keys::KeyEvent(
      $crate::readline::keys::KeyCode::Char($ch),
      $crate::readline::keys::ModKeys::CTRL_SHIFT
    )
  };
  (Alt + Ctrl + $ch:literal) => { key!(Ctrl + Alt + $ch) };
  (Ctrl + Alt + $ch:literal) => {
    $crate::readline::keys::KeyEvent(
      $crate::readline::keys::KeyCode::Char($ch),
      $crate::readline::keys::ModKeys::CTRL_ALT
    )
  };
  (Alt + Shift + $ch:literal) => { key!(Shift + Alt + $ch) };
  (Shift + Alt + $ch:literal) => {
    $crate::readline::keys::KeyEvent(
      $crate::readline::keys::KeyCode::Char($ch),
      $crate::readline::keys::ModKeys::SHIFT_ALT
    )
  };

  (Ctrl + $ch:literal) => {
    $crate::readline::keys::KeyEvent(
      $crate::readline::keys::KeyCode::Char($ch),
      $crate::readline::keys::ModKeys::CTRL
    )
  };
  (Shift + $ch:literal) => {
    $crate::readline::keys::KeyEvent(
      $crate::readline::keys::KeyCode::Char($ch),
      $crate::readline::keys::ModKeys::SHIFT
    )
  };
  (Alt + $ch:literal) => {
    $crate::readline::keys::KeyEvent(
      $crate::readline::keys::KeyCode::Char($ch),
      $crate::readline::keys::ModKeys::ALT
    )
  };

  ($ch:literal) => {
    $crate::readline::keys::KeyEvent(
      $crate::readline::keys::KeyCode::Char($ch),
      $crate::readline::keys::ModKeys::NONE
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
///  match_loop!(chars.peek() => &ch => ch, {
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

/// A macro that abbreviates the creation of a ShErr, allowing you to specify the kind and a format string with arguments, and optionally a span for error location.
/// Providing a span will automatically make the printed error point at the offending text referred to by the span.
/// Examples:
/// ```
/// sherr!(ParseErr, "Unexpected token: {}", token);
/// sherr!(SyntaxErr @ span, "Expected ';' but found '{}'", found);
/// ```
#[macro_export]
macro_rules! sherr {
	($kind:ident($($inner:tt)*)@$span:expr, $($arg:tt)*) => {
		$crate::util::error::ShErr::at(
			$crate::util::error::ShErrKind::$kind($($inner)*),
			$span, format!($($arg)*)
		)
	};
	($kind:ident($($inner:tt)*), $($arg:tt)*) => {
		$crate::util::error::ShErr::simple(
			$crate::util::error::ShErrKind::$kind($($inner)*),
			format!($($arg)*)
		)
	};
	($kind:ident@$span:expr, $($arg:tt)*) => {
		$crate::util::error::ShErr::at(
			$crate::util::error::ShErrKind::$kind,
			$span, format!($($arg)*)
		)
	};
	($kind:ident, $($arg:tt)*) => {
		$crate::util::error::ShErr::simple(
			$crate::util::error::ShErrKind::$kind,
			format!($($arg)*)
		)
	};
}

/// Defines a two-way mapping between an enum and its string representation, implementing both Display and FromStr.
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
		impl Display for $name {
			fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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

/*
 * Below are our primitives for writing to the shell's output channels.
 * It is basically a re-implementation of println! and eprintln! and the others,
 * but generally more reliable in an environment that experiences constant output redirection and swapping of the underlying file descriptors.
 * These can be considered more reliable because they just push bytes directly to the kernel and let the os figure it out
 */

#[macro_export]
macro_rules! outln {
  ($($arg:tt)*) => {{
    use std::io::Write;
    writeln!($crate::util::FdWriter(nix::libc::STDOUT_FILENO), $($arg)*)
  }};
}

#[macro_export]
macro_rules! errln {
  ($($arg:tt)*) => {{
    use std::io::Write;
    writeln!($crate::util::FdWriter(nix::libc::STDERR_FILENO), $($arg)*)
  }};
}

#[macro_export]
macro_rules! out {
  ($($arg:tt)*) => {{
    use std::io::Write;
    write!($crate::util::FdWriter(nix::libc::STDOUT_FILENO), $($arg)*)
  }};
}

#[macro_export]
macro_rules! err {
  ($($arg:tt)*) => {{
    use std::io::Write;
    write!($crate::util::FdWriter(nix::libc::STDERR_FILENO), $($arg)*)
  }};
}
