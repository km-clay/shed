use crate::{HashMap, eval::lex::Span, state::vars::ShStr};
use std::{
  cell::RefCell,
  fmt::Display,
  ops::Deref,
  sync::{
    Arc, LazyLock, RwLock, Weak,
    atomic::{AtomicI32, Ordering},
  },
};

/// A map of currently active text sources
///
/// Holds weak references, so that when all [`SourceHandle`]s for a source are dropped, the source
/// will be removed from this map.
static SOURCES: LazyLock<RwLock<SourceRegistry>> = LazyLock::new(Default::default);
static SRC_GENERATION: AtomicI32 = AtomicI32::new(0);

thread_local! {
  /// A strong reference to the last source accessed in the [`SOURCES`] map.
  static LAST_SOURCE: RefCell<Option<SourceHandle>> = const { RefCell::new(None) };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
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
/// they are used, so it's important to manage the lifetimes of `SourceHandle`s carefully. [`StrongSpans`](StrongSpan) can
/// be used to extend the lifetime of a `Span` by keeping a `SourceHandle` alive for as long as the `StrongSpan` exists.
#[derive(Debug, PartialEq, Eq, Hash)]
pub(crate) struct SourceHandle {
  ptr: Arc<Source>,
  id : SourceId,
}

impl SourceHandle {
  pub(crate) fn get_id(&self) -> SourceId {
    self.id
  }

  /// Create a clone of the inner `Arc<Source>` that this `SourceHandle` wraps
  ///
  /// We use this instead of deriving `Clone`, because `SourceHandle` is meant to be a unique handle to a source,
  /// and if we have to share ownership, we want to be make it clear. It's also easier to grep, so that's nice
  ///
  /// `SourceHandle` is meant to define the lifetime for a source input, so it's important that we keep the refcount as low
  /// as possible.
  pub(crate) fn share_handle(&self) -> Self {
    Self {
      ptr: Arc::clone(&self.ptr),
      id : self.id,
    }
  }
}

impl Drop for SourceHandle {
  fn drop(&mut self) {
    // now we check to see if nothing else references this source
    if Arc::strong_count(&self.ptr) != 1 {
      return;
    }

    // take a write lock. no reader can upgrade while we hold this
    // so the strong count is a reliable measurement past this point
    let mut reg = SOURCES.write().unwrap();

    if Arc::strong_count(&self.ptr) == 1 {
      reg.sources.remove(&self.id);
    }
  }
}

/// A variant of [`Span`] that carries a `SourceHandle` with it.
///
/// These are used in cases where a `Span` could reasonably outlive its source.
/// This is best effort; if the source is already dropped, the `StrongSpan` will still be valid,
/// but any attempt to access the source text will panic.
#[derive(Debug)]
pub(crate) struct StrongSpan {
  span  : Span,
  handle: Option<SourceHandle>,
}

impl Span {
  /// Upgrade a [`Span`] to a [`StrongSpan`], which carries a `SourceHandle` with it.
  pub(crate) fn upgrade(self) -> StrongSpan {
    StrongSpan {
      span  : self,
      handle: handle_for(self.source()),
    }
  }
}

impl Clone for StrongSpan {
  fn clone(&self) -> Self {
    Self {
      span  : self.span,
      handle: self.handle.as_ref().map(SourceHandle::share_handle),
    }
  }
}

impl Deref for StrongSpan {
  type Target = Span;
  fn deref(&self) -> &Self::Target {
    &self.span
  }
}

impl ariadne::Span for StrongSpan {
  type SourceId = self::SourceId;

  fn source(&self) -> &Self::SourceId {
    ariadne::Span::source(&self.span)
  }

  fn start(&self) -> usize {
    self.span.start()
  }

  fn end(&self) -> usize {
    self.span.end()
  }
}

/// Build a temporary co-owning handle for an already-registered source.
///
/// Returns `None` if the source has been dropped.
pub(crate) fn handle_for(id: SourceId) -> Option<SourceHandle> {
  LAST_SOURCE.with(|slot| {
    if let Some(handle) = &*slot.borrow()
      && handle.id == id
    {
      return Some(handle.share_handle());
    }

    let src    = SOURCES.read().unwrap().get_source(id)?;
    let handle = SourceHandle { ptr: src, id };
    let shared = handle.share_handle();
    *slot.borrow_mut() = Some(handle);
    Some(shared)
  })
}

impl Deref for SourceHandle {
  type Target = [u8];
  fn deref(&self) -> &Self::Target {
    &self.ptr.content
  }
}

#[derive(Debug, Hash, Eq, PartialEq)]
pub(crate) struct Source {
  name   : Option<ShStr>,
  content: ShStr,
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

  fn register(&mut self, name: Option<ShStr>, src: ShStr) -> SourceHandle {
    let id     = SourceId(SRC_GENERATION.fetch_add(1, Ordering::AcqRel));

    let source = Arc::new(Source { name, content: src });
    let weak   = Arc::downgrade(&source);
    self.sources.insert(id, weak);
    SourceHandle { ptr: source, id }
  }
}

pub(crate) fn register_source<T: Into<ShStr>>(src: T) -> SourceHandle {
  SOURCES.write().unwrap().register(None, src.into())
}

pub(crate) fn register_named_source<T: Into<ShStr>>(name: T, src: T) -> SourceHandle {
  SOURCES
    .write()
    .unwrap()
    .register(Some(name.into()), src.into())
}

pub(crate) fn get_source(id: SourceId) -> Option<ShStr> {
  SOURCES
    .read()
    .unwrap()
    .get_source(id)
    .map(|s| s.content.clone())
}

pub(crate) fn get_source_name(id: SourceId) -> Option<ShStr> {
  SOURCES
    .read()
    .unwrap()
    .get_source(id)
    .and_then(|s| s.name.clone())
}

pub(crate) fn slice_source(span: Span) -> Option<ShStr> {
  let start = span.start();
  let end   = span.end();
  let id    = span.source();
  let source = LAST_SOURCE.with(|slot| {
    if let Some(handle) = &*slot.borrow()
      && handle.id == id
    {
      return Some(Arc::clone(&handle.ptr));
    }

    let src = SOURCES.read().unwrap().get_source(id)?;
    *slot.borrow_mut() = Some(SourceHandle {
      ptr: Arc::clone(&src),
      id,
    });
    Some(src)
  })?;

  source.content.try_slice(start..end)
}
