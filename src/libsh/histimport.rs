use std::{
  path::PathBuf,
  time::{Duration, SystemTime, UNIX_EPOCH},
};

use regex::Regex;

use crate::{
  libsh::{error::ShResult, strops::ends_with_unescaped},
  match_loop,
  readline::history::HistEntry,
  sherr,
};

pub fn import_history(path: PathBuf) -> ShResult<Vec<HistEntry>> {
  let content = std::fs::read(&path)
    .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
    .map_err(|e| sherr!(ParseErr, "Failed to read history file: {e}"))?;

  let filename = path
    .file_name()
    .and_then(|n| n.to_str())
    .ok_or_else(|| sherr!(ParseErr, "Invalid history file name"))?;

  match filename {
    ".bash_history" => try_import_bash(&content),
    ".zsh_history" => try_import_zsh(&content),
    "fish_history" => try_import_fish(&content),
    _ => try_import_bash(&content)
      .or_else(|_| try_import_zsh(&content))
      .or_else(|_| try_import_fish(&content))
      .map_err(|_| sherr!(ParseErr, "Unknown history format")),
  }
}

fn try_import_bash(content: &str) -> ShResult<Vec<HistEntry>> {
  let mut lines = content.lines().peekable();
  let mut entries = vec![];
  let timestamp_pat = Regex::new(r#"^#(\d+)$"#).unwrap();

  while let Some(line) = lines.next() {
    if let Some(caps) = timestamp_pat.captures(line) {
      let secs: u64 = caps[1].parse().unwrap_or(0);
      let timestamp = UNIX_EPOCH + Duration::from_secs(secs);
      if let Some(cmd) = lines.next() {
        entries.push(HistEntry {
          timestamp,
          command: cmd.to_string(),
          ..HistEntry::default()
        });
      }
    } else {
      entries.push(HistEntry {
        command: line.to_string(),
        ..HistEntry::default()
      });
    }
  }

  Ok(entries)
}

fn collect_continuation<'a>(first: &'a str, lines: &mut impl Iterator<Item = &'a str>) -> String {
  let mut parts = vec![];
  let mut line = first;
  loop {
    parts.push(line.strip_suffix('\\').unwrap_or(line));
    if !ends_with_unescaped(line, "\\") {
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

fn try_import_zsh(content: &str) -> ShResult<Vec<HistEntry>> {
  let mut lines = content.lines().peekable();
  let mut entries = vec![];

  while let Some(line) = lines.next() {
    if line.starts_with(": ")
      && let Some((meta, cmd_start)) = &line[2..].split_once(';')
    {
      let timestamp = meta
        .split_once(':')
        .and_then(|(ts, _)| ts.parse::<u64>().ok())
        .map(|secs| UNIX_EPOCH + Duration::from_secs(secs))
        .unwrap_or(SystemTime::now());

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
  let mut entries = vec![];
  let mut lines = content.lines();

  while let Some(line) = lines.next() {
    if let Some(cmd) = line.strip_prefix("- cmd: ") {
      let timestamp = lines
        .next()
        .and_then(|l| l.trim().strip_prefix("when: "))
        .and_then(|ts| ts.parse::<u64>().ok())
        .map(|secs| UNIX_EPOCH + Duration::from_secs(secs))
        .unwrap_or(SystemTime::now());

      entries.push(HistEntry {
        timestamp,
        command: expand_fish_cmd(cmd),
        ..HistEntry::default()
      });
    }
  }

  Ok(entries)
}
