use std::{
  collections::VecDeque,
  ops::Index,
  os::unix::ffi::OsStrExt,
  path::{Path, PathBuf},
  rc::Rc,
};

use crate::{
  match_loop, shopt,
  util::{path_entries, path_from_bytes},
};

/// A matcher representing a single byte
#[derive(Debug, Clone)]
enum One {
  Byte(u8),                     // literal character
  Any,                          // any character ('?')
  Class(Rc<[ClassItem]>, bool), // char class
}

impl One {
  fn matches(&self, b: u8, ci: bool) -> bool {
    match self {
      One::Byte(x) => {
        if ci {
          x.eq_ignore_ascii_case(&b)
        } else {
          *x == b
        }
      }
      One::Any => true,
      One::Class(items, polarity) => ClassItem::is_match(*polarity, items, b, ci),
    }
  }
}

/// A matcher representing `One` byte, or `Star` - one or more bytes
#[derive(Debug, Clone)]
enum Atom {
  One(One),
  Star,
}

impl Atom {
  fn tokenize(pattern: &[u8]) -> Rc<[Self]> {
    let mut out = vec![];
    let mut i = 0;
    let n = pattern.len();

    while i < n {
      match pattern[i] {
        b'\\' => {
          // a backslash escapes the following byte (any byte) into a literal
          if i + 1 < n {
            out.push(Self::One(One::Byte(pattern[i + 1])));
            i += 2;
          } else {
            out.push(Self::One(One::Byte(b'\\')));
            i += 1;
          }
        }
        b'*' => {
          out.push(Self::Star);
          while i < n && pattern[i] == b'*' {
            i += 1; // collapse a run of stars into one
          }
        }
        b'?' => {
          out.push(Self::One(One::Any));
          i += 1;
        }
        b'[' if i + 1 < n => {
          if let Some((polarity, items, len)) = parse_class(&pattern[i + 1..]) {
            out.push(Self::One(One::Class(items.into(), polarity)));
            i += 1 + len; // skip the `[` and the class body
          } else {
            out.push(Self::One(One::Byte(b'['))); // unterminated `[` is literal
            i += 1;
          }
        }
        b => {
          out.push(Self::One(One::Byte(b)));
          i += 1;
        }
      }
    }

    Rc::from(out)
  }
}

#[derive(Debug, Clone, Copy)]
enum SweepDir {
  Forward,
  Backward,
}

#[derive(Debug, Clone, Copy)]
enum SweepMode {
  Longest(SweepDir),
  Shortest(SweepDir),
}

impl SweepMode {
  fn is_reverse(self) -> bool {
    match self {
      SweepMode::Longest(dir) | SweepMode::Shortest(dir) => matches!(dir, SweepDir::Backward),
    }
  }
  fn is_longest(self) -> bool {
    matches!(self, SweepMode::Longest(_))
  }
}

/// For atom sets under 64 items, this uses a bitset fast path
enum StateSet {
  Bitset(u64),
  Vector(Vec<bool>),
}

impl StateSet {
  pub fn new(len: usize) -> Self {
    if len < 64 {
      Self::Bitset(0)
    } else {
      Self::Vector(vec![false; len])
    }
  }
  pub fn get(&self, idx: usize) -> Option<bool> {
    match self {
      StateSet::Bitset(set) => (idx < 64).then(|| set & (1 << idx) != 0),
      StateSet::Vector(set) => set.get(idx).copied(),
    }
  }
  pub fn set(&mut self, idx: usize, val: bool) {
    match self {
      StateSet::Bitset(set) => {
        if idx < 64 {
          if val {
            *set |= 1 << idx;
          } else {
            *set &= !(1 << idx);
          }
        }
      }
      StateSet::Vector(set) => {
        if let Some(slot) = set.get_mut(idx) {
          *slot = val;
        }
      }
    }
  }
  pub fn fill(&mut self, val: bool) {
    match self {
      StateSet::Bitset(set) => {
        *set = if val { u64::MAX } else { 0 };
      }
      StateSet::Vector(set) => set.fill(val),
    }
  }
}

impl Index<usize> for StateSet {
  type Output = bool;
  fn index(&self, index: usize) -> &Self::Output {
    if self.get(index).unwrap_or(false) {
      &true
    } else {
      &false
    }
  }
}

/// Thompson NFA Simulation
///
/// Given a list of `Atom` and text, find the start of longest/shortest suffix
fn rsweep(atoms: &[Atom], text: &[u8], longest: bool, ci: bool) -> Option<usize> {
  let mode = if longest {
    SweepMode::Longest(SweepDir::Backward)
  } else {
    SweepMode::Shortest(SweepDir::Backward)
  };
  sweep_inner(atoms, text, mode, ci)
}

