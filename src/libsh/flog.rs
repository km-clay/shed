use std::fmt::Display;

use super::term::{Style, Styled};

#[derive(Clone, Copy, PartialEq, PartialOrd, Ord, Eq, Debug)]
#[repr(u8)]
pub enum ShedLogLevel {
  NONE = 0,
  ERROR = 1,
  WARN = 2,
  INFO = 3,
  DEBUG = 4,
  TRACE = 5,
}

impl Display for ShedLogLevel {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    use ShedLogLevel::*;
    match self {
      ERROR => write!(f, "{}", "ERROR".styled(Style::Red | Style::Bold)),
      WARN => write!(f, "{}", "WARN".styled(Style::Yellow | Style::Bold)),
      INFO => write!(f, "{}", "INFO".styled(Style::Green | Style::Bold)),
      DEBUG => write!(f, "{}", "DEBUG".styled(Style::Magenta | Style::Bold)),
      TRACE => write!(f, "{}", "TRACE".styled(Style::Blue | Style::Bold)),
      NONE => write!(f, ""),
    }
  }
}

pub fn log_level() -> ShedLogLevel {
  use ShedLogLevel::*;
  let level = std::env::var("FERN_LOG_LEVEL").unwrap_or_default();
  match level.to_lowercase().as_str() {
    "error" => ERROR,
    "warn" => WARN,
    "info" => INFO,
    "debug" => DEBUG,
    "trace" => TRACE,
    _ => NONE,
  }
}

/// A structured logging macro designed for `shed`.
///
/// `flog!` was implemented because `rustyline` uses `env_logger`, which
/// clutters the debug output. This macro prints log messages in a structured
/// format, including the log level, filename, and line number.
///
/// # Usage
///
/// The macro supports three types of arguments:
///
/// ## 1. **Formatted Messages**
/// Similar to `println!` or `format!`, allows embedding values inside a
/// formatted string.
///
/// ```rust
/// flog!(ERROR, "foo is {}", foo);
/// ```
/// **Output:**
/// ```plaintext
/// [ERROR][file.rs:10] foo is <value of foo>
/// ```
///
/// ## 2. **Literals**
/// Directly prints each literal argument as a separate line.
///
/// ```rust
/// flog!(WARN, "foo", "bar");
/// ```
/// **Output:**
/// ```plaintext
/// [WARN][file.rs:10] foo
/// [WARN][file.rs:10] bar
/// ```
///
/// ## 3. **Expressions**
/// Logs the evaluated result of each given expression, displaying both the
/// expression and its value.
///
/// ```rust
/// flog!(INFO, 1.min(2));
/// ```
/// **Output:**
/// ```plaintext
/// [INFO][file.rs:10] 1
/// ```
///
/// # Considerations
/// - This macro uses `eprintln!()` internally, so its formatting rules must be
///   followed.
/// - **Literals and formatted messages** require arguments that implement
///   [`std::fmt::Display`].
/// - **Expressions** require arguments that implement [`std::fmt::Debug`].
#[macro_export]
macro_rules! flog {
	($level:path, $fmt:literal, $($args:expr),+ $(,)?) => {{
		use $crate::libsh::flog::log_level;
		use $crate::libsh::term::Styled;
		use $crate::libsh::term::Style;

		if $level <= log_level() {
			let file = file!().styled(Style::Cyan);
			let line = line!().to_string().styled(Style::Cyan);

			eprintln!(
				"[{}][{}:{}] {}",
				$level, file, line, format!($fmt, $($args),+)
			);
		}
	}};

	($level:path, $($val:expr),+ $(,)?) => {{
		use $crate::libsh::flog::log_level;
		use $crate::libsh::term::Styled;
		use $crate::libsh::term::Style;

		if $level <= log_level() {
			let file = file!().styled(Style::Cyan);
			let line = line!().to_string().styled(Style::Cyan);

			$(
				let val_name = stringify!($val);
				eprintln!(
					"[{}][{}:{}] {} = {:#?}",
					$level, file, line, val_name, &$val
				);
			)+
		}
	}};

	($level:path, $($lit:literal),+ $(,)?) => {{
		use $crate::libsh::flog::log_level;
		use $crate::libsh::term::Styled;
		use $crate::libsh::term::Style;

		if $level <= log_level() {
			let file = file!().styled(Style::Cyan);
			let line = line!().to_string().styled(Style::Cyan);

			$(
				eprintln!(
					"[{}][{}:{}] {}",
					$level, file, line, $lit
				);
			)+
		}
	}};
}
