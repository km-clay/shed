use std::collections::VecDeque;

use crate::{libsh::term::{Style, Styled}, prompt::readline::{history::History, keys::{KeyCode, KeyEvent, ModKeys}, linebuf::LineBuf, term::{raw_mode, KeyReader, LineWriter}, vimode::{ViInsert, ViMode, ViNormal}, FernVi, Readline}};

use pretty_assertions::assert_eq;

use super::super::*;

#[derive(Default,Debug)]
struct TestReader {
	pub bytes: VecDeque<u8>
}

impl TestReader {
	pub fn new() -> Self {
		Self::default()
	}
	pub fn with_initial(mut self, bytes: &[u8]) -> Self {
		let bytes = bytes.iter();
		self.bytes.extend(bytes);
		self
	}

	pub fn parse_esc_seq_from_bytes(&mut self) -> Option<KeyEvent> {
		let mut seq = vec![0x1b];
		let b1 = self.bytes.pop_front()?;
		seq.push(b1);

		match b1 {
			b'[' => {
				let b2 = self.bytes.pop_front()?;
				seq.push(b2);

				match b2 {
					b'A' => Some(KeyEvent(KeyCode::Up, ModKeys::empty())),
					b'B' => Some(KeyEvent(KeyCode::Down, ModKeys::empty())),
					b'C' => Some(KeyEvent(KeyCode::Right, ModKeys::empty())),
					b'D' => Some(KeyEvent(KeyCode::Left, ModKeys::empty())),
					b'1'..=b'9' => {
						let mut digits = vec![b2];

						while let Some(&b) = self.bytes.front() {
							seq.push(b);
							self.bytes.pop_front();

							if b == b'~' || b == b';' {
								break;
							} else if b.is_ascii_digit() {
								digits.push(b);
							} else {
								break;
							}
						}

						let key = match digits.as_slice() {
							[b'1'] => KeyCode::Home,
							[b'3'] => KeyCode::Delete,
							[b'4'] => KeyCode::End,
							[b'5'] => KeyCode::PageUp,
							[b'6'] => KeyCode::PageDown,
							[b'7'] => KeyCode::Home, // xterm alternate
							[b'8'] => KeyCode::End,  // xterm alternate

							[b'1', b'5'] => KeyCode::F(5),
							[b'1', b'7'] => KeyCode::F(6),
							[b'1', b'8'] => KeyCode::F(7),
							[b'1', b'9'] => KeyCode::F(8),
							[b'2', b'0'] => KeyCode::F(9),
							[b'2', b'1'] => KeyCode::F(10),
							[b'2', b'3'] => KeyCode::F(11),
							[b'2', b'4'] => KeyCode::F(12),
							_ => KeyCode::Esc,
						};

						Some(KeyEvent(key, ModKeys::empty()))
					}
					_ => Some(KeyEvent(KeyCode::Esc, ModKeys::empty())),
				}
			}

			b'O' => {
				let b2 = self.bytes.pop_front()?;
				seq.push(b2);

				let key = match b2 {
					b'P' => KeyCode::F(1),
					b'Q' => KeyCode::F(2),
					b'R' => KeyCode::F(3),
					b'S' => KeyCode::F(4),
					_ => KeyCode::Esc,
				};

				Some(KeyEvent(key, ModKeys::empty()))
			}

			_ => Some(KeyEvent(KeyCode::Esc, ModKeys::empty())),
		}
	}
}

impl KeyReader for TestReader {
	fn read_key(&mut self) -> Option<KeyEvent> {
		use core::str;

		let mut collected = Vec::with_capacity(4);

		loop {
			let byte = self.bytes.pop_front()?;
			collected.push(byte);

			// If it's an escape sequence, delegate
			if collected[0] == 0x1b && collected.len() == 1 {
				if let Some(&_next @ (b'[' | b'0')) = self.bytes.front() {
					println!("found escape seq");
					let seq = self.parse_esc_seq_from_bytes();
					println!("{seq:?}");
					return seq
				}
			}

			// Try parse as valid UTF-8
			if let Ok(s) = str::from_utf8(&collected) {
				return Some(KeyEvent::new(s, ModKeys::empty()));
			}

			if collected.len() >= 4 {
				break;
			}
		}

		None
	}
}

pub struct TestWriter {
}

impl TestWriter {
	pub fn new() -> Self {
		Self {}
	}
}

