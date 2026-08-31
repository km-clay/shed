//! A `log`-crate backend that routes log records into the shell's message output.

use super::ui;
use crate::{sherr, system_msg, util::error::ShResult, var};

pub(crate) fn init() -> ShResult<()> {
  log::set_logger(&Flog).map_err(|e| sherr!(InternalErr, "Failed to set logger: {e}"))?;
  update_log_level();
  Ok(())
}

pub(crate) fn update_log_level() {
  let level_raw = var!("SHED_LOG");
  let level = level_raw
    .to_str_lossy()
    .parse::<log::LevelFilter>()
    .unwrap_or(log::LevelFilter::Error);
  log::set_max_level(level);
}

struct Flog;
impl log::Log for Flog {
  fn enabled(&self, metadata: &log::Metadata) -> bool {
    metadata.level() <= log::max_level()
  }
  fn log(&self, record: &log::Record) {
    if !self.enabled(record.metadata()) {
      return;
    }

    let level = ui::stylize_loglevel(record.level());
    let args = record.args();

    let line = if let Some(file) = record.file()
      && let Some(line) = record.line()
    {
      format!("[{level} {file}:{line}] {args}")
    } else {
      format!("[{level}] {args}")
    };

    system_msg!("{line}");
  }
  fn flush(&self) {}
}
