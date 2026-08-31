//! Macros used in the parsing process
//!
//! These macros are used to cut down on boilerplate code and make the parsing functions more readable.

/// Try to match a specific parsing rule
///
/// # Notes
/// * If the match fails, execution continues.
/// * If the match succeeds, the matched node is returned.
macro_rules! try_match {
  ($expr:expr) => {
    if let Some(node) = $expr {
      return Ok(Some(node));
    }
  };
}

/// A helper macro for returning parse errors with context
///
/// This macro is used to cut down on boilerplate when returning errors in the various parsing functions.
/// This macro also calls `self.panic_mode` internally, and requires a mutable borrow of the '$tks' parameter.
macro_rules! bail {
	($parser:expr, $tks:expr, $($arg:tt)*) => {
		$parser.panic_mode(&mut $tks);
		return Err(parse_err!($parser, $tks, $($arg)*));
	};
}

/// A helper macro for constructing parse errors with context
macro_rules! parse_err {
	($parser:expr, $span:expr, $($arg:tt)*) => {
		$crate::eval::parse::stream::parse_err_full(
			&format!($($arg)*),
			&$span.unwrap_or_default(),
			&$parser.context,
		)
	};
}

macro_rules! extend_span {
  ($span:expr, $other:expr) => {
    if let Some(span) = &mut $span {
      span.merge_inplace(&$other);
    } else {
      $span = Some($other.clone());
    }
  };
}

/// A helper macro for constructing AST nodes with varying amounts of information
///
/// The first three parameters are always required, but the flags and redirs can be optionally left out if not needed. This is used to cut down on boilerplate when constructing nodes in the various parsing functions
/// example:
/// ```
/// node!(self, node_tks, NdRule::Conjunction { elements }, vec![], NdFlags::empty())
/// ```
macro_rules! node {
  ($parser:expr, $span:expr, $class:expr, $redirs:expr, $flags:expr) => {{
    let mut flags = $flags;
    if $parser.last_consumed_was_sep() {
      flags |= $crate::eval::parse::node::NdFlags::PUNCTUATED;
    }
    let context = (!$parser.context.is_empty()).then(|| {
      $parser
        .tree
        .alloc_labels($parser.context.clone().into_iter())
    });

    $crate::eval::parse::node::Node {
      class: $class,
      flags,
      redirs: $redirs,
      context,
      span: $span,
    }
  }};
  ($parser:expr, $span:expr, $class:expr, $redirs:expr) => {{
    let mut flags = $crate::eval::parse::node::NdFlags::empty();
    if $parser.last_consumed_was_sep() {
      flags |= $crate::eval::parse::node::NdFlags::PUNCTUATED;
    }
    let context = Some(
      $parser
        .tree
        .alloc_labels($parser.context.clone().into_iter()),
    );
    $crate::eval::parse::node::Node {
      class: $class,
      flags,
      redirs: $redirs,
      context,
      span: $span,
    }
  }};
  ($parser:expr, $span:expr, $class:expr) => {{
    let mut flags = $crate::eval::parse::node::NdFlags::empty();
    if $parser.last_consumed_was_sep() {
      flags |= $crate::eval::parse::node::NdFlags::PUNCTUATED;
    }
    let context = Some(
      $parser
        .tree
        .alloc_labels($parser.context.clone().into_iter()),
    );
    $crate::eval::parse::node::Node {
      class: $class,
      flags,
      redirs: None,
      context,
      span: $span,
    }
  }};
}
