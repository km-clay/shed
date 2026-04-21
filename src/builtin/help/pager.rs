use std::io::Write;

use nix::{
  libc::STDOUT_FILENO,
  unistd::{isatty, write},
};
use regex::Regex;

use crate::{
  builtin::help::{
    StyledHelp,
    markup::{MarkedSpan, REF_SEQ, RESET_SEQ, SEARCH_RES_SEQ, TAG_SEQ},
  },
  libsh::error::ShResult,
  procio::borrow_fd,
  readline::{
    SimpleEditor,
    editcmd::Direction,
    keys::KeyEvent,
  }, state::with_term, write_term,
};

pub enum PagerEvent {
  Continue,
  Back,
  Forward,
  OpenRef(String), // Open a new pager from this cross-reference
  Exit,
}

pub enum PagerCmd {
  Scroll(isize), // line offset
  TopOfPage,
  BottomOfPage,
}

#[derive(Default, Debug)]
pub struct SearchQuery {
  editor: SimpleEditor,
  dir: Direction,
  results: Vec<(usize, usize)>, // spans
  anchor: usize,                // line we started on
  active: bool,
}

impl SearchQuery {
  pub fn reset(&mut self) {
    self.active = false;
    self.editor.buf.clear_buffer();
    self.results.clear();
  }

  pub fn is_empty(&self) -> bool {
    self.editor.buf.is_empty()
  }
}

pub struct HelpPager {
  search: SearchQuery,
  ref_keys: Vec<(usize, char)>,
  cross_refs: Vec<MarkedSpan>, // spans

  jump_dist: usize,

  scroll_offset: usize,
  filename: Option<String>,
  content: StyledHelp,
}

impl HelpPager {
  pub fn new(content: String, scroll_offset: usize, filename: Option<String>) -> Option<Self> {
    if !isatty(STDOUT_FILENO).unwrap_or(false) {
      // If we're not in a terminal, just print the content and exit
      // Someone could be piping the output, like `help | grep foo`
      write(borrow_fd(STDOUT_FILENO), content.as_bytes()).ok();
      write(borrow_fd(STDOUT_FILENO), b"\n").ok();
      return None;
    }
    let content = StyledHelp::new(&content);
    let cross_refs = content.find_markers(REF_SEQ);

    Some(Self {
      jump_dist: 15,
      ref_keys: vec![],
      search: SearchQuery::default(),
      scroll_offset,
      filename,
      content,
      cross_refs,
    })
  }
  pub fn content(&self) -> &str {
    self.content.content()
  }

  pub fn cross_refs_in_viewport(&self) -> Vec<usize> {
    let top = self.scroll_offset;
    let t_rows = with_term(|t| t.t_rows());
    let bottom = top + t_rows as usize;

    let first = self
      .cross_refs
      .iter()
      .position(|c_ref| c_ref.line_no(self.content()) >= top);

    let last = self
      .cross_refs
      .iter()
      .rposition(|c_ref| c_ref.line_no(self.content()) < bottom);

    match (first, last) {
      (Some(f), Some(l)) if f <= l => (f..=l).collect(),
      _ => vec![],
    }
  }

  pub fn display(&mut self) -> ShResult<()> {
    // need to take this out of the struct for a sec
    // so that we can buffer the lines without allocating
    write_term!("\x1b[H")?;
    let height = with_term(|t| t.t_rows());
    let mut content = self.content().to_string();

    for (s, e) in self.search.results.iter().rev() {
      content.insert_str(*e, RESET_SEQ);
      content.insert_str(*s, SEARCH_RES_SEQ);
    }

    let content_lines: Vec<_> = content
      .lines()
      .skip(self.scroll_offset)
      .take(height as usize)
      .collect();

    for (i, line) in content_lines.iter().enumerate() {
      if !self.ref_keys.is_empty() {
        let mut line = line.to_string();
        let indexes = self.cross_refs.iter().enumerate().filter(|(ci, c_ref)| {
          self.ref_keys.iter().any(|(j, _)| *j == *ci)
            && c_ref.line_no(self.content()) == self.scroll_offset + i
        });

        for index in indexes.rev() {
          let (_, _, postfix) = self.cross_refs[index.0].rel_to_line(self.content());
          let Some((_, ch)) = self.ref_keys.iter().find(|(j, _)| *j == index.0) else {
            continue;
          };

          line = format!(
            "{}{TAG_SEQ}[{ch}]{RESET_SEQ}{}",
            &line[..postfix.end],
            &line[postfix.end..],
          );
        }

        write_term!("{line}\x1b[K\n").ok();
      } else {
        write_term!("{line}\x1b[K\n").ok();
      }
    }

    for _ in content_lines.len()..height as usize {
      write_term!("\x1b[1;34m~\x1b[0m\x1b[K\n").ok(); // draw tildes on empty lines
    }

    write_term!("\r").ok();

    if let Some(name) = &self.filename {
      write_term!("\x1b[1;7;4m {name} \x1b[0m ",).ok();
    }

    if self.search.active {
      let query = self.search.editor.buf.joined();
      let prefix = match self.search.dir {
        Direction::Forward => '/',
        Direction::Backward => '?',
      };
      write_term!("\x1b[1;7;4m {prefix}{query} \x1b[0m",).ok();
    }

    with_term(|t| t.flush())?;
    Ok(())
  }

