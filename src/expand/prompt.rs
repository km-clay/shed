use crate::prelude::*;

#[derive(Debug)]
pub enum PromptTk {
	AsciiOct(i32),
	Text(String),
	AnsiSeq(String),
	VisGrp,
	UserSeq,
	Runtime,
	Weekday,
	Dquote,
	Squote,
	Return,
	Newline,
	Pwd,
	PwdShort,
	Hostname,
	HostnameShort,
	ShellName,
	Username,
	PromptSymbol,
	ExitCode,
	SuccessSymbol,
	FailureSymbol,
	JobCount
}

pub fn format_cmd_runtime(dur: std::time::Duration) -> String {
	const ETERNITY: u128 = f32::INFINITY as u128;
	let mut micros     = dur.as_micros();
	let mut millis     = 0;
	let mut seconds    = 0;
	let mut minutes    = 0;
	let mut hours      = 0;
	let mut days       = 0;
	let mut weeks      = 0;
	let mut months     = 0;
	let mut years      = 0;
	let mut decades    = 0;
	let mut centuries  = 0;
	let mut millennia  = 0;
	let mut epochs     = 0;
	let mut aeons      = 0;
	let mut eternities = 0;

	if micros >= 1000 {
		millis = micros / 1000;
		micros %= 1000;
	}
	if millis >= 1000 {
		seconds = millis / 1000;
		millis %= 1000;
	}
	if seconds >= 60 {
		minutes = seconds / 60;
		seconds %= 60;
	}
	if minutes >= 60 {
		hours = minutes / 60;
		minutes %= 60;
	}
	if hours >= 24 {
		days = hours / 24;
		hours %= 24;
	}
	if days >= 7 {
		weeks = days / 7;
		days %= 7;
	}
	if weeks >= 4 {
		months = weeks / 4;
		weeks %= 4;
	}
	if months >= 12 {
		years = months / 12;
		weeks %= 12;
	}
	if years >= 10 {
		decades = years / 10;
		years %= 10;
	}
	if decades >= 10 {
		centuries = decades / 10;
		decades %= 10;
	}
	if centuries >= 10 {
		millennia = centuries / 10;
		centuries %= 10;
	}
	if millennia >= 1000 {
		epochs = millennia / 1000;
		millennia %= 1000;
	}
	if epochs >= 1000 {
		aeons = epochs / 1000;
		epochs %= aeons;
	}
	if aeons == ETERNITY {
		eternities = aeons / ETERNITY;
		aeons %= ETERNITY;
	}

	// Format the result
	let mut result = Vec::new();
	if eternities > 0 {
		let mut string = format!("{} eternit", eternities);
		if eternities > 1 {
			string.push_str("ies");
		} else {
			string.push('y');
		}
		result.push(string)
	}
	if aeons > 0 {
		let mut string = format!("{} aeon", aeons);
		if aeons > 1 {
			string.push('s')
		}
		result.push(string)
	}
	if epochs > 0 {
		let mut string = format!("{} epoch", epochs);
		if epochs > 1 {
			string.push('s')
		}
		result.push(string)
	}
	if millennia > 0 {
		let mut string = format!("{} millenni", millennia);
		if millennia > 1 {
			string.push_str("um")
		} else {
			string.push('a')
		}
		result.push(string)
	}
	if centuries > 0 {
		let mut string = format!("{} centur", centuries);
		if centuries > 1 {
			string.push_str("ies")
		} else {
			string.push('y')
		}
		result.push(string)
	}
	if decades > 0 {
		let mut string = format!("{} decade", decades);
		if decades > 1 {
			string.push('s')
		}
		result.push(string)
	}
	if years > 0 {
		let mut string = format!("{} year", years);
		if years > 1 {
			string.push('s')
		}
		result.push(string)
	}
	if months > 0 {
		let mut string = format!("{} month", months);
		if months > 1 {
			string.push('s')
		}
		result.push(string)
	}
	if weeks > 0 {
		let mut string = format!("{} week", weeks);
		if weeks > 1 {
			string.push('s')
		}
		result.push(string)
	}
	if days > 0 {
		let mut string = format!("{} day", days);
		if days > 1 {
			string.push('s')
		}
		result.push(string)
	}
	if hours > 0 {
		let string = format!("{}h", hours);
		result.push(string);
	}
	if minutes > 0 {
		let string = format!("{}m", minutes);
		result.push(string);
	}
	if seconds > 0 {
		let string = format!("{}s", seconds);
		result.push(string);
	}
	if millis > 0 {
		let string = format!("{}ms",millis);
		result.push(string);
	}
	if result.is_empty() && micros > 0 {
		let string = format!("{}µs",micros);
		result.push(string);
	}

	result.join(" ")
}

