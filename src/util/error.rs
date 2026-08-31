//! The shell's error type (`ShErr` / `ShResult`) and its `ariadne`-rendered diagnostics.
#![expect(clippy::needless_pass_by_value)]
use ariadne::{Color, Label};
use ariadne::{Report, ReportKind};
use nix::errno::Errno;
use std::cell::RefCell;
use std::fmt::{self, Debug, Display};
use std::io::Write;
use std::rc::Rc;

use crate::{
  HashMap,
  eval::lex::{Span, SpanSource},
  procio::{self, RedirGuard},
  sherr, shopt,
  state::vars::VarStr,
  util::random,
  varstr,
};

use super::FdWriter;

pub(crate) type ShResult<T> = Result<T, ShErr>;

pub(crate) struct ColorRng {
  last_color: Option<Color>,
}

impl ColorRng {
  fn get_colors() -> &'static [Color] {
    &[
      Color::Red,
      Color::Cyan,
      Color::Blue,
      Color::Green,
      Color::Yellow,
      Color::Magenta,
      Color::Fixed(208), // orange
      Color::Fixed(39),  // deep sky blue
      Color::Fixed(170), // orchid / magenta-pink
      Color::Fixed(76),  // chartreuse
      Color::Fixed(51),  // aqua
      Color::Fixed(226), // bright yellow
      Color::Fixed(99),  // slate blue
      Color::Fixed(214), // light orange
      Color::Fixed(48),  // spring green
      Color::Fixed(201), // hot pink
      Color::Fixed(81),  // steel blue
      Color::Fixed(220), // gold
      Color::Fixed(105), // medium purple
    ]
  }

  pub(crate) fn last_color(&mut self) -> Color {
    if let Some(color) = self.last_color.take() {
      color
    } else {
      let color = self.next().unwrap_or(Color::White);
      self.last_color = Some(color);
      color
    }
  }
}

impl Iterator for ColorRng {
  type Item = Color;
  fn next(&mut self) -> Option<Self::Item> {
    let colors = Self::get_colors();
    let idx = random::random::<usize>() % colors.len();

    Some(colors[idx])
  }
}

thread_local! {
  static COLOR_RNG: RefCell<ColorRng> = const { RefCell::new(ColorRng { last_color: None }) };
}

pub(crate) fn next_color() -> Color {
  COLOR_RNG.with(|rng| {
    let color = rng.borrow_mut().next().unwrap();
    rng.borrow_mut().last_color = Some(color);
    color
  })
}

pub(crate) fn last_color() -> Color {
  COLOR_RNG.with(|rng| rng.borrow_mut().last_color())
}

pub(crate) fn get_context(msg: impl Into<LabelMsg>, span: &Span) -> LabelBuilder {
  let color = last_color();
  LabelBuilder::new(span.clone())
    .with_color(color)
    .with_message(msg)
}

fn group_labels(labels: Vec<LabelBuilder>) -> Vec<(Span, Label<Span>)> {
  let n = labels.len();
  if n == 0 {
    return labels
      .into_iter()
      .map(|label| (label.span(), label.into()))
      .collect();
  }

  let labels: Vec<(Span, Label<Span>)> = labels
    .into_iter()
    .map(|label| (label.span(), label.into()))
    .collect();

  // avert your eyes
  let mut chain_id: Vec<usize> = (0..n).collect();
  for i in 0..n {
    for j in i + 1..n {
      if related_by_containment(&labels[i].0, &labels[j].0) {
        let (a, b) = (chain_id[i], chain_id[j]);
        if a != b {
          let (keep, drop) = (a.min(b), a.max(b));
          for c in &mut chain_id {
            if *c == drop {
              *c = keep;
            }
          }
        }
      }
    }
  }
  // labels[0] is the primary span (pushed first by `at()`); its chain id
  // marks the chain we want to render last.
  let primary = chain_id[0];

  let mut annotated: Vec<(usize, Span, Label<Span>)> = labels
    .into_iter()
    .enumerate()
    .map(|(i, (s, l))| (chain_id[i], s, l))
    .collect();

  annotated.sort_by(|a, b| {
    let a_primary = a.0 == primary;
    let b_primary = b.0 == primary;
    a_primary.cmp(&b_primary).then(a.0.cmp(&b.0)).then_with(|| {
      let size_a = a.1.range().end - a.1.range().start;
      let size_b = b.1.range().end - b.1.range().start;
      size_a.cmp(&size_b)
    })
  });

  annotated.into_iter().map(|(_, s, l)| (s, l)).collect()
}