/// Thompson NFA Simulation
///
/// Given a list of `Atom` and text, find the end of longest/shortest prefix
fn sweep(atoms: &[Atom], text: &[u8], longest: bool, ci: bool) -> Option<usize> {
  let mode = if longest {
    SweepMode::Longest(SweepDir::Forward)
  } else {
    SweepMode::Shortest(SweepDir::Forward)
  };
  sweep_inner(atoms, text, mode, ci)
}

/// Thompson NFA Simulation
///
/// The main engine that drives `shed`'s pattern matching.
/// Given a list of `Atom` and text, find the end/start of longest/shortest match
fn sweep_inner(atoms: &[Atom], text: &[u8], mode: SweepMode, ci: bool) -> Option<usize> {
  /*
   * leaving this here in case I forget how this algorithm works, because it's a really weird one.
   * https://swtch.com/~rsc/regexp/regexp1.html
   *
   * The algorithm works by flipping between two sets of booleans to map any possible pathway that
   * matches the target string. There are two types of Atoms: Star, and One(_). One set (cur) is initialized
   * by setting `set[0]` to true, and the other set (next) is filled with `false`.
   *
   * For each byte, and then for each atom, if the corresponding boolean of the *previous set* is "true"
   * then that Atom is "active" and should try to match on the current byte. Star atoms remain active *permanently*
   * after getting hit once, and *always* activate the next Atom. One(_) atoms activate the next Atom only if they match
   * the current byte. This automatically handles runs of One(_) atoms, because they must build a "bridge" to the next Star,
   * or the "Accept slot" (more on that later). Any run of One(_) atoms that fails to build this bridge automatically
   * collapses into the most recent Star. If there isn't a star to collapse into, the match simply fails.
   *
   * After going through each byte, swap the sets, fill the new one with `false` and use the old one to see which Atoms
   * are currently active. The sets themselves are each `atoms.len() + 1` slots long. The extra slot is the "Accept" slot.
   * If the "Accept slot" is ever true, then the string matches the glob. If we are eagerly searching, we store the index,
   * and if we aren't, we return the index immediately.
   *
   * Here's an example run:
   *
   * glob: af*ota*b, input: afbarotabizb
   * atoms: One(a), One(f), Star, One(o), One(t), One(a), Star, One(b)
   *
   * resulting truth table:
   *
   * |   | `a` | `f` | star | `o` | `t` | `a` | star | `b` |accept|
   * |   |  T  |     |      |     |     |     |      |     |      |
   * | a |     |  T  |      |     |     |     |      |     |      |
   * | f |     |     |  T   |  T  |     |     |      |     |      |
   * | b |     |     |  T   |  T  |     |     |      |     |      |
   * | a |     |     |  T   |  T  |     |     |      |     |      |
   * | r |     |     |  T   |  T  |     |     |      |     |      |
   * | o |     |     |  T   |  T  |  T  |     |      |     |      |
   * | t |     |     |  T   |  T  |     |  T  |      |     |      |
   * | a |     |     |  T   |  T  |     |     |  T   |  T  |      |
   * | b |     |     |  T   |  T  |     |     |  T   |  T  |  T   | -> matches (shortest)
   * | i |     |     |  T   |  T  |     |     |  T   |  T  |      |
   * | z |     |     |  T   |  T  |     |     |  T   |  T  |      |
   * | b |     |     |  T   |  T  |     |     |  T   |  T  |  T   | -> matches (longest)
   */

  let m = atoms.len();
  let reverse = mode.is_reverse();
  let longest = mode.is_longest();

  let atom = |j: usize| {
    if reverse {
      &atoms[m - 1 - j]
    } else {
      &atoms[j]
    }
  };

  let close = |set: &mut StateSet| {
    // if a position has a Star, the next position is certainly reachable
    for j in 0..m {
      if set[j] && matches!(atom(j), Atom::Star) {
        set.set(j + 1, true);
      }
    }
  };

  let mut cur = StateSet::new(m + 1);
  let mut next = StateSet::new(m + 1);

  cur.set(0, true);
  close(&mut cur);

  let mut result = None;

  if cur[m] {
    if longest {
      result = Some(0);
    } else {
      return Some(0);
    }
  }

  let bytes = if reverse {
    itertools::Either::Left(text.iter().rev())
  } else {
    itertools::Either::Right(text.iter())
  };

  for (i, &b) in bytes.enumerate() {
    next.fill(false);

    for j in 0..m {
      if !cur[j] {
        continue;
      }
      match atom(j) {
        Atom::Star => next.set(j, true),
        Atom::One(one) => {
          if one.matches(b, ci) {
            next.set(j + 1, true);
          }
        }
      }
    }

    close(&mut next);
    std::mem::swap(&mut cur, &mut next);

    if cur[m] {
      let end = i + 1;
      if longest {
        result = Some(end);
      } else {
        return Some(end);
      }
    }
  }

  result
}

