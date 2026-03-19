use std::{fmt::Display, sync::Mutex};

use crate::readline::linebuf::Line;

pub static REGISTERS: Mutex<Registers> = Mutex::new(Registers::new());

#[cfg(test)]
pub static SAVED_REGISTERS: Mutex<Option<Registers>> = Mutex::new(None);

#[cfg(test)]
pub fn save_registers() {
  let mut saved = SAVED_REGISTERS.lock().unwrap();
  *saved = Some(REGISTERS.lock().unwrap().clone());
}

#[cfg(test)]
pub fn restore_registers() {
  let mut saved = SAVED_REGISTERS.lock().unwrap();
  if let Some(ref registers) = *saved {
    *REGISTERS.lock().unwrap() = registers.clone();
  }
  *saved = None;
}

pub fn read_register(ch: Option<char>) -> Option<RegisterContent> {
  let lock = REGISTERS.lock().unwrap();
  lock.get_reg(ch).map(|r| r.content().clone())
}

pub fn write_register(ch: Option<char>, buf: RegisterContent) {
  let mut lock = REGISTERS.lock().unwrap();
  if let Some(r) = lock.get_reg_mut(ch) {
    r.write(buf)
  }
}

pub fn append_register(ch: Option<char>, buf: RegisterContent) {
  let mut lock = REGISTERS.lock().unwrap();
  if let Some(r) = lock.get_reg_mut(ch) {
    r.append(buf)
  }
}

#[derive(Default, Clone, Debug)]
pub enum RegisterContent {
  Span(Vec<Line>),
  Line(Vec<Line>),
  Block(Vec<Line>),
  #[default]
  Empty,
}

impl Display for RegisterContent {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Block(s) | Self::Line(s) | Self::Span(s) => {
        write!(
          f,
          "{}",
          s.iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n")
        )
      }
      Self::Empty => write!(f, ""),
    }
  }
}

impl RegisterContent {
  pub fn clear(&mut self) {
    *self = Self::Empty
  }
  pub fn len(&self) -> usize {
    match self {
      Self::Span(s) | Self::Line(s) | Self::Block(s) => s.len(),
      Self::Empty => 0,
    }
  }
  pub fn is_empty(&self) -> bool {
    match self {
      Self::Span(s) => s.is_empty(),
      Self::Line(s) => s.is_empty(),
      Self::Block(s) => s.is_empty(),
      Self::Empty => true,
    }
  }
  pub fn is_block(&self) -> bool {
    matches!(self, Self::Block(_))
  }
  pub fn is_line(&self) -> bool {
    matches!(self, Self::Line(_))
  }
  pub fn is_span(&self) -> bool {
    matches!(self, Self::Span(_))
  }
  pub fn char_count(&self) -> usize {
    self.to_string().chars().count()
  }
}

#[derive(Default, Clone, Debug)]
pub struct Registers {
  default: Register,
  a: Register,
  b: Register,
  c: Register,
  d: Register,
  e: Register,
  f: Register,
  g: Register,
  h: Register,
  i: Register,
  j: Register,
  k: Register,
  l: Register,
  m: Register,
  n: Register,
  o: Register,
  p: Register,
  q: Register,
  r: Register,
  s: Register,
  t: Register,
  u: Register,
  v: Register,
  w: Register,
  x: Register,
  y: Register,
  z: Register,
}

impl Registers {
  pub const fn new() -> Self {
    Self {
      default: Register::new(),
      a: Register::new(),
      b: Register::new(),
      c: Register::new(),
      d: Register::new(),
      e: Register::new(),
      f: Register::new(),
      g: Register::new(),
      h: Register::new(),
      i: Register::new(),
      j: Register::new(),
      k: Register::new(),
      l: Register::new(),
      m: Register::new(),
      n: Register::new(),
      o: Register::new(),
      p: Register::new(),
      q: Register::new(),
      r: Register::new(),
      s: Register::new(),
      t: Register::new(),
      u: Register::new(),
      v: Register::new(),
      w: Register::new(),
      x: Register::new(),
      y: Register::new(),
      z: Register::new(),
    }
  }
  pub fn get_reg(&self, ch: Option<char>) -> Option<&Register> {
    let Some(ch) = ch else {
      return Some(&self.default);
    };
    match ch {
      'a' => Some(&self.a),
      'b' => Some(&self.b),
      'c' => Some(&self.c),
      'd' => Some(&self.d),
      'e' => Some(&self.e),
      'f' => Some(&self.f),
      'g' => Some(&self.g),
      'h' => Some(&self.h),
      'i' => Some(&self.i),
      'j' => Some(&self.j),
      'k' => Some(&self.k),
      'l' => Some(&self.l),
      'm' => Some(&self.m),
      'n' => Some(&self.n),
      'o' => Some(&self.o),
      'p' => Some(&self.p),
      'q' => Some(&self.q),
      'r' => Some(&self.r),
      's' => Some(&self.s),
      't' => Some(&self.t),
      'u' => Some(&self.u),
      'v' => Some(&self.v),
      'w' => Some(&self.w),
      'x' => Some(&self.x),
      'y' => Some(&self.y),
      'z' => Some(&self.z),
      _ => None,
    }
  }
  pub fn get_reg_mut(&mut self, ch: Option<char>) -> Option<&mut Register> {
    let Some(ch) = ch else {
      return Some(&mut self.default);
    };
    match ch {
      'a' => Some(&mut self.a),
      'b' => Some(&mut self.b),
      'c' => Some(&mut self.c),
      'd' => Some(&mut self.d),
      'e' => Some(&mut self.e),
      'f' => Some(&mut self.f),
      'g' => Some(&mut self.g),
      'h' => Some(&mut self.h),
      'i' => Some(&mut self.i),
      'j' => Some(&mut self.j),
      'k' => Some(&mut self.k),
      'l' => Some(&mut self.l),
      'm' => Some(&mut self.m),
      'n' => Some(&mut self.n),
      'o' => Some(&mut self.o),
      'p' => Some(&mut self.p),
      'q' => Some(&mut self.q),
      'r' => Some(&mut self.r),
      's' => Some(&mut self.s),
      't' => Some(&mut self.t),
      'u' => Some(&mut self.u),
      'v' => Some(&mut self.v),
      'w' => Some(&mut self.w),
      'x' => Some(&mut self.x),
      'y' => Some(&mut self.y),
      'z' => Some(&mut self.z),
      _ => None,
    }
  }
}

#[derive(Clone, Default, Debug)]
pub struct Register {
  content: RegisterContent,
}

impl Register {
  pub const fn new() -> Self {
    Self {
      content: RegisterContent::Empty,
    }
  }
  pub fn content(&self) -> &RegisterContent {
    &self.content
  }
  pub fn write(&mut self, buf: RegisterContent) {
    self.content = buf
  }
  pub fn append(&mut self, mut buf: RegisterContent) {
    match buf {
      RegisterContent::Empty => {}
      RegisterContent::Span(ref mut s)
      | RegisterContent::Block(ref mut s)
      | RegisterContent::Line(ref mut s) => match &mut self.content {
        RegisterContent::Empty => self.content = buf,
        RegisterContent::Span(existing)
        | RegisterContent::Line(existing)
        | RegisterContent::Block(existing) => existing.append(s),
      },
    }
  }
  pub fn clear(&mut self) {
    self.content.clear()
  }
  pub fn is_line(&self) -> bool {
    self.content.is_line()
  }
  pub fn is_span(&self) -> bool {
    self.content.is_span()
  }
}
