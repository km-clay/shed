use std::{
  path::Path,
  time::{Duration, SystemTime, UNIX_EPOCH},
};

use regex::Regex;
use serde_json::Value;

use crate::{procio::bytes_to_string, util::random};

use super::{
  history::HistEntry,
  match_loop, sherr,
  util::{ShResult, ends_with_unescaped},
};

pub fn import_history<P: AsRef<Path>>(path: P) -> ShResult<Vec<HistEntry>> {
  let path = path.as_ref();
  let content = std::fs::read(path)
    .map(bytes_to_string)
    .map_err(|e| sherr!(ParseErr, "Failed to read history file: {e}"))?;

  if let Ok(val) = serde_json::from_str::<Value>(&content) {
    return import_json(&val);
  }

  let filename = path
    .file_name()
    .and_then(|n| n.to_str())
    .ok_or_else(|| sherr!(ParseErr, "Invalid history file name"))?;

  // Known filenames try the matching importer first, then fall back through
  // the chain if the file's content doesn't actually carry that format's
  // markers. Bash is the catch-all (any text is a valid bash history).
  match filename {
    ".bash_history" => Ok(try_import_bash(&content)),
    ".zsh_history" => Ok(try_import_zsh(&content).unwrap_or_else(|_| try_import_bash(&content))),
    "fish_history" => Ok(try_import_fish(&content).unwrap_or_else(|_| try_import_bash(&content))),
    _ => Ok(
      try_import_zsh(&content)
        .or_else(|_| try_import_fish(&content))
        .unwrap_or_else(|_| try_import_bash(&content)),
    ),
  }
}

fn import_json(content: &Value) -> ShResult<Vec<HistEntry>> {
  let obj = content
    .as_object()
    .ok_or_else(|| sherr!(ParseErr, "JSON history is not an object"))?;
  let mut entries: Vec<(usize, HistEntry)> = vec![];

  for (key, val) in obj {
    let val_obj = val
      .as_object()
      .ok_or_else(|| sherr!(ParseErr, "JSON history entry '{key}' is not an object"))?;

    let command = val_obj
      .get("command")
      .and_then(|v| v.as_str())
      .ok_or_else(|| {
        sherr!(
          ParseErr,
          "JSON history entry '{key}' missing 'command' string"
        )
      })?;
    let cwd = val_obj
      .get("cwd")
      .and_then(|v| v.as_str())
      .ok_or_else(|| sherr!(ParseErr, "JSON history entry '{key}' missing 'cwd' string"))?;
    let runtime = val_obj
      .get("runtime")
      .and_then(serde_json::Value::as_u64)
      .ok_or_else(|| sherr!(ParseErr, "JSON history entry '{key}' missing 'runtime' u64"))?;
    let status = val_obj
      .get("status")
      .and_then(serde_json::Value::as_i64)
      .ok_or_else(|| sherr!(ParseErr, "JSON history entry '{key}' missing 'status' i64"))?;
    let timestamp = val_obj
      .get("timestamp")
      .and_then(serde_json::Value::as_u64)
      .ok_or_else(|| {
        sherr!(
          ParseErr,
          "JSON history entry '{key}' missing 'timestamp' u64"
        )
      })?;
    let token = val_obj
      .get("token")
      .and_then(|v| v.as_str())
      .and_then(|v| v.parse::<random::Uuid>().ok())
      .ok_or_else(|| {
        sherr!(
          ParseErr,
          "JSON history entry '{key}' missing 'token' string"
        )
      })?;

    let id = key
      .parse::<usize>()
      .map_err(|_| sherr!(ParseErr, "JSON history entry key '{key}' is not a valid id"))?;

    let entry = HistEntry {
      command: command.into(),
      cwd: cwd.into(),
      runtime: Duration::from_micros(runtime),
      status: status as i32,
      timestamp: UNIX_EPOCH + Duration::from_secs(timestamp),
      token,
    };

    entries.push((id, entry));
  }

  entries.sort_by_key(|(id, _)| *id);

  Ok(entries.into_iter().map(|(_, entry)| entry).collect())
}

fn try_import_bash(content: &str) -> Vec<HistEntry> {
  let mut lines = content.lines().peekable();
  let mut entries = vec![];
  let timestamp_pat = Regex::new(r"^#(\d+)$").unwrap();

  while let Some(line) = lines.next() {
    if let Some(caps) = timestamp_pat.captures(line) {
      let secs: u64 = caps[1].parse().unwrap_or(0);
      let timestamp = UNIX_EPOCH + Duration::from_secs(secs);
      if let Some(cmd) = lines.next() {
        entries.push(HistEntry {
          timestamp,
          command: cmd.into(),
          ..HistEntry::default()
        });
      }
    } else {
      entries.push(HistEntry {
        command: line.into(),
        ..HistEntry::default()
      });
    }
  }

  entries
}

