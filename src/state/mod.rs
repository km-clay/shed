use std::{
  cell::RefCell,
  sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, AtomicI32},
  },
};

pub mod scopes;
use rusqlite::Connection;
pub use scopes::*;
pub mod logic;
pub use logic::*;
pub mod vars;
pub use vars::*;
pub mod util;
pub use util::*;
pub mod meta;
pub use meta::*;
pub mod jobs;
pub use jobs::*;
pub mod terminal;
pub use terminal::*;

use crate::{shopt::ShOpts, util::error::ShErr};

pub static INTERACTIVE: AtomicBool = AtomicBool::new(false);
pub static STATUS_CODE: AtomicI32 = AtomicI32::new(0);

#[derive(Debug)]
pub struct Shed {
  // constructed in state/util.rs
  pub jobs: RefCell<JobTab>,
  pub var_scopes: RefCell<ScopeStack>,
  pub meta: RefCell<MetaTab>,
  pub logic: RefCell<LogTab>,
  pub shopts: RefCell<ShOpts>,
  pub db_conn: OnceLock<Option<Arc<Connection>>>,
  pub terminal: RefCell<Terminal>,

  #[cfg(test)]
  saved: RefCell<Option<Box<Self>>>,
}

impl Shed {
  pub fn new() -> Self {
    Self {
      jobs: RefCell::new(JobTab::new()),
      var_scopes: RefCell::new(ScopeStack::new()),
      meta: RefCell::new(MetaTab::new()),
      logic: RefCell::new(LogTab::new()),
      shopts: RefCell::new(ShOpts::default()),
      db_conn: OnceLock::new(),
      terminal: RefCell::new(Terminal::new()),

      #[cfg(test)]
      saved: RefCell::new(None),
    }
  }

  #[cfg(test)]
  fn clone_db_conn(&self) -> OnceLock<Option<Arc<Connection>>> {
    let lock = OnceLock::new();
    if let Some(val) = self.db_conn.get() {
      let _ = lock.set(val.clone());
    }
    lock
  }
}

impl Default for Shed {
  fn default() -> Self {
    Self::new()
  }
}

#[cfg(test)]
impl Shed {
  pub fn save(&self) {
    let saved = Self {
      jobs: RefCell::new(self.jobs.borrow().clone()),
      var_scopes: RefCell::new(self.var_scopes.borrow().clone()),
      meta: RefCell::new(self.meta.borrow().clone()),
      logic: RefCell::new(self.logic.borrow().clone()),
      shopts: RefCell::new(self.shopts.borrow().clone()),
      db_conn: self.clone_db_conn(),
      terminal: RefCell::new(self.terminal.borrow().clone()),
      saved: RefCell::new(None),
    };
    *self.saved.borrow_mut() = Some(Box::new(saved));
  }

  pub fn restore(&self) {
    if let Some(saved) = self.saved.take() {
      *self.jobs.borrow_mut() = saved.jobs.into_inner();
      *self.var_scopes.borrow_mut() = saved.var_scopes.into_inner();
      *self.meta.borrow_mut() = saved.meta.into_inner();
      *self.logic.borrow_mut() = saved.logic.into_inner();
      *self.shopts.borrow_mut() = saved.shopts.into_inner();
    }
  }
}