fn tokenize_prompt(raw: &str) -> Vec<PromptTk> {
	let mut chars = raw.chars().peekable();
	let mut tk_text = String::new();
	let mut tokens = vec![];

	while let Some(ch) = chars.next() {
		match ch {
			'\\' => {
				// Push any accumulated text as a token
				if !tk_text.is_empty() {
					tokens.push(PromptTk::Text(std::mem::take(&mut tk_text)));
				}

				// Handle the escape sequence
				if let Some(ch) = chars.next() {
					match ch {
						'w' => tokens.push(PromptTk::Pwd),
						'W' => tokens.push(PromptTk::PwdShort),
						'h' => tokens.push(PromptTk::Hostname),
						'H' => tokens.push(PromptTk::HostnameShort),
						's' => tokens.push(PromptTk::ShellName),
						'u' => tokens.push(PromptTk::Username),
						'$' => tokens.push(PromptTk::PromptSymbol),
						'n' => tokens.push(PromptTk::Text("\n".into())),
						'r' => tokens.push(PromptTk::Text("\r".into())),
						'T' => tokens.push(PromptTk::Runtime),
						'\\' => tokens.push(PromptTk::Text("\\".into())),
						'"' => tokens.push(PromptTk::Text("\"".into())),
						'\'' => tokens.push(PromptTk::Text("'".into())),
						'e' => {
							if chars.next() == Some('[') {
								let mut params = String::new();

								// Collect parameters and final character
								while let Some(ch) = chars.next() {
									match ch {
										'0'..='9' | ';' | '?' | ':' => params.push(ch), // Valid parameter characters
										'A'..='Z' | 'a'..='z' => { // Final character (letter)
											params.push(ch);
											break;
										}
										_ => {
											// Invalid character in ANSI sequence
											tokens.push(PromptTk::Text(format!("\x1b[{params}")));
											break;
										}
									}
								}

								tokens.push(PromptTk::AnsiSeq(format!("\x1b[{params}")));
							} else {
								// Handle case where 'e' is not followed by '['
								tokens.push(PromptTk::Text("\\e".into()));
							}
						}
						'0'..='7' => {
							// Handle octal escape
							let mut octal_str = String::new();
							octal_str.push(ch);

							// Collect up to 2 more octal digits
							for _ in 0..2 {
								if let Some(&next_ch) = chars.peek() {
									if next_ch >= '0' && next_ch <= '7' {
										octal_str.push(chars.next().unwrap());
									} else {
										break;
									}
								} else {
									break;
								}
							}

							// Parse the octal string into an integer
							if let Ok(octal) = i32::from_str_radix(&octal_str, 8) {
								tokens.push(PromptTk::AsciiOct(octal));
							} else {
								// Fallback: treat as raw text
								tokens.push(PromptTk::Text(format!("\\{octal_str}")));
							}
						}
						_ => {
							// Unknown escape sequence: treat as raw text
							tokens.push(PromptTk::Text(format!("\\{ch}")));
						}
					}
				} else {
					// Handle trailing backslash
					tokens.push(PromptTk::Text("\\".into()));
				}
			}
			_ => {
				// Accumulate non-escape characters
				tk_text.push(ch);
			}
		}
	}

	// Push any remaining text as a token
	if !tk_text.is_empty() {
		tokens.push(PromptTk::Text(tk_text));
	}

	tokens
}

pub fn expand_prompt(raw: &str, shenv: &mut ShEnv) -> ShResult<String> {
	let mut tokens = tokenize_prompt(raw).into_iter();
	let mut result = String::new();

	while let Some(token) = tokens.next() {
		match token {
			PromptTk::AsciiOct(_) => todo!(),
			PromptTk::Text(txt) => result.push_str(&txt),
			PromptTk::AnsiSeq(params) => result.push_str(&params),
			PromptTk::Runtime => {
				log!(INFO, "getting runtime");
				if let Some(runtime) = shenv.meta().get_runtime() {
					log!(DEBUG, runtime);
					let runtime_fmt = format_cmd_runtime(runtime);
					result.push_str(&runtime_fmt);
				}
			}
			PromptTk::Pwd => {
				let mut pwd = std::env::var("PWD")?;
				let home = std::env::var("HOME")?;
				if pwd.starts_with(&home) {
					pwd = pwd.replacen(&home, "~", 1);
				}
				result.push_str(&pwd);
			}
			PromptTk::PwdShort => {
				let mut path = std::env::var("PWD")?;
				let home = std::env::var("HOME")?;
				if path.starts_with(&home) {
					path = path.replacen(&home, "~", 1);
				}
				let pathbuf = PathBuf::from(&path);
				let mut segments = pathbuf.iter().count();
				let mut path_iter = pathbuf.into_iter();
				while segments > 4 {
					path_iter.next();
					segments -= 1;
				}
				let path_rebuilt: PathBuf = path_iter.collect();
				let mut path_rebuilt = path_rebuilt.to_str().unwrap().to_string();
				if path_rebuilt.starts_with(&home) {
					path_rebuilt = path_rebuilt.replacen(&home, "~", 1);
				}
				result.push_str(&path_rebuilt);
			}
			PromptTk::Hostname => {
				let hostname = std::env::var("HOSTNAME")?;
				result.push_str(&hostname);
			}
			PromptTk::HostnameShort => todo!(),
			PromptTk::ShellName => result.push_str("fern"),
			PromptTk::Username => {
				let username = std::env::var("USER")?;
				result.push_str(&username);
			}
			PromptTk::PromptSymbol => {
				let uid = std::env::var("UID")?;
				let symbol = if &uid == "0" {
					'#'
				} else {
					'$'
				};
				result.push(symbol);
			}
			PromptTk::ExitCode => todo!(),
			PromptTk::SuccessSymbol => todo!(),
			PromptTk::FailureSymbol => todo!(),
			PromptTk::JobCount => todo!(),
			_ => unimplemented!()
		}
	}

	Ok(result)
}