impl LineWriter for TestWriter {
	fn clear_rows(&mut self, _layout: &prompt::readline::term::Layout) -> libsh::error::ShResult<()> {
		Ok(())
	}

	fn redraw(
		&mut self,
		_prompt: &str,
		_line: &LineBuf,
		_new_layout: &prompt::readline::term::Layout,
	) -> libsh::error::ShResult<()> {
		Ok(())
	}

	fn flush_write(&mut self, _buf: &str) -> libsh::error::ShResult<()> {
		Ok(())
	}
}

impl FernVi {
	pub fn new_test(prompt: Option<String>,input: &str, initial: &str) -> Self {
		Self {
			reader: Box::new(TestReader::new().with_initial(input.as_bytes())),
			writer: Box::new(TestWriter::new()),
			prompt: prompt.unwrap_or("$ ".styled(Style::Green)),
			mode: Box::new(ViInsert::new()),
			old_layout: None,
			repeat_action: None,
			repeat_motion: None,
			history: History::new().unwrap(),
			editor: LineBuf::new().with_initial(initial, 0)
		}
	}
}

fn fernvi_test(input: &str, initial: &str) -> String {
	let mut fernvi = FernVi::new_test(None,input,initial);
	let raw_mode = raw_mode();
	let line = fernvi.readline().unwrap();
	std::mem::drop(raw_mode);
	line 
}

fn normal_cmd(cmd: &str, buf: &str, cursor: usize) -> (String,usize) {
	let cmd = ViNormal::new()
		.cmds_from_raw(cmd)
		.pop()
		.unwrap();
	let mut buf = LineBuf::new().with_initial(buf, cursor);
	buf.exec_cmd(cmd).unwrap();
	(buf.as_str().to_string(),buf.cursor.get()) 
}

#[test]
fn vimode_insert_cmds() {
	let raw = "abcdefghijklmnopqrstuvwxyz1234567890-=[];'<>/\\x1b";
	let mut mode = ViInsert::new();
	let cmds = mode.cmds_from_raw(raw);
	insta::assert_debug_snapshot!(cmds)
}

#[test]
fn vimode_normal_cmds() {
	let raw = "d2wg?5b2P5x";
	let mut mode = ViNormal::new();
	let cmds = mode.cmds_from_raw(raw);
	insta::assert_debug_snapshot!(cmds)
}

#[test]
fn linebuf_empty_linebuf() {
	let mut buf = LineBuf::new();
	assert_eq!(buf.as_str(), "");
	buf.update_graphemes_lazy();
	assert_eq!(buf.grapheme_indices(), &[]);
	assert!(buf.slice(0..0).is_none());
}

#[test]
fn linebuf_ascii_content() {
	let mut buf = LineBuf::new().with_initial("hello", 0);

	buf.update_graphemes_lazy();
	assert_eq!(buf.grapheme_indices(), &[0, 1, 2, 3, 4]);

	assert_eq!(buf.grapheme_at(0), Some("h"));
	assert_eq!(buf.grapheme_at(4), Some("o"));
	assert_eq!(buf.slice(1..4), Some("ell"));
	assert_eq!(buf.slice_to(2), Some("he"));
	assert_eq!(buf.slice_from(2), Some("llo"));
}

#[test]
fn linebuf_unicode_graphemes() {
	let mut buf = LineBuf::new().with_initial("a🇺🇸b́c", 0);

	buf.update_graphemes_lazy();
	let indices = buf.grapheme_indices();
	assert_eq!(indices.len(), 4); // 4 graphemes + 1 end marker

	assert_eq!(buf.grapheme_at(0), Some("a"));
	assert_eq!(buf.grapheme_at(1), Some("🇺🇸"));
	assert_eq!(buf.grapheme_at(2), Some("b́")); // b + combining accent
	assert_eq!(buf.grapheme_at(3), Some("c"));
	assert_eq!(buf.grapheme_at(4), None); // out of bounds

	assert_eq!(buf.slice(0..2), Some("a🇺🇸"));
	assert_eq!(buf.slice(1..3), Some("🇺🇸b́"));
	assert_eq!(buf.slice(2..4), Some("b́c"));
}

#[test]
fn linebuf_slice_to_from_cursor() {
	let mut buf = LineBuf::new().with_initial("abçd", 2);

	buf.update_graphemes_lazy();
	assert_eq!(buf.slice_to_cursor(), Some("ab"));
	assert_eq!(buf.slice_from_cursor(), Some("çd"));
}