#[derive(Debug, Clone)]
pub(crate) struct Pattern {
  glob: Glob,
  orig: Rc<[u8]>, // used for caching
}

#[derive(Debug, Clone)]
struct Glob {
  atoms: Rc<[Atom]>,
  ci: bool,
}
impl Glob {
  fn new(pattern: &[u8], ci: bool) -> Self {
    Self {
      atoms: Atom::tokenize(pattern),
      ci,
    }
  }

  fn is_match(&self, other: &[u8]) -> bool {
    sweep(&self.atoms, other, true, self.ci) == Some(other.len())
  }

  pub fn match_shortest_prefix(&self, text: &[u8]) -> Option<usize> {
    self.match_prefix(text, false)
  }

  pub fn match_longest_prefix(&self, text: &[u8]) -> Option<usize> {
    self.match_prefix(text, true)
  }

  fn match_prefix(&self, text: &[u8], longest: bool) -> Option<usize> {
    sweep(&self.atoms, text, longest, self.ci)
  }

  pub fn match_shortest_suffix(&self, text: &[u8]) -> Option<usize> {
    self.match_suffix(text, false)
  }

  pub fn match_longest_suffix(&self, text: &[u8]) -> Option<usize> {
    self.match_suffix(text, true)
  }

  fn match_suffix(&self, text: &[u8], longest: bool) -> Option<usize> {
    rsweep(&self.atoms, text, longest, self.ci)
  }

  pub fn find(&self, text: &[u8], from: usize) -> Option<(usize, usize)> {
    (from..=text.len()).find_map(|start| {
      sweep(&self.atoms, &text[start..], true, self.ci).map(|len| (start, start + len))
    })
  }
}

#[derive(Debug, Clone)]
enum ClassItem {
  Byte(u8),
  Range(u8, u8),
  Posix(PosixClass),
}

impl ClassItem {
  fn matches_raw(items: &[Self], b: u8) -> bool {
    items.iter().any(|it| match *it {
      ClassItem::Byte(x) => b == x,
      ClassItem::Range(lo, hi) => (lo..=hi).contains(&b),
      ClassItem::Posix(pc) => pc.test(b),
    })
  }
  fn is_match(polarity: bool, items: &[Self], b: u8, ci: bool) -> bool {
    // A byte matches a case-insensitive class if the byte OR its case-flip
    // (`b ^ 0x20` toggles ASCII letter case) is a member.
    let hit = Self::matches_raw(items, b)
      || (ci && b.is_ascii_alphabetic() && Self::matches_raw(items, b ^ 0x20));
    hit == polarity
  }
}

#[derive(Debug, Clone, Copy)]
enum PosixClass {
  Alpha,
  Digit,
  Alnum,
  Upper,
  Lower,
  Space,
  Blank,
  XDigit,
  Punct,
  Graph,
  Print,
  Cntrl,
}

impl PosixClass {
  fn from_name(name: &[u8]) -> Option<Self> {
    match name {
      b"alnum" => Some(Self::Alnum),
      b"alpha" => Some(Self::Alpha),
      b"blank" => Some(Self::Blank),
      b"cntrl" => Some(Self::Cntrl),
      b"digit" => Some(Self::Digit),
      b"graph" => Some(Self::Graph),
      b"lower" => Some(Self::Lower),
      b"print" => Some(Self::Print),
      b"punct" => Some(Self::Punct),
      b"space" => Some(Self::Space),
      b"upper" => Some(Self::Upper),
      b"xdigit" => Some(Self::XDigit),
      _ => None,
    }
  }

  fn test(self, b: u8) -> bool {
    match self {
      Self::Alpha => b.is_ascii_alphabetic(),
      Self::Alnum => b.is_ascii_alphanumeric(),
      Self::Cntrl => b.is_ascii_control(),
      Self::Digit => b.is_ascii_digit(),
      Self::Graph => b.is_ascii_graphic(),
      Self::Lower => b.is_ascii_lowercase(),
      Self::Print => b.is_ascii_graphic() || b == b' ',
      Self::Punct => b.is_ascii_punctuation(),
      Self::Upper => b.is_ascii_uppercase(),
      Self::XDigit => b.is_ascii_hexdigit(),
      Self::Blank => matches!(b, b' ' | b'\t'),
      Self::Space => matches!(b, b'\t' | b'\n' | b'\r' | 0x0B | 0x0C | b' '),
    }
  }
}