/// Returns true if the two spans are related by containment, i.e. one is fully contained within the other.
fn related_by_containment(a: &Span, b: &Span) -> bool {
  if a.span_source() != b.span_source() {
    return false;
  }
  let ra = a.range();
  let rb = b.range();

  let a_contains_b = ra.start <= rb.start && ra.end >= rb.end;
  let b_contains_a = rb.start <= ra.start && rb.end >= ra.end;

  a_contains_b || b_contains_a
}

pub(crate) trait ShResultExt {
  fn try_blame(self, span: Span) -> Self;
  /// If the value is `Err()`, attach a span to it
  fn promote_err(self, span: Span) -> Self;
}

impl<T> ShResultExt for Result<T, ShErr> {
  /// Blame a span if no blame has been assigned yet
  fn try_blame(self, new_span: Span) -> Self {
    self.map_err(|e| e.try_blame(new_span))
  }
  fn promote_err(self, span: Span) -> Self {
    self.map_err(|e| e.promote(span))
  }
}

#[derive(Clone)]
pub(crate) enum LabelMsg {
  Eager(VarStr),
  Lazy(Rc<dyn Fn() -> VarStr>),
}

impl Display for LabelMsg {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      LabelMsg::Eager(msg) => write!(f, "{msg}"),
      LabelMsg::Lazy(fmt) => write!(f, "{}", fmt()),
    }
  }
}

impl Debug for LabelMsg {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      LabelMsg::Eager(msg) => write!(f, "LabelMsg::Eager({msg:?})"),
      LabelMsg::Lazy(_) => write!(f, "LabelMsg::Lazy(<closure>)"),
    }
  }
}

impl LabelMsg {
  #[cfg(test)]
  fn get(self) -> VarStr {
    match self {
      LabelMsg::Eager(msg) => msg,
      LabelMsg::Lazy(f) => f(),
    }
  }

  pub(crate) fn lazy(f: impl Fn() -> VarStr + 'static) -> Self {
    LabelMsg::Lazy(Rc::new(f))
  }
}

impl From<VarStr> for LabelMsg {
  fn from(msg: VarStr) -> Self {
    LabelMsg::Eager(msg)
  }
}

impl From<Rc<dyn Fn() -> VarStr>> for LabelMsg {
  fn from(msg: Rc<dyn Fn() -> VarStr>) -> Self {
    LabelMsg::Lazy(msg)
  }
}

impl From<&str> for LabelMsg {
  fn from(msg: &str) -> Self {
    LabelMsg::Eager(VarStr::from(msg))
  }
}

impl From<String> for LabelMsg {
  fn from(msg: String) -> Self {
    Self::from(msg.as_str())
  }
}

#[derive(Debug, Clone)]
pub(crate) struct LabelBuilder {
  span: Span,
  message: Option<LabelMsg>,
  color: Option<Color>,
}

impl LabelBuilder {
  pub(crate) fn new(span: Span) -> Self {
    Self {
      span,
      message: None,
      color: None,
    }
  }
  pub(crate) fn with_message(mut self, message: impl Into<LabelMsg>) -> Self {
    self.message = Some(message.into());
    self
  }
  pub(crate) fn with_color(mut self, color: Color) -> Self {
    self.color = Some(color);
    self
  }
  pub(crate) fn span(&self) -> Span {
    self.span.clone()
  }
}

impl From<LabelBuilder> for ariadne::Label<Span> {
  fn from(val: LabelBuilder) -> Self {
    let mut label = ariadne::Label::new(val.span);
    if let Some(message) = val.message {
      label = label.with_message(message);
    }
    if let Some(color) = val.color {
      label = label.with_color(color);
    }
    label
  }
}