fn collect_continuation<'a>(first: &'a str, lines: &mut impl Iterator<Item = &'a str>) -> String {
  let mut parts = vec![];
  let mut line = first;
  loop {
    let cont = ends_with_unescaped(line.as_bytes(), b"\\");
    parts.push(if cont {
      line.strip_suffix('\\').unwrap()
    } else {
      line
    });
    if !cont {
      break;
    }
    if let Some(next) = lines.next() {
      line = next;
    } else {
      break;
    }
  }
  parts.join("\n")
}

/// Returns true if `line` looks like a zsh extended-history header:
/// `: <epoch>:<elapsed>;<command>`.
fn looks_like_zsh_header(line: &str) -> bool {
  let Some(rest) = line.strip_prefix(": ") else {
    return false;
  };
  let Some((meta, _cmd)) = rest.split_once(';') else {
    return false;
  };
  let Some((ts, elapsed)) = meta.split_once(':') else {
    return false;
  };
  ts.parse::<u64>().is_ok() && elapsed.parse::<u64>().is_ok()
}

fn try_import_zsh(content: &str) -> ShResult<Vec<HistEntry>> {
  if !content.lines().any(looks_like_zsh_header) {
    return Err(sherr!(ParseErr, "no zsh extended-history markers found"));
  }

  let mut lines = content.lines().peekable();
  let mut entries = vec![];

  while let Some(line) = lines.next() {
    if line.starts_with(": ")
      && let Some((meta, cmd_start)) = &line[2..].split_once(';')
    {
      let timestamp = meta
        .split_once(':')
        .and_then(|(ts, _)| ts.parse::<u64>().ok())
        .map_or(SystemTime::now(), |secs| {
          UNIX_EPOCH + Duration::from_secs(secs)
        });

      entries.push(HistEntry {
        timestamp,
        command: collect_continuation(cmd_start, &mut lines),
        ..HistEntry::default()
      });
    } else {
      entries.push(HistEntry {
        command: collect_continuation(line, &mut lines),
        ..HistEntry::default()
      });
    }
  }

  Ok(entries)
}

fn expand_fish_cmd(cmd: &str) -> String {
  let mut out = String::new();
  let mut chars = cmd.chars();

  match_loop!(chars.next() => ch, {
    '\\' => {
      let Some(esc_ch) = chars.next() else {
        out.push('\\');
        break;
      };
      match esc_ch {
        'n' => out.push('\n'),
        '\\' => out.push('\\'),
        _ => {
          out.push('\\');
          out.push(esc_ch);
        }
      }
    }
    _ => out.push(ch)
  });

  out
}

