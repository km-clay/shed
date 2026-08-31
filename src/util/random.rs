use std::{cell::Cell, fmt::Display, ops::Range, str::FromStr};

use crate::{
  sherr,
  util::{
    error::{ShErr, ShResult},
    strops::{ByteCursor, SliceCursor},
  },
};

thread_local! {
  static RNG: Cell<u64> = Cell::new(os_random());
}

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Fills the given buffer with random bytes from the OS's random number generator.
///
/// Uses a syscall internally, should be used only if we need cryptographic safety.
#[cfg(linux_like)]
pub(crate) fn os_fill(buf: &mut [u8]) -> ShResult<()> {
  let mut filled = 0;
  while filled < buf.len() {
    let remaining = buf.len() - filled;
    let res = unsafe { nix::libc::getrandom(buf[filled..].as_mut_ptr().cast(), remaining, 0) };
    if res < 0 {
      let e = std::io::Error::last_os_error();
      if e.raw_os_error() == Some(nix::libc::EINTR) {
        continue;
      }
      return Err(e.into());
    }
    filled += res as usize;
  }
  Ok(())
}

/// Fills the given buffer with random bytes from the OS's random number generator.
///
/// Uses a syscall internally, should be used only if we need cryptographic safety.
#[cfg(not(linux_like))]
#[expect(clippy::unnecessary_wraps)] // signature matches the Linux variant
pub(crate) fn os_fill(buf: &mut [u8]) -> ShResult<()> {
  unsafe { nix::libc::arc4random_buf(buf.as_mut_ptr().cast(), buf.len()) };
  Ok(())
}

pub(crate) trait OsRandom {
  fn get_os_random() -> Self;
}

pub fn os_random<T: OsRandom>() -> T {
  T::get_os_random()
}

macro_rules! impl_os_randint {
  ($($t:ty),*) => {$(
    impl OsRandom for $t {
      fn get_os_random() -> Self {
        let mut b = [0u8; size_of::<$t>()];
        os_fill(&mut b).expect("failed to get random bytes");
        <$t>::from_ne_bytes(b)
      }
    }
  )*};
}

impl_os_randint!(
  u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize
);

impl OsRandom for bool {
  fn get_os_random() -> Self {
    u8::get_os_random() & 1 == 1
  }
}

fn next_u64() -> u64 {
  RNG.with(|rng| {
    let mut x = rng.get();
    // xorshift64* algorithm
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    rng.set(x);
    x.wrapping_mul(0x2545_F491_4F6C_DD1D)
  })
}

/// Fill a buffer with *reasonably unpredictable* bytes. The randomness created by this is deterministic, but should be good enough for most purposes. If you need cryptographic safety, use [`os_fill()`] instead.
fn fill(buf: &mut [u8]) {
  for chunk in buf.chunks_mut(8) {
    let n = next_u64().to_ne_bytes();
    chunk.copy_from_slice(&n[..chunk.len()]);
  }
}

pub(crate) trait Random {
  fn get_random() -> Self;
}

pub(crate) fn random<T: Random>() -> T {
  T::get_random()
}

macro_rules! impl_randint {
  ($($t:ty),*) => {$(
    impl Random for $t {
      fn get_random() -> Self {
        let mut b = [0u8; size_of::<$t>()];
        fill(&mut b);
        <$t>::from_ne_bytes(b)
      }
    }
  )*};
}

impl_randint!(
  u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize
);

pub(crate) trait RandomRange: Sized {
  fn random_range(range: Range<Self>) -> Self;
}

pub(crate) fn random_range<T: RandomRange>(range: Range<T>) -> T {
  T::random_range(range)
}

macro_rules! below {
  ($n: expr, $t:ty) => {{
    let t = (<$t>::MAX - $n + 1) % $n;
    loop {
      let r = next_u64() as $t;
      if r >= t {
        break r % $n;
      }
    }
  }};
}
macro_rules! impl_range {
  ($($t:ty),*) => {$(
    impl RandomRange for $t {
      fn random_range(range: Range<Self>) -> Self {
        let (start, end) = (range.start, range.end);
        assert!(end >= start, "invalid range: {start}..{end}");

        let diff = end - start;
        let offset = below!(diff, $t);
        start + offset
      }
    }
  )*};

  ($($t:ty => $u:ty),*) => {$(
    impl RandomRange for $t {
      fn random_range(range: Range<Self>) -> Self {
        let (start, end) = (range.start, range.end);
        assert!(end >= start, "invalid range: {start}..{end}");

        let diff = (end as $u).wrapping_sub(start as $u);
        let offset = below!(diff, $u);
        start.wrapping_add(offset as $t)
      }
    }
  )*};
}

impl_range!(u8, u16, u32, u64, usize);
impl_range!(
  i8 => u8, i16 => u16, i32 => u32, i64 => u64, isize => usize
);

impl Random for bool {
  fn get_random() -> Self {
    u8::get_random() & 1 == 1
  }
}

/// Universally Unique Identifer (v4)
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub(crate) struct Uuid([u8; 16]);

impl Uuid {
  pub fn new_v4() -> Self {
    Self::get_os_random()
  }
}

impl From<u128> for Uuid {
  fn from(value: u128) -> Self {
    let mut b = value.to_ne_bytes();
    b[6] = (b[6] & 0x0F) | 0x40;
    b[8] = (b[8] & 0x3F) | 0x80;
    Self(b)
  }
}

impl OsRandom for Uuid {
  fn get_os_random() -> Self {
    Self::from(os_random::<u128>())
  }
}

impl Random for Uuid {
  fn get_random() -> Self {
    Self::from(random::<u128>())
  }
}

fn hex_val(b: u8) -> Option<u8> {
  match b {
    b'0'..=b'9' => Some(b - b'0'),
    b'a'..=b'f' => Some(b - b'a' + 10),
    b'A'..=b'F' => Some(b - b'A' + 10),
    _ => None,
  }
}

impl FromStr for Uuid {
  type Err = ShErr;
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    let err = || sherr!(ParseErr, "invalid UUID string: {s}");
    let mut buf = [0u8; 16];
    let mut i = 0;
    let mut bytes = SliceCursor::new(s.as_bytes());

    while let Some(b) = bytes.next_byte() {
      if b == b'-' {
        continue;
      } // lenient
      if i >= 16 {
        return Err(err());
      } // too many

      let hi = hex_val(b).ok_or_else(err)?;
      let lo = bytes.next_byte().and_then(hex_val).ok_or_else(err)?;
      buf[i] = (hi << 4) | lo;
      i += 1;
    }
    if i != 16 {
      return Err(err());
    } // too few

    Ok(Self(buf))
  }
}

impl Display for Uuid {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let mut out = [0u8; 36];
    let mut i = 0;

    for (j, &b) in self.0.iter().enumerate() {
      if matches!(j, 4 | 6 | 8 | 10) {
        out[i] = b'-';
        i += 1;
      }
      out[i]/*-*/= HEX[(b >> 4) as usize];
      out[i + 1] = HEX[(b & 0x0F) as usize];
      i += 2;
    }

    f.write_str(std::str::from_utf8(&out).unwrap())
  }
}