/// The shell's main error type.
#[derive(Debug)]
pub(crate) struct ShErr {
  kind: ShErrKind,
  src_span: Option<Span>,
  labels: Vec<LabelBuilder>,
  notes: Vec<VarStr>,

  /// If we propagate through a redirect boundary, we take ownership of
  /// the RedirGuard(s) so that redirections stay alive until the error
  /// is printed.  Multiple guards can accumulate as the error bubbles
  /// through nested redirect scopes.
  io_guards: Vec<RedirGuard>,
}

impl ShErr {
  pub(crate) fn new(kind: ShErrKind, span: Span) -> Self {
    Self {
      kind,
      src_span: Some(span),
      labels: vec![],
      notes: vec![],
      io_guards: vec![],
    }
  }
  pub(crate) fn simple(kind: ShErrKind, msg: VarStr) -> Self {
    Self {
      kind,
      src_span: None,
      labels: vec![],
      notes: vec![msg],
      io_guards: vec![],
    }
  }
  pub(crate) fn loop_break(code: i32) -> Self {
    Self::simple(
      ShErrKind::LoopContinue(code),
      varstr!("'continue' found outside of loop"),
    )
  }
  pub(crate) fn loop_continue(code: i32) -> Self {
    Self::simple(
      ShErrKind::LoopBreak(code),
      varstr!("'break' found outside of loop"),
    )
  }
  pub(crate) fn is_flow_control(&self) -> bool {
    self.kind.is_flow_control()
  }
  /// Test-only: the messages of every label attached to this error, in
  /// attachment order. Lets us golden-test traceback *context* without
  /// coupling to ANSI/box-drawing rendering.
  #[cfg(test)]
  pub(crate) fn label_messages(&self) -> Vec<String> {
    self
      .labels
      .iter()
      .filter_map(|l| {
        l.message
          .as_ref()
          .map(|m| m.clone().get().to_str_lossy().into_owned())
      })
      .collect()
  }
  pub(crate) fn option_promote(self, span: Option<Span>) -> Self {
    match span {
      Some(span) => self.promote(span),
      None => self,
    }
  }
  /// Promotes a shell error from a simple error to an error that blames a span
  pub(crate) fn promote(self, span: Span) -> Self {
    self.promote_inner(span, Self::try_blame)
  }

  pub(crate) fn force_promote(self, span: Span) -> Self {
    self.promote_inner(span, Self::blame)
  }

  /// Collapse nested labels
  ///
  /// Takes the span of the outer-most label, and the message/color of the inner-most label
  /// and combines them into one, discarding all intermediate labels.
  pub(crate) fn collapse_context(self) -> Self {
    if self.labels.is_empty() {
      return self;
    }

    let Self { labels, .. } = self;

    let LabelBuilder { message, color, .. } = labels.first().cloned().unwrap();
    let LabelBuilder { span, .. } = labels.last().cloned().unwrap();
    let collapsed = LabelBuilder {
      span,
      message,
      color,
    };

    let labels = vec![collapsed];

    Self { labels, ..self }
  }