  pub fn handle_input(&mut self) -> ShResult<PagerEvent> {
    with_term(|t| t.read())?;
    let keys = with_term(|t| t.drain_keys())?;

    let mut res = PagerEvent::Continue;
    for key in keys {
      res = self.handle_key(key)?;
    }

    Ok(res)
  }

  pub fn handle_key(&mut self, key: KeyEvent) -> ShResult<PagerEvent> {
    use crate::readline::keys::KeyCode as K;

    let KeyEvent(code, _mods) = &key;

    let cmd = match code {
      K::Tab => {
        if self.ref_keys.is_empty() {
          self.enter_hint_mode();
        } else {
          self.ref_keys.clear();
        }
        return Ok(PagerEvent::Continue);
      }

      K::Esc => {
        if !self.ref_keys.is_empty() {
          self.ref_keys.clear();
          return Ok(PagerEvent::Continue);
        } else if self.search.active {
          self.search.reset();
          return Ok(PagerEvent::Continue);
        } else {
          return Ok(PagerEvent::Exit);
        }
      }

      K::Backspace if self.search.active && self.search.is_empty() => {
        self.search.reset();
        return Ok(PagerEvent::Continue);
      }

      K::Enter if self.search.active => {
        self.search(true);
        self.search.active = false; // keep results for highlighting

        return Ok(PagerEvent::Continue);
      }

      _ if self.search.active => {
        self.search.editor.handle_key(key)?;
        if self.search.editor.buf.is_empty() {
          self.search.results.clear();
        } else {
          self.search(false);
        }

        return Ok(PagerEvent::Continue);
      }

      K::Char(ch @ ('/' | '?')) => {
        if !self.ref_keys.is_empty() {
          self.ref_keys.clear();
        }
        self.search.reset();
        let dir = match ch {
          '?' => Direction::Backward,
          '/' => Direction::Forward,
          _ => unreachable!(),
        };

        self.search.active = true;
        self.search.dir = dir;
        self.search.anchor = self.scroll_offset;

        return Ok(PagerEvent::Continue);
      }

      K::Char(ch) if !self.ref_keys.is_empty() => {
        if let Some(index) = self
          .ref_keys
          .iter()
          .find(|(_, c)| *c == *ch)
          .map(|(i, _)| *i)
        {
          self.ref_keys.clear();
          let c_ref = &self.cross_refs[index];
          let target = c_ref.content(self.content());
          return Ok(PagerEvent::OpenRef(target.to_string()));
        } else {
          self.ref_keys.clear();
          return self.handle_key(key); // re-process the key without hint mode
        }
      }

      K::Char(dir @ ('n' | 'N')) => {
        match dir {
          'n' => self.jump_to_match(Direction::Forward),
          'N' => self.jump_to_match(Direction::Backward),
          _ => unreachable!(),
        }
        return Ok(PagerEvent::Continue);
      }

      K::Char('q') => return Ok(PagerEvent::Exit),

      K::Char('g') => PagerCmd::TopOfPage,
      K::Char('G') => PagerCmd::BottomOfPage,

      K::Char('d') => PagerCmd::Scroll(self.jump_dist as isize),
      K::Char('u') => PagerCmd::Scroll(-(self.jump_dist as isize)),

      K::Down | K::Char('j') => PagerCmd::Scroll(1),
      K::Up | K::Char('k') => PagerCmd::Scroll(-1),
      K::Left | K::Char('h') => return Ok(PagerEvent::Back),
      K::Right | K::Char('l') => return Ok(PagerEvent::Forward),

      _ => return Ok(PagerEvent::Continue),
    };

    self.exec_cmd(cmd)?;

    Ok(PagerEvent::Continue)
  }