fn parse_class(p: &[u8]) -> Option<(bool, Vec<ClassItem>, usize)> {
  let n = p.len();
  let mut items = vec![];
  let mut polarity = true;
  let mut j = 0;

  if j < n && (p[j] == b'!' || p[j] == b'^') {
    polarity = false;
    j += 1;
  }
  if j < n && p[j] == b']' {
    items.push(ClassItem::Byte(b']'));
    j += 1;
  }

  let member = |k: usize| -> Option<(u8, usize)> {
    if p[k] == b'\\' && k + 1 < n {
      Some((p[k + 1], k + 2))
    } else {
      Some((p[k], k + 1))
    }
  };

  while j < n {
    match p[j] {
      b']' => return Some((polarity, items, j + 1)),
      b'[' if j + 1 < n && p[j + 1] == b':' => {
        // posix class
        let start = j + 2;
        let mut k = start;

        // find ':]'
        while k + 1 < n && !(p[k] == b':' && p[k + 1] == b']') {
          k += 1;
        }
        if k + 1 >= n {
          return None;
        } // unterminated
        let class = PosixClass::from_name(&p[start..k])?;
        items.push(ClassItem::Posix(class));
        j = k + 2;
      }
      _ => {
        let (lo, after) = member(j)?;
        if after + 1 < n && p[after] == b'-' && p[after + 1] != b']' {
          // lo-hi range
          let (hi, after2) = member(after + 1)?;
          let (lo, hi) = crate::util::ordered(lo, hi);
          items.push(ClassItem::Range(lo, hi));
          j = after2;
        } else {
          items.push(ClassItem::Byte(lo));
          j = after;
        }
      }
    }
  }

  None
}

impl Pattern {
  pub fn compile(pattern: &[u8], ci: bool) -> Self {
    Self {
      glob: Glob::new(pattern, ci),
      orig: pattern.into(),
    }
  }
  pub fn orig(&self) -> &Rc<[u8]> {
    &self.orig
  }
  pub fn is_match(&self, text: &[u8]) -> bool {
    self.glob.is_match(text)
  }
  pub fn match_shortest_prefix(&self, text: &[u8]) -> Option<usize> {
    self.glob.match_shortest_prefix(text)
  }
  pub fn match_longest_prefix(&self, text: &[u8]) -> Option<usize> {
    self.glob.match_longest_prefix(text)
  }
  pub fn match_shortest_suffix(&self, text: &[u8]) -> Option<usize> {
    self.glob.match_shortest_suffix(text)
  }
  pub fn match_longest_suffix(&self, text: &[u8]) -> Option<usize> {
    self.glob.match_longest_suffix(text)
  }
  pub fn find(&self, text: &[u8], from: usize) -> Option<(usize, usize)> {
    self.glob.find(text, from)
  }
}

/// Quick structural check: only return true if the string could plausibly be a glob.
pub(super) fn might_be_glob(s: &[u8]) -> bool {
  let mut open_bracket = false;
  let mut bytes = s.iter();

  match_loop!(bytes.next() => b, {
    b'\\' => {
      bytes.next(); // escaped
    }
    b'*' | b'?' => return true,
    b']' if open_bracket => return true,
    b'[' => open_bracket = true,
    _ => {}
  });
  false
}

pub fn normalize_dir<P: AsRef<Path>>(path: &P) -> &Path {
  let path = path.as_ref();
  if path.as_os_str().is_empty() {
    Path::new(".")
  } else {
    path
  }
}

enum PathSeg {
  RecStar,
  Literal(Box<[u8]>),
  Glob { pat: Pattern, lit_dot: bool },
}

impl PathSeg {
  pub fn compile_segments(pattern: &[u8], ci: bool) -> Vec<Self> {
    pattern
      .split(|&b| b == b'/')
      .filter(|s| !s.is_empty())
      .map(|seg| {
        if seg == b"**" {
          PathSeg::RecStar
        } else if might_be_glob(seg) {
          let lit_dot = seg.starts_with(b".");
          PathSeg::Glob {
            pat: Pattern::compile(seg, ci),
            lit_dot,
          }
        } else {
          PathSeg::Literal(seg.into())
        }
      })
      .collect()
  }
}