  fn promote_inner<F: FnOnce(Self, Span) -> Self>(mut self, span: Span, blame_func: F) -> Self {
    if self.notes.is_empty() {
      return blame_func(self, span);
    }
    let first = self.notes[0].clone();
    if self.notes.len() > 1 {
      self.notes = self.notes[1..].to_vec();
    } else {
      self.notes = vec![];
    }

    blame_func(self, span.clone()).labeled(span, first)
  }
  /// Persist all io guards, closing saved fds without restoring them.
  /// Use this when an error is being converted to a control flow signal
  /// (like `ErrInterrupt`) that will propagate past the redirect scope.
  pub(crate) fn persist_redirs(&mut self) {
    for guard in self.io_guards.drain(..) {
      guard.persist();
    }
  }
  /// Give a redirguard to this error so that it remains alive
  /// This allows redirguards to move their guarded context upwards
  pub(crate) fn with_redirs(mut self, guard: Option<RedirGuard>) -> Self {
    if let Some(guard) = guard {
      self.io_guards.push(guard);
    }
    self
  }
  pub(crate) fn at(kind: ShErrKind, span: Span, msg: VarStr) -> Self {
    let color = last_color();
    let label = LabelBuilder::new(span.clone())
      .with_message(msg)
      .with_color(color);
    Self::new(kind, span.clone()).with_label(label)
  }
  pub(crate) fn labeled(self, span: Span, msg: VarStr) -> Self {
    let color = last_color();
    let label = LabelBuilder::new(span.clone())
      .with_message(msg)
      .with_color(color);
    self.with_label(label)
  }
  pub(crate) fn blame(mut self, span: Span) -> Self {
    self.src_span = Some(span);
    self
  }
  pub(crate) fn try_blame(self, span: Span) -> Self {
    match self {
      ShErr { src_span: None, .. } => Self {
        src_span: Some(span),
        ..self
      },
      _ => self,
    }
  }
  pub(crate) fn kind(&self) -> &ShErrKind {
    &self.kind
  }
  pub(crate) fn kind_mut(&mut self) -> &mut ShErrKind {
    &mut self.kind
  }
  pub(crate) fn set_kind(&mut self, kind: ShErrKind) {
    self.kind = kind;
  }
  pub(crate) fn with_label(mut self, label: LabelBuilder) -> Self {
    self.labels.push(label);
    self
  }
  pub(crate) fn with_context<'a>(mut self, ctx: impl Iterator<Item = &'a LabelBuilder>) -> Self {
    self.labels.extend(ctx.cloned());
    self
  }
  pub(crate) fn with_note(mut self, note: VarStr) -> Self {
    self.notes.push(note);
    self
  }
  pub(crate) fn src_span(&self) -> Option<&Span> {
    self.src_span.as_ref()
  }
  pub(crate) fn build_report(&self) -> Option<Report<'_, Span>> {
    let span = self.src_span.as_ref()?;
    let kind = if self.kind().is_warning() {
      ReportKind::Warning
    } else {
      ReportKind::Error
    };
    let mut report = Report::build(kind, span.clone()).with_config(
      ariadne::Config::default()
        .with_index_type(ariadne::IndexType::Byte)
        .with_tab_width(shopt!(line.tab_width))
        .with_color(true),
    );
    report = report.with_message(self.kind.to_string());

    for (_, label) in group_labels(self.labels.clone()) {
      report = report.with_label(label);
    }
    for note in &self.notes {
      report = report.with_note(note);
    }

    Some(report.finish())
  }
  fn collect_sources(&self) -> HashMap<SpanSource, VarStr> {
    let mut source_map = HashMap::default();
    if let Some(span) = &self.src_span {
      let src = span.span_source().clone();
      source_map
        .entry(src.clone())
        .or_insert_with(|| (*src.content()).into());
    }
    for span in self.labels.iter().map(LabelBuilder::span) {
      let src = span.span_source().clone();
      source_map
        .entry(src.clone())
        .or_insert_with(|| (*src.content()).into());
    }
    source_map
  }
  fn default_write(&self, fd: &mut impl Write) {
    writeln!(fd, "\n{}", self.kind).ok();
    for note in &self.notes {
      writeln!(fd, "note: {note}").ok();
    }
  }
  fn print_error_internal(&self, fd: &mut impl Write) {
    if *self.kind() == ShErrKind::Interrupt {
      // Don't print anything for Interrupt
      // This only occurs when the user breaks out of something with ctrl + c
      return;
    }
    let Some(report) = self.build_report() else {
      return self.default_write(fd);
    };

    let sources = self.collect_sources();
    let cache = ariadne::FnCache::new(move |src: &SpanSource| {
      sources
        .get(src)
        .map(|s| s.to_str_lossy().into_owned())
        .ok_or_else(|| format!("Failed to fetch source '{}'", src.name()))
    });
    writeln!(fd).ok();
    if report.write(cache, &mut *fd).is_err() {
      self.default_write(fd);
    }
  }
  pub(crate) fn print_error(self) {
    // retrieve and attach the active call-frame traceback context, if any
    let frames = crate::state::Shed::call_context();
    let with_frames = if frames.is_empty() {
      self
    } else {
      self.with_context(frames.iter())
    };

    let error = if shopt!(core.compact_errors) {
      with_frames.collapse_context()
    } else {
      with_frames
    };

    error.print_error_internal(&mut FdWriter(procio::stderr_fileno()));
  }
}

