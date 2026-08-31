//! Execution of shell functions
//!
//! This module contains the logic for executing shell functions, including:
//! * storing function definitions
//! * handling function calls
//! * managing associated execution context.
//!
//! It manages the function call stack, variable scope, redirections, and error handling specific to function execution.

use bstr::ByteSlice;
use shed_macros::styled_format;

use super::{AssignBehavior, Ast, KEYWORDS, NdRule, NodeId, Tk};
use crate::{
  defer,
  eval::parse::NdFlags,
  procio::{RedirResult, RedirSet},
  sherr,
  state::{
    Shed,
    logic::{ShFunc, TrapTarget},
    meta::MetaTab,
    shopt,
    terminal::Terminal,
    vars::{VarFlags, VarKind},
  },
  util::{
    self,
    error::{self, LabelMsg, ShErrKind, ShResult, ShResultExt},
    guards,
  },
};

impl super::Dispatcher {
  /// Execute a function definition
  ///
  /// Creates a new [`Ast`] from the function body and stores it in the function table.
  /// The function name is checked against reserved keywords and forbidden names.
  pub(super) fn exec_func_def(tree: &Ast, func_def: NodeId) -> ShResult<()> {
    let blame = tree.span_for(func_def);
    let NdRule::FuncDef { name, body, ctx } = &tree[func_def].class else {
      unreachable!()
    };
    let body = tree.break_off(*body);

    let func_name = tree[*name].span.as_bytes();
    let func_name = func_name.strip_suffix(b"()").unwrap_or(func_name);

    if KEYWORDS.contains(&func_name) || matches!(func_name, b"builtin" | b"command") {
      return Err(sherr!(
          SyntaxErr @ tree[*name].span.clone(),
          "function: Forbidden function name `{}`",
          func_name.to_str_lossy()
      ));
    }

    let func = ShFunc::defined(body, blame).with_ctx(tree[*ctx].clone());
    Shed::logic_mut(|l| l.insert_func(&func_name.to_str_lossy(), func)); // Store the AST
    if Shed::term(Terminal::interactive) {
      Shed::meta_mut(|m| {
        m.set_last_was_func_def(true);
      });
    }

    Shed::set_status(0);
    Ok(())
  }
  /// Execute a shell function
  ///
  /// Handles setup for function execution, including variable assignments, redirections, and error handling.
  /// Also adds a stack trace frame for the function call to be used in `shed`'s error reporting
  pub(super) fn exec_func(&mut self, tree: &Ast, func_id: NodeId) -> ShResult<()> {
    let func = &tree[func_id];
    if Shed::meta_mut(MetaTab::take_fork) {
      let func_body = tree.break_off(func_id);

      let Some(root) = func_body.get_root() else {
        return Err(sherr!(
            InternalErr @ tree.span_for(func_id),
            "Function body has no root node",
        ));
      };

      let name = func_body
        .command_for(root)
        .map(Tk::to_str_lossy)
        .unwrap_or_default();

      return self.run_fork(name.as_bytes(), |s| {
        super::catch_exit(|| s.exec_func(&func_body, root), super::exit_with);
      });
    }

    // need to do this in a new scope so we can borrow func safely
    let (func_name, mut blame) = {
      let borrow = &func;

      // borrow func.class to avoid partial move
      let NdRule::Command { ref argv, .. } = borrow.class else {
        unreachable!()
      };

      let Some(func_name) = argv.first() else {
        return Err(sherr!(
            InternalErr @ tree.span_for(func_id),
            "Expected function name in command position"
        ));
      };

      let name = tree[func_name]
        .clone()
        .expand()?
        .get_first_word()
        .unwrap_or_default();

      (name, tree[func_name].span.clone())
    };

    let Some(sh_func) = Shed::logic(|l| l.get_func(&func_name.to_str_lossy())) else {
      return Err(sherr!(
          InternalErr @ blame,
          "Failed to find function '{func_name}'"
      ));
    };

    let (func_body, func_src_ctx) = match sh_func {
      ShFunc::Defined { logic, ctx, .. } => (logic, ctx),
      ShFunc::Autoload(src) => {
        Shed::logic_mut(|l| l.remove_func(&func_name.to_str_lossy())); // remove autoload from the table
        src.source()?;

        // retry, passing func by value
        // the scoped assignment and borrow above are done
        // so that we can pass func untouched to dispatch_cmd()
        return self.dispatch_cmd(tree, func_id);
      }
    };

    let NdRule::Command { assignments, argv } = &func.class else {
      unreachable!()
    };

    let caller_contexts: Vec<_> = tree[func.context].to_vec();

    let label_name = func_name.clone();
    let call_ctx = error::get_context(
      LabelMsg::lazy(move || styled_format!("in call to function '{}'", &label_name).into()),
      &blame,
    );

    let max_depth = Shed::shopts(|s| s.core.max_recurse_depth);
    let depth = Shed::meta(MetaTab::func_depth);
    if depth > max_depth {
      return Err(sherr!(
          InternalErr @ blame,
          "maximum recursion depth ({max_depth}) exceeded",
      ));
    }

    // Prefix assignments on a function call (`X=2 f`) are temporary: snapshot
    // the prior values first so they revert on return
    let _var_guard = guards::prefix_assign_guard(tree, &tree[*assignments]);
    Self::set_assignments(tree, &tree[*assignments], AssignBehavior::Export)?;

    let redirs = RedirSet::from(&tree[func.redirs]);
    let _guard = match redirs.try_apply(false) {
      RedirResult::Applied(guard) => Some(guard),
      RedirResult::NoRedirs => None,
      RedirResult::Skipped => return Ok(()),
      RedirResult::Error(e) => return Err(e),
    };

    blame.rename(func_name.clone());

    let argv = super::prepare_argv(&tree[*argv]).try_blame(blame.clone())?;

    if !func.flags.contains(NdFlags::NO_TRACE) {
      shopt::xtrace_print(&argv);
    }

    defer! {
      if let Some(trap) = Shed::logic(|l| l.get_trap(TrapTarget::Return)) {
        util::with_saved_status(|| {
          if let Err(e) = super::exec_nonint(trap, Some("trap RETURN".into())) {
            e.print_error();
          }
        });
      }
    }

    let _guard = guards::function_scope_guard(Some(argv));
    let _func_guard = Shed::meta_mut(MetaTab::enter_func);

    // getopts OPTIND variable
    // scoped per-script and per-function call
    Shed::vars_mut(|v| v.set_var("OPTIND", VarKind::Int(1), VarFlags::LOCAL)).ok();

    let Some(root) = func_body.get_root() else {
      return Err(sherr!(
          InternalErr @ tree.span_for(func_id),
          "Function body has no root node",
      ));
    };

    let mut frame = caller_contexts;
    frame.push(call_ctx);
    if let Some(ctx) = func_src_ctx {
      frame.push(ctx.clone());
    }
    let _ctx_frame = Shed::push_call_frame(frame);

    let _timer = self.take_timer();
    match self.dispatch_node(&func_body, root) {
      Ok(()) => Ok(()),
      Err(e) => match e.kind() {
        ShErrKind::FuncReturn(code) => {
          Shed::set_status(*code);
          Ok(())
        }
        ShErrKind::Raised(_, code) => {
          Shed::set_status(*code);
          if Shed::meta(MetaTab::func_depth) <= 1 {
            // raise builtin: fold the live call-frame context into the error so
            // collapse_context sees the outer call-site span, then collapse to a single label.
            let e = e.with_context(Shed::call_context().iter());
            return Err(e.collapse_context());
          }

          // nested raise, continue propagating
          Err(e)
        }
        ShErrKind::ErrInterrupt => {
          // set -e caught an error.
          Err(e.with_context(Shed::call_context().iter()))
        }
        _ => Err(e),
      },
    }
  }
}