pub fn expand_glob(pattern: &[u8], case_insensitive: bool) -> Vec<Vec<u8>> {
  if !might_be_glob(pattern) || shopt!(set.noglob) {
    return vec![pattern.to_vec()];
  }

  let segments = PathSeg::compile_segments(pattern, case_insensitive);
  let absolute = pattern.starts_with(b"/");
  let dirs_only = pattern.len() > 1 && pattern.ends_with(b"/");
  let dotglob = shopt!(core.dotglob);

  let seed = if absolute {
    PathBuf::from("/")
  } else {
    PathBuf::new() // empty
  };

  // using a deque lets us switch between BFS/DFS if one proves
  // to be more efficient than the other. `pop_front` = BFS, `pop_back` = DFS
  let mut frontier = VecDeque::from([(seed, 0usize)]); // (path, segments consumed)
  let mut out = vec![];

  while let Some((path, i)) = frontier.pop_front() {
    if i == segments.len() {
      if !dirs_only || path.is_dir() {
        let bytes = path.as_os_str().as_bytes().to_vec();

        out.push(bytes);
      }
      continue;
    }

    match &segments[i] {
      PathSeg::RecStar => {
        let is_last_segment = i + 1 == segments.len();

        if is_last_segment && !path.as_os_str().is_empty() {
          let mut base = path.as_os_str().as_bytes().to_vec();
          base.push(b'/');
          out.push(base);
        } else {
          frontier.push_back((path.clone(), i + 1));
        }

        for entry in path_entries(&normalize_dir(&path)) {
          let is_dir = entry.file_type().is_ok_and(|ft| ft.is_dir());

          let mut child = path.clone();
          child.push(entry.file_name());
          if is_dir {
            frontier.push_back((child, i));
          } else if is_last_segment && !dirs_only {
            frontier.push_back((child, i + 1));
          }
        }
      }
      PathSeg::Literal(lit) => {
        let mut child = path.clone();
        child.push(path_from_bytes(lit));
        if child.exists() {
          frontier.push_back((child, i + 1));
        }
      }
      PathSeg::Glob { pat, lit_dot } => {
        for entry in path_entries(&normalize_dir(&path)) {
          let name = entry.file_name();
          let bytes = name.as_bytes();
          if bytes.first() == Some(&b'.') && !*lit_dot && !dotglob {
            continue;
          }
          if pat.is_match(bytes) {
            let mut child = path.clone();
            child.push(name);
            frontier.push_back((child, i + 1));
          }
        }
      }
    }
  }

  if out.is_empty() && !shopt!(core.nullglob) {
    out.push(pattern.to_vec());
  }

  out.sort();
  out.dedup();
  out
}

pub(crate) fn replace_posix_classes(s: &str) -> String {
  let mut out = String::with_capacity(s.len());
  let mut chars = s.chars().peekable();
  let mut in_bracket = false;

  match_loop!(chars.next() => ch, {
    '\\' => {
      out.push(ch);
      if let Some(next_ch) = chars.next() {
        out.push(next_ch);
      }
    }
    '[' if !in_bracket => {
      in_bracket = true;
      out.push(ch);

      // convert '^' to '!'
      // glob crate uses '!' for negation in it's patterns
      if chars.peek() == Some(&'^') {
        chars.next();
        out.push('!');
      }
    }
    ']' if in_bracket => {
      in_bracket = false;
      out.push(ch);
    }
    '[' if in_bracket && chars.peek() == Some(&':') => {
      chars.next();
      let mut name = String::new();
      match_loop!(chars.peek() => &ch => ch, {
        ':' => {
          chars.next();
          break
        }
        _ => {
          name.push(ch);
          chars.next();
        }
      });

      if chars.peek() == Some(&']')
      && let Some(posix_chars) = posix_class_chars(&name) {
        chars.next();
        out.push_str(posix_chars);
      } else {
        out.push('[');
        out.push(':');
        out.push_str(&name);
      }

    }
    _ => out.push(ch),
  });

  out
}

fn posix_class_chars(name: &str) -> Option<&'static str> {
  match name {
    "alnum" => Some("a-zA-Z0-9"),
    "alpha" => Some("a-zA-Z"),
    "blank" => Some(" \t"),
    "cntrl" => Some("\x00-\x1F\x7F"),
    "digit" => Some("0-9"),
    "graph" => Some("!-~"),
    "lower" => Some("a-z"),
    "print" => Some(" -~"),
    "punct" => Some("!-/:-@\\[-`{-~"),
    "space" => Some(" \t\r\n\x0b\x0c"),
    "upper" => Some("A-Z"),
    "xdigit" => Some("A-Fa-f0-9"),
    _ => None,
  }
}