fn try_import_fish(content: &str) -> ShResult<Vec<HistEntry>> {
  if !content.lines().any(|l| l.starts_with("- cmd: ")) {
    return Err(sherr!(ParseErr, "no fish history markers found"));
  }

  let mut entries = vec![];
  let mut lines = content.lines();

  while let Some(line) = lines.next() {
    if let Some(cmd) = line.strip_prefix("- cmd: ") {
      let timestamp = lines
        .next()
        .and_then(|l| l.trim().strip_prefix("when: "))
        .and_then(|ts| ts.parse::<u64>().ok())
        .map_or(SystemTime::now(), |secs| {
          UNIX_EPOCH + Duration::from_secs(secs)
        });

      entries.push(HistEntry {
        timestamp,
        command: expand_fish_cmd(cmd),
        ..HistEntry::default()
      });
    }
  }

  Ok(entries)
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::io::Write;

  /// Write `content` to a tempfile with a chosen file name; returns
  /// (`TempDir` guard, full path).
  fn write_hist_file(name: &str, content: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join(name);
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(content.as_bytes()).unwrap();
    (dir, path)
  }

  fn secs_since_epoch(ts: SystemTime) -> u64 {
    ts.duration_since(UNIX_EPOCH).unwrap().as_secs()
  }

  // ===================== try_import_bash =====================

  #[test]
  fn bash_unprefixed_lines_become_entries_without_timestamps() {
    let content = "echo one\necho two\necho three\n";
    let entries = try_import_bash(content);
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].command, "echo one");
    assert_eq!(entries[1].command, "echo two");
    assert_eq!(entries[2].command, "echo three");
  }

  #[test]
  fn bash_timestamp_comment_attaches_to_next_line() {
    let content = "#1700000000\necho stamped\n";
    let entries = try_import_bash(content);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].command, "echo stamped");
    assert_eq!(secs_since_epoch(entries[0].timestamp), 1_700_000_000);
  }

  #[test]
  fn bash_timestamp_at_end_with_no_following_command_is_dropped() {
    // Pin the current behavior: a trailing `#N` with nothing after
    // simply produces no entry (the `if let Some(cmd) = lines.next()`
    // sees None).
    let content = "echo first\n#1700000000\n";
    let entries = try_import_bash(content);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].command, "echo first");
  }

  #[test]
  fn bash_mixed_timestamped_and_plain_lines() {
    let content = "echo a\n#1700000100\necho b\necho c\n";
    let entries = try_import_bash(content);
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].command, "echo a");
    assert_eq!(entries[1].command, "echo b");
    assert_eq!(secs_since_epoch(entries[1].timestamp), 1_700_000_100);
    assert_eq!(entries[2].command, "echo c");
  }

  // ===================== try_import_zsh =====================

  #[test]
  fn zsh_extended_format_parses_timestamp_and_command() {
    let content = ": 1700000200:0;echo zsh_one\n";
    let entries = try_import_zsh(content).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].command, "echo zsh_one");
    assert_eq!(secs_since_epoch(entries[0].timestamp), 1_700_000_200);
  }

  #[test]
  fn zsh_backslash_continuation_joins_lines() {
    // A trailing unescaped `\` continues the command to the next line.
    let content = ": 1700000300:0;echo first \\\necho second\n";
    let entries = try_import_zsh(content).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].command, "echo first \necho second");
  }

  #[test]
  fn zsh_malformed_timestamp_within_valid_file_falls_back_to_now() {
    // The unwrap_or(SystemTime::now()) branch fires when timestamp
    // parsing fails *inside* a file that's already been recognized as
    // zsh (via at least one valid header). The bad-timestamp entry is
    // still produced.
    let content = ": 1700000000:0;echo good\n: notanumber:0;echo bad\n";
    let entries = try_import_zsh(content).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].command, "echo good");
    assert_eq!(entries[1].command, "echo bad");
  }

  // ===================== try_import_fish =====================

  #[test]
  fn fish_cmd_and_when_pair_produces_entry() {
    let content = "- cmd: echo fish_one\n  when: 1700000400\n";
    let entries = try_import_fish(content).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].command, "echo fish_one");
    assert_eq!(secs_since_epoch(entries[0].timestamp), 1_700_000_400);
  }

  #[test]
  fn fish_cmd_without_when_falls_back_to_now() {
    let content = "- cmd: echo no_when\nother junk\n";
    let entries = try_import_fish(content).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].command, "echo no_when");
  }

  #[test]
  fn fish_multiple_entries_parsed_in_order() {
    let content = "- cmd: one\n  when: 100\n- cmd: two\n  when: 200\n- cmd: three\n  when: 300\n";
    let entries = try_import_fish(content).unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].command, "one");
    assert_eq!(entries[1].command, "two");
    assert_eq!(entries[2].command, "three");
  }

  #[test]
  fn fish_lines_without_cmd_prefix_are_skipped() {
    // Random metadata interspersed with entries.
    let content = "# header\n- cmd: real\n  when: 100\n  paths:\n    - foo\n";
    let entries = try_import_fish(content).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].command, "real");
  }

  // ===================== expand_fish_cmd =====================

  #[test]
  fn expand_fish_cmd_passthrough() {
    assert_eq!(expand_fish_cmd("plain text"), "plain text");
  }

  #[test]
  fn expand_fish_cmd_backslash_n_becomes_newline() {
    assert_eq!(expand_fish_cmd("a\\nb"), "a\nb");
  }

  #[test]
  fn expand_fish_cmd_escaped_backslash_collapses_to_single() {
    assert_eq!(expand_fish_cmd("a\\\\b"), "a\\b");
  }

  #[test]
  fn expand_fish_cmd_unknown_escape_preserved_verbatim() {
    assert_eq!(expand_fish_cmd("a\\xb"), "a\\xb");
  }

  #[test]
  fn expand_fish_cmd_trailing_backslash_kept_as_literal() {
    // Backslash at end of string — chars.next() returns None, the
    // function pushes the lone backslash and breaks.
    assert_eq!(expand_fish_cmd("foo\\"), "foo\\");
  }

  // ===================== collect_continuation =====================

  #[test]
  fn collect_continuation_returns_single_line_unchanged() {
    let mut lines = std::iter::empty::<&str>();
    assert_eq!(
      collect_continuation("simple line", &mut lines),
      "simple line"
    );
  }

  #[test]
  fn collect_continuation_joins_escaped_lines() {
    let rest = vec!["second", "third"];
    let mut iter = rest.into_iter();
    let result = collect_continuation("first\\", &mut iter);
    // First line had a trailing `\` so the next iter line joins on a
    // newline; the second line doesn't end with `\` so we stop.
    assert_eq!(result, "first\nsecond");
  }

  #[test]
  fn collect_continuation_stops_at_iter_exhaustion() {
    // Trailing `\` but no more lines — the loop breaks via the
    // `if let Some(next) = ... else { break }` arm.
    let mut iter = std::iter::empty::<&str>();
    let result = collect_continuation("only\\", &mut iter);
    assert_eq!(result, "only");
  }

  #[test]
  fn collect_continuation_preserves_escaped_trailing_backslash() {
    // The physical line ends in an *escaped* backslash (`foo\\`), which is not a
    // line continuation. The final `\` must be kept, and no further lines are
    // consumed. (Regression: the old unconditional `strip_suffix('\\')` dropped
    // the last backslash, corrupting the imported command.)
    let rest = vec!["should_not_be_joined"];
    let mut iter = rest.into_iter();
    let result = collect_continuation("printf '%s' foo\\\\", &mut iter);
    assert_eq!(result, "printf '%s' foo\\\\");
    // The follow-up line was left untouched in the iterator.
    assert_eq!(iter.next(), Some("should_not_be_joined"));
  }

  // ===================== import_history dispatch =====================

  #[test]
  fn dispatch_by_bash_history_filename() {
    let (_dir, path) = write_hist_file(".bash_history", "echo bash_entry\n");
    let entries = import_history(path).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].command, "echo bash_entry");
  }

  #[test]
  fn dispatch_by_zsh_history_filename() {
    let (_dir, path) = write_hist_file(".zsh_history", ": 1700000500:0;echo zsh_entry\n");
    let entries = import_history(path).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].command, "echo zsh_entry");
  }

  #[test]
  fn dispatch_by_fish_history_filename() {
    let (_dir, path) = write_hist_file(
      "fish_history",
      "- cmd: echo fish_entry\n  when: 1700000600\n",
    );
    let entries = import_history(path).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].command, "echo fish_entry");
  }

  #[test]
  fn dispatch_unknown_filename_tries_zsh_then_fish_then_bash() {
    // Unknown filenames try zsh, then fish, then bash. zsh and fish
    // refuse content without their format markers, so plain-text input
    // falls through to bash.
    let (_dir, path) = write_hist_file("random_name", "echo fallback\n");
    let entries = import_history(path).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].command, "echo fallback");
  }

  #[test]
  fn dispatch_unknown_filename_with_zsh_markers_uses_zsh() {
    // Content with zsh extended-history markers should be detected and
    // parsed as zsh, not as bash, even with an unknown filename.
    let (_dir, path) = write_hist_file("random_name", ": 1700000000:0;echo from_zsh\n");
    let entries = import_history(path).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].command, "echo from_zsh");
    assert_eq!(secs_since_epoch(entries[0].timestamp), 1_700_000_000);
  }

  #[test]
  fn dispatch_unknown_filename_with_fish_markers_uses_fish() {
    let (_dir, path) =
      write_hist_file("random_name", "- cmd: echo from_fish\n  when: 1700000000\n");
    let entries = import_history(path).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].command, "echo from_fish");
  }

  #[test]
  fn zsh_filename_with_bash_content_falls_back_to_bash() {
    // If the user names a file `.zsh_history` but it actually contains
    // plain bash-style history (no extended markers), we should still
    // import it correctly via the bash fallback.
    let (_dir, path) = write_hist_file(".zsh_history", "echo no_markers\n");
    let entries = import_history(path).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].command, "echo no_markers");
  }

  #[test]
  fn try_import_zsh_errors_without_markers() {
    assert!(try_import_zsh("echo just_bash\n").is_err());
  }

  #[test]
  fn try_import_fish_errors_without_markers() {
    assert!(try_import_fish("echo just_bash\n").is_err());
  }

  #[test]
  fn missing_file_errors() {
    let path = std::path::PathBuf::from("/path/that/definitely/does/not/exist/zzz");
    assert!(import_history(path).is_err());
  }
}