impl Display for ShErr {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let mut buf = vec![];
    self.print_error_internal(&mut buf);
    let out = procio::bytes_to_string(buf);
    write!(f, "{out}")
  }
}

impl From<std::fmt::Error> for ShErr {
  fn from(value: std::fmt::Error) -> Self {
    sherr!(InternalErr, "{value}")
  }
}

impl From<rusqlite::Error> for ShErr {
  fn from(value: rusqlite::Error) -> Self {
    ShErr::simple(ShErrKind::HistoryReadErr, varstr!("{value}"))
  }
}

impl From<std::io::Error> for ShErr {
  fn from(e: std::io::Error) -> Self {
    let msg = std::io::Error::last_os_error();
    ShErr::simple(ShErrKind::IoErr(e.kind()), varstr!("{msg}"))
  }
}

impl From<std::env::VarError> for ShErr {
  fn from(value: std::env::VarError) -> Self {
    ShErr::simple(ShErrKind::InternalErr, varstr!("{value}"))
  }
}

impl From<Errno> for ShErr {
  fn from(value: Errno) -> Self {
    ShErr::simple(ShErrKind::Errno(value), varstr!("{value}"))
  }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) enum ShErrKind {
  IoErr(std::io::ErrorKind),
  Custom(VarStr, i32), // message, error code
  InvalidOpt,
  SyntaxErr,
  ParseErr,
  InternalErr,
  ExecFail,
  HistoryReadErr,
  BadPermission,
  Errno(Errno),
  NotFound,
  InvalidAssignment,
  TryFailed,

  // Warnings
  DeprecationWarning,

  // Not really errors, more like internal signals
  /// Created by the `exit` builtin
  CleanExit(i32),
  /// Created by the `return` builtin
  FuncReturn(i32),
  /// Created by the `continue` builtin
  LoopContinue(i32),
  /// Created by the `break` builtin
  LoopBreak(i32),
  /// Created by the `raise` builtin
  Raised(Option<VarStr>, i32),
  /// Created by non-zero exit status when `errexit` is set
  ErrInterrupt,
  /// Created by `SIGINT` in scripts or `Ctrl+C` in interactive mode
  Interrupt,
}

impl ShErrKind {
  pub(crate) fn is_flow_control(&self) -> bool {
    matches!(
      self,
      Self::CleanExit(_)
        | Self::FuncReturn(_)
        | Self::LoopContinue(_)
        | Self::LoopBreak(_)
        | Self::Interrupt
    )
  }
  pub(crate) fn is_warning(&self) -> bool {
    matches!(self, Self::DeprecationWarning)
  }
}

impl Display for ShErrKind {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let output = match self {
      Self::IoErr(e) => &format!("I/O Error: {e}"),
      Self::Custom(msg, _) => msg.to_str().unwrap_or_default(),
      Self::InvalidOpt => "Invalid option",
      Self::ParseErr => "Parse Error",
      Self::InternalErr => "Internal Error",
      Self::HistoryReadErr => "History Parse Error",
      Self::ExecFail => "Execution Failed",
      Self::DeprecationWarning => "Deprecation Warning",
      Self::BadPermission => "Bad Permissions",
      Self::Errno(e) => &format!("Errno: {}", e.desc()),
      Self::NotFound => "Not Found",
      Self::TryFailed => "Try Failed",
      Self::CleanExit(_) | Self::Interrupt => "",
      Self::InvalidAssignment => "Invalid Assignment",
      Self::ErrInterrupt => "errexit",
      Self::SyntaxErr | Self::FuncReturn(_) | Self::LoopContinue(_) | Self::LoopBreak(_) => {
        "Syntax Error"
      }
      Self::Raised(kind, _) => {
        if let Some(kind) = kind {
          return Display::fmt(&kind, f);
        }
        return write!(f, "Raised Error");
      }
    };
    write!(f, "{output}")
  }
}