  pub fn max_scroll(&self) -> usize {
    let t_rows = with_term(|t| t.t_rows());
    self
      .content()
      .lines()
      .count()
      .saturating_sub(t_rows)
  }

  pub fn search(&mut self, jump: bool) {
    if self.search.editor.buf.joined().is_empty() || !self.search.active {
      return;
    }
    let pat = self.search.editor.buf.joined();
    let re = Regex::new(&regex::escape(&pat)).unwrap();
    let content = self.content();

    // collect entries into self.search.results
    // results contains absolute byte spans
    self.search.results = re
      .find_iter(content)
      .map(|m| (m.start(), m.end()))
      .collect();

    if jump {
      self.jump_to_match(self.search.dir);
    }
  }

  pub fn jump_to_match(&mut self, dir: Direction) {
    if self.search.results.is_empty() {
      return;
    }

    let content = self.content();
    let anchor = self.search.anchor;

    // I'd like to personally thank the borrow checker for forcing this thing into existence
    let lf_positions: Vec<_> = content
      .bytes()
      .enumerate()
      .filter(|(_, c)| *c == b'\n')
      .map(|(i, _)| i)
      .collect();

    let line_for = |start: &usize| {
      lf_positions
        .iter()
        .position(|pos| *pos > *start)
        .unwrap_or(lf_positions.len())
    };

    // Try to find a match past the anchor in the given direction
    let after_anchor = self.search.results.iter().filter(|(start, _)| {
      let line_no = line_for(start);
      match dir {
        Direction::Forward => line_no > anchor,
        Direction::Backward => line_no < anchor,
      }
    });

    let found = match dir {
      Direction::Forward => after_anchor.min_by_key(|(start, _)| *start),
      Direction::Backward => after_anchor.max_by_key(|(start, _)| *start),
    };

    // If nothing found past anchor, wrap around
    let found = found.or_else(|| match dir {
      Direction::Forward => self.search.results.iter().min_by_key(|(start, _)| *start),
      Direction::Backward => self.search.results.iter().max_by_key(|(start, _)| *start),
    });

    if let Some((start, _)) = found {
      let line_no = line_for(start);
      self.scroll_offset = line_no.saturating_sub(1);
      self.search.anchor = line_no;
    }
  }

  pub fn enter_hint_mode(&mut self) {
    if self.search.active {
      self.search.reset();
    }

    let mut chars = HintChars::new();
    let c_refs = self.cross_refs_in_viewport();

    for i in c_refs {
      if let Some(ch) = chars.next() {
        self.ref_keys.push((i, ch));
      } else {
        break; // no more hint chars available
      }
    }
  }

  pub fn exec_cmd(&mut self, cmd: PagerCmd) -> ShResult<()> {
    match cmd {
      PagerCmd::Scroll(n) => {
        self.scroll_offset = self
          .scroll_offset
          .saturating_add_signed(n)
          .min(self.max_scroll());
      }
      PagerCmd::TopOfPage => {
        self.scroll_offset = 0;
      }
      PagerCmd::BottomOfPage => {
        let rows = with_term(|t| t.t_rows());
        let n_lines = self.content().lines().count();
        self.scroll_offset = n_lines.saturating_sub(rows as usize);
      }
    }

    Ok(())
  }
}

pub struct HintChars {
  seq: String,
}

impl HintChars {
  pub fn new() -> Self {
    Self {
      seq: "MNBVCXZPOIUYTREWQLKJHGFDSAmnbvcxzpoiuytrewqlkjhgfdsa".into(),
    }
  }
}

impl Iterator for HintChars {
  type Item = char;
  fn next(&mut self) -> Option<Self::Item> {
    self.seq.pop()
  }
}
