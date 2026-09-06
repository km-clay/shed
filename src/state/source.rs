use crate::{HashMap, eval::lex::Span, state::vars::VarStr};
use std::{
  borrow::Cow,
  fmt::Display,
  ops::Deref,
  sync::{
    Arc, LazyLock, RwLock, Weak,
    atomic::{AtomicI32, Ordering},
  },
};

static SOURCES: LazyLock<RwLock<SourceRegistry>> = LazyLock::new(Default::default);
static SRC_GENERATION: AtomicI32 = AtomicI32::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct SourceId(i32);

impl SourceId {
  pub(crate) const NONE: Self = SourceId(-1);
}

impl Display for SourceId {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    self.0.fmt(f)
  }
}

/// A handle for a source of text input.
///
/// Wraps an `Arc<Source>`, which holds the actual text. This struct itself *does not* implement `Clone`, because
/// it is meant to manage the lifetime of the `Source` it wraps. When the `Arc` refcount hits zero, the `Source` will die.
///
/// The reason we use an `Arc` here instead of just literally storing the `Source` on the struct is because of the rare
/// instance where sharing ownership of the source text actually is necessary, like breaking off a function definition to
/// store in the logic table. For these cases, [`SourceHandle::share_handle`] can be used to create a new `SourceHandle`
/// that shares ownership of the same `Source`.
///
/// As long as at least one handle exists, the `Source` will remain alive. When all handles are dropped, the `Source` will
/// be dropped as well. Any [`Spans`](crate::eval::lex::Span) that still refer to the dropped source will cause a panic if
/// they are used, so it's important to manage the lifetimes of `SourceHandle`s carefully.
#[derive(Debug, PartialEq, Eq, Hash)]
pub(crate) struct SourceHandle {
  ptr: Arc<Source>,
  id: SourceId,
}

impl SourceHandle {
  pub(crate) fn get_id(&self) -> SourceId {
    self.id
  }

  /// Create a clone of the inner `Arc<Source>` that this `SourceHandle` wraps
  ///
  /// We use this instead of deriving `Clone`, because `SourceHandle` is meant to be a unique handle to a source,
  /// and if we have to share ownership, we want to be explicit about doing so. It's also easier to grep, so that's nice
  ///
  /// `SourceHandle` is meant to define the lifetime for a source input, so it's important that we keep the refcount as low
  /// as possible.
  pub(crate) fn share_handle(&self) -> Self {
    Self {
      ptr: Arc::clone(&self.ptr),
      id: self.id,
    }
  }

  pub(crate) fn name(&self) -> Option<&[u8]> {
    self.ptr.name.as_deref()
  }

  pub(crate) fn as_bytes(&self) -> &[u8] {
    &self.ptr.content
  }
}

/// Build a temporary co-owning handle for an already-registered source.
///
/// Returns `None` if the source has been dropped.
pub(crate) fn handle_for(id: SourceId) -> Option<SourceHandle> {
  SOURCES
    .read()
    .unwrap()
    .get_source(id)
    .map(|ptr| SourceHandle { ptr, id })
}

impl Deref for SourceHandle {
  type Target = [u8];
  fn deref(&self) -> &Self::Target {
    &self.ptr.content
  }
}

#[derive(Debug, Hash, Eq, PartialEq)]
pub(crate) struct Source {
  name: Option<Box<[u8]>>,
  content: Box<[u8]>,
}

#[derive(Default)]
struct SourceRegistry {
  sources: HashMap<SourceId, Weak<Source>>,
}

impl SourceRegistry {
  fn get_source(&self, id: SourceId) -> Option<Arc<Source>> {
    if id.0 < 0 {
      return None;
    }
    self.sources.get(&id).and_then(Weak::upgrade)
  }

  fn register(&mut self, name: Option<Box<[u8]>>, src: Box<[u8]>) -> SourceHandle {
    let id = SourceId(SRC_GENERATION.fetch_add(1, Ordering::AcqRel));
    let source = Arc::new(Source {
      name: name.map(|n| n.into()),
      content: src.into(),
    });
    let weak = Arc::downgrade(&source);
    self.sources.insert(id, weak);
    SourceHandle { ptr: source, id }
  }
}

pub(crate) fn register_source<T: InputSource>(src: T) -> SourceHandle {
  SOURCES
    .write()
    .unwrap()
    .register(None, src.into_source_bytes())
}

pub(crate) fn register_named_source<T: InputSource>(name: T, src: T) -> SourceHandle {
  SOURCES
    .write()
    .unwrap()
    .register(Some(name.into_source_bytes()), src.into_source_bytes())
}

trait InputSource {
  fn into_source_bytes(self) -> Box<[u8]>;
}
impl InputSource for VarStr {
  fn into_source_bytes(self) -> Box<[u8]> {
    self.as_bytes().into()
  }
}
impl InputSource for Vec<u8> {
  fn into_source_bytes(self) -> Box<[u8]> {
    self.into()
  }
}
impl InputSource for &[u8] {
  fn into_source_bytes(self) -> Box<[u8]> {
    self.into()
  }
}
impl InputSource for &str {
  fn into_source_bytes(self) -> Box<[u8]> {
    self.as_bytes().into()
  }
}
impl InputSource for String {
  fn into_source_bytes(self) -> Box<[u8]> {
    self.into_bytes().into()
  }
}
impl InputSource for Cow<'_, str> {
  fn into_source_bytes(self) -> Box<[u8]> {
    self.into_owned().into_bytes().into()
  }
}

pub(crate) fn get_source(id: SourceId) -> Option<VarStr> {
  SOURCES
    .read()
    .unwrap()
    .get_source(id)
    .map(|s| VarStr::from(&*s.content))
}

pub(crate) fn get_source_name(id: SourceId) -> Option<VarStr> {
  SOURCES
    .read()
    .unwrap()
    .get_source(id)
    .and_then(|s| s.name.as_deref().map(VarStr::from))
}

pub(crate) fn slice_source(span: Span) -> Option<VarStr> {
  let start = span.start();
  let end = span.end();
  let id = span.source();
  let source = {
    let lock = SOURCES.read().unwrap();
    lock.get_source(id)
  };

  source.map(|s| VarStr::from(&(*s.content)[start..end]))
}