#[test]
fn linebuf_out_of_bounds_slices() {
	let mut buf = LineBuf::new().with_initial("test", 0);

	buf.update_graphemes_lazy();

	assert_eq!(buf.grapheme_at(5), None); // out of bounds
	assert_eq!(buf.slice(2..5), None); // end out of bounds
	assert_eq!(buf.slice(4..4), None); // valid but empty
}

#[test]
fn linebuf_this_line() {
	let initial = "This is the first line\nThis is the second line\nThis is the third line\nThis is the fourth line";
	let mut buf = LineBuf::new().with_initial(initial, 57);
	let (start,end) = buf.this_line();
	assert_eq!(buf.slice(start..end), Some("This is the third line\n"))
}

#[test]
fn linebuf_prev_line() {
	let initial = "This is the first line\nThis is the second line\nThis is the third line\nThis is the fourth line";
	let mut buf = LineBuf::new().with_initial(initial, 57);
	let (start,end) = buf.nth_prev_line(1).unwrap();
	assert_eq!(buf.slice(start..end), Some("This is the second line\n"))
}

#[test]
fn linebuf_prev_line_first_line_is_empty() {
	let initial = "\nThis is the first line\nThis is the second line\nThis is the third line\nThis is the fourth line";
	let mut buf = LineBuf::new().with_initial(initial, 36);
	let (start,end) = buf.nth_prev_line(1).unwrap();
	assert_eq!(buf.slice(start..end), Some("This is the first line\n"))
}

#[test]
fn linebuf_next_line() {
	let initial = "This is the first line\nThis is the second line\nThis is the third line\nThis is the fourth line";
	let mut buf = LineBuf::new().with_initial(initial, 57);
	let (start,end) = buf.nth_next_line(1).unwrap();
	assert_eq!(buf.slice(start..end), Some("This is the fourth line"))
}

#[test]
fn linebuf_next_line_last_line_is_empty() {
	let initial = "This is the first line\nThis is the second line\nThis is the third line\nThis is the fourth line\n";
	let mut buf = LineBuf::new().with_initial(initial, 57);
	let (start,end) = buf.nth_next_line(1).unwrap();
	assert_eq!(buf.slice(start..end), Some("This is the fourth line\n"))
}

#[test]
fn linebuf_next_line_several_trailing_newlines() {
	let initial = "This is the first line\nThis is the second line\nThis is the third line\nThis is the fourth line\n\n\n\n";
	let mut buf = LineBuf::new().with_initial(initial, 81);
	let (start,end) = buf.nth_next_line(1).unwrap();
	assert_eq!(buf.slice(start..end), Some("\n"))
}

#[test]
fn linebuf_next_line_only_newlines() {
	let initial = "\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n";
	let mut buf = LineBuf::new().with_initial(initial, 7);
	let (start,end) = buf.nth_next_line(1).unwrap();
	assert_eq!(start, 8);
	assert_eq!(buf.slice(start..end), Some("\n"))
}

#[test]
fn linebuf_prev_line_only_newlines() {
	let initial = "\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n";
	let mut buf = LineBuf::new().with_initial(initial, 7);
	let (start,end) = buf.nth_prev_line(1).unwrap();
	assert_eq!(buf.slice(start..end), Some("\n"));
	assert_eq!(start, 6);
}

#[test]
fn linebuf_cursor_motion() {
	let mut buf = LineBuf::new().with_initial("Thé quíck 🦊 bröwn fóx jumpś óver the 💤 lázy dóg 🐶", 0);

	buf.update_graphemes_lazy();
	let total = buf.grapheme_indices.as_ref().unwrap().len();

	for i in 0..total {
		buf.cursor.set(i);

		let expected_to = buf.buffer.get(..buf.grapheme_indices_owned()[i]).unwrap_or("").to_string();
		let expected_from = if i + 1 < total {
			buf.buffer.get(buf.grapheme_indices_owned()[i]..).unwrap_or("").to_string()
		} else {
			// last grapheme, ends at buffer end
			buf.buffer.get(buf.grapheme_indices_owned()[i]..).unwrap_or("").to_string()
		};

		let expected_at = {
			let start = buf.grapheme_indices_owned()[i];
			let end = buf.grapheme_indices_owned().get(i + 1).copied().unwrap_or(buf.buffer.len());
			buf.buffer.get(start..end).map(|slice| slice.to_string())
		};

		assert_eq!(
			buf.slice_to_cursor(),
			Some(expected_to.as_str()),
			"Failed at cursor position {i}: slice_to_cursor"
		);
		assert_eq!(
			buf.slice_from_cursor(),
			Some(expected_from.as_str()),
			"Failed at cursor position {i}: slice_from_cursor"
		);
		assert_eq!(
			buf.grapheme_at(i).map(|slice| slice.to_string()),
			expected_at,
			"Failed at cursor position {i}: grapheme_at"
		);
	}
}

#[test]
fn editor_delete_word() {
	assert_eq!(normal_cmd(
		"dw",
		"The quick brown fox jumps over the lazy dog",
		16),
		("The quick brown jumps over the lazy dog".into(), 16)
	);
}

#[test]
fn editor_delete_backwards() {
	assert_eq!(normal_cmd(
		"2db",
		"The quick brown fox jumps over the lazy dog",
		16),
		("The fox jumps over the lazy dog".into(), 4)
	);
}

#[test]
fn editor_rot13_five_words_backwards() {
	assert_eq!(normal_cmd(
		"g?5b",
		"The quick brown fox jumps over the lazy dog",
		31),
		("The dhvpx oebja sbk whzcf bire the lazy dog".into(), 4)
	);
}

#[test]
fn editor_delete_word_on_whitespace() {
	assert_eq!(normal_cmd(
		"dw",
		"The quick  brown fox",
		10), //on the whitespace between "quick" and "brown"
		("The quick brown fox".into(), 10)
	);
}

#[test]
fn editor_delete_5_words() {
	assert_eq!(normal_cmd(
		"5dw",
		"The quick brown fox jumps over the lazy dog",
		16,),
		("The quick brown dog".into(), 16)
	);
}

#[test]
fn editor_delete_end_includes_last() {
	assert_eq!(normal_cmd(
		"de",
		"The quick brown fox::::jumps over the lazy dog",
		16),
		("The quick brown ::::jumps over the lazy dog".into(), 16)
	);
}

#[test]
fn editor_delete_end_unicode_word() {
	assert_eq!(normal_cmd(
		"de",
		"naïve café world",
		0),
		(" café world".into(), 0)
	);
}

#[test]
fn editor_inplace_edit_cursor_position() {
	assert_eq!(normal_cmd(
		"5~",
		"foobar",
		0),
		("FOOBAr".into(), 4)
	);
	assert_eq!(normal_cmd(
		"5rg",
		"foobar",
		0),
		("gggggr".into(), 4)
	);
}

#[test]
fn editor_insert_mode_not_clamped() {
	assert_eq!(normal_cmd(
		"a",
		"foobar",
		5),
		("foobar".into(), 6)
	)
}

#[test]
fn editor_overshooting_motions() {
	assert_eq!(normal_cmd(
		"5dw",
		"foo bar",
		0),
		("".into(), 0)
	);
	assert_eq!(normal_cmd(
		"3db",
		"foo bar",
		0),
		("foo bar".into(), 0)
	);
	assert_eq!(normal_cmd(
		"3dj",
		"foo bar",
		0),
		("foo bar".into(), 0)
	);
	assert_eq!(normal_cmd(
		"3dk",
		"foo bar",
		0),
		("foo bar".into(), 0)
	);
}

#[test]
fn editor_textobj_quoted() {
	assert_eq!(normal_cmd(
		"di\"",
		"this buffer has \"some \\\"quoted\" text",
		0),
		("this buffer has \"\" text".into(), 17)
	);
	assert_eq!(normal_cmd(
		"da\"",
		"this buffer has \"some \\\"quoted\" text",
		0),
		("this buffer has text".into(), 16)
	);
	assert_eq!(normal_cmd(
		"di'",
		"this buffer has 'some \\'quoted' text",
		0),
		("this buffer has '' text".into(), 17)
	);
	assert_eq!(normal_cmd(
		"da'",
		"this buffer has 'some \\'quoted' text",
		0),
		("this buffer has text".into(), 16)
	);
	assert_eq!(normal_cmd(
		"di`",
		"this buffer has `some \\`quoted` text",
		0),
		("this buffer has `` text".into(), 17)
	);
	assert_eq!(normal_cmd(
		"da`",
		"this buffer has `some \\`quoted` text",
		0),
		("this buffer has text".into(), 16)
	);
}

#[test]
fn editor_textobj_delimited() {
	assert_eq!(normal_cmd(
		"di)",
		"this buffer has (some \\(\\)(inner) \\(\\)delimited) text",
		0),
		("this buffer has () text".into(), 17)
	);
	assert_eq!(normal_cmd(
		"da)",
		"this buffer has (some \\(\\)(inner) \\(\\)delimited) text",
		0),
		("this buffer has text".into(), 16)
	);
	assert_eq!(normal_cmd(
		"di]",
		"this buffer has [some \\[\\][inner] \\[\\]delimited] text",
		0),
		("this buffer has [] text".into(), 17)
	);
	assert_eq!(normal_cmd(
		"da]",
		"this buffer has [some \\[\\][inner] \\[\\]delimited] text",
		0),
		("this buffer has text".into(), 16)
	);
	assert_eq!(normal_cmd(
		"di}",
		"this buffer has {some \\{\\}{inner} \\{\\}delimited} text",
		0),
		("this buffer has {} text".into(), 17)
	);
	assert_eq!(normal_cmd(
		"da}",
		"this buffer has {some \\{\\}{inner} \\{\\}delimited} text",
		0),
		("this buffer has text".into(), 16)
	);
	assert_eq!(normal_cmd(
		"di>",
		"this buffer has <some \\<\\><inner> \\<\\>delimited> text",
		0),
		("this buffer has <> text".into(), 17)
	);
	assert_eq!(normal_cmd(
		"da>",
		"this buffer has <some \\<\\><inner> \\<\\>delimited> text",
		0),
		("this buffer has text".into(), 16)
	);
}

const LOREM_IPSUM: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.\nUt enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat.\nDuis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur.\nExcepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.\nCurabitur pretium tincidunt lacus. Nulla gravida orci a odio. Nullam varius, turpis et commodo pharetra.";

#[test]
fn editor_delete_line_up() {
	assert_eq!(normal_cmd(
		"dk",
		LOREM_IPSUM,
		237),
		("Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.\nExcepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.\nCurabitur pretium tincidunt lacus. Nulla gravida orci a odio. Nullam varius, turpis et commodo pharetra.".into(), 240,)
	)
}

#[test]
fn fernvi_test_simple() {
	assert_eq!(fernvi_test(
		"foo bar\x1bbdw\r",
		""),
		"foo "
	)
}

#[test]
fn fernvi_test_mode_change() {
	assert_eq!(fernvi_test(
		"foo bar biz buzz\x1bbbb2cwbiz buzz bar\r",
		""),
		"foo biz buzz bar buzz"
	)
}

#[test]
fn fernvi_test_lorem_ipsum_1() {
	assert_eq!(fernvi_test(
			"\x1bwwwwwwww5dWdBdBjjdwjdwbbbcwasdasdasdasd\x1b\r",
			LOREM_IPSUM),
			"Lorem ipsum dolor sit amet, incididunt ut labore et dolore magna aliqua.\nUt enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat.\nDuis aute irure dolor in repin voluptate velit esse cillum dolore eu fugiat nulla pariatur.\nExcepteur asdasdasdasd occaecat cupinon proident, sunt in culpa qui officia deserunt mollit anim id est laborum.\nCurabitur pretium tincidunt lacus. Nulla gravida orci a odio. Nullam varius, turpis et commodo pharetra."
	)
}

#[test]
fn fernvi_test_lorem_ipsum_undo() {
	assert_eq!(fernvi_test(
			"\x1bwwwwwwwwainserting some characters now...\x1bu\r",
			LOREM_IPSUM),
			LOREM_IPSUM
	)
}

#[test]
fn fernvi_test_lorem_ipsum_ctrl_w() {
	assert_eq!(fernvi_test(
			"\x1bj5wiasdasdkjhaksjdhkajshd\x17wordswordswords\x17somemorewords\x17\x1b[D\x1b[D\x17\x1b\r",
			LOREM_IPSUM),
			"Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.\nUt enim ad minim am, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat.\nDuis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur.\nExcepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.\nCurabitur pretium tincidunt lacus. Nulla gravida orci a odio. Nullam varius, turpis et commodo pharetra."
	)
}
