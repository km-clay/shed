//! Execution of variable assignments
//!
//! This module handles the execution of variable assignments, including:
//! * parsing assignment nodes from the AST
//! * expanding variable values
//! * applying assignments to the shell's variable state
//! * handling compound assignments and array indexing
//! * managing export behavior and environment variable updates
//!
//! Also handles assignment arithmetic operations like `+=`, `-=`, `*=`, and `/=` for integer and string variables,
//! as well as array appending for indexed arrays.

use std::collections::VecDeque;

use itertools::Itertools;

use crate::{
  eval::parse::AssignKind,
  expand::{arithmetic, escape},
  sherr,
  state::{
    Shed,
    meta::MetaTab,
    params, shopt,
    vars::{Var, VarFlags, VarKind, VarKindTag, VarStr},
  },
  util::error::ShResult,
};

use super::{Ast, NdFlags, NdRule, NodeId, Span};

#[derive(Debug, Clone, Copy)]
pub(crate) enum AssignBehavior {
  Export,
  Set,
}

impl super::Dispatcher {
  pub(crate) fn set_assignments(
    tree: &Ast,
    assigns: &[NodeId],
    behavior: AssignBehavior,
  ) -> ShResult<Vec<VarStr>> {
    let mut new_env_vars = vec![];
    let mut flags = match behavior {
      AssignBehavior::Export => VarFlags::EXPORT,
      AssignBehavior::Set => VarFlags::empty(),
    };
    if Shed::shopts(|o| o.set.allexport) {
      flags = VarFlags::EXPORT;
    }

    let trace = Shed::shopts(|o| o.set.xtrace);

    for assign_id in assigns {
      let assign = &tree[*assign_id];
      let is_arr = assign.flags.contains(NdFlags::ARR_ASSIGN);
      let span = tree[assign.span].clone();
      let NdRule::Assignment { kind, var, val } = &assign.class else {
        unreachable!()
      };
      let old_status = Shed::get_status();
      let var_name = &tree[*var].span.to_str_lossy();
      let is_integer = !is_arr
        && Shed::vars(|v| v.get_var_flags(var_name)).is_some_and(|f| f.contains(VarFlags::INTEGER));
      let val = if is_arr {
        VarKind::arr_from_tk(&tree[*val])?
      } else if is_integer {
        let raw = tree[*val].expand_no_split()?;
        let n = arithmetic::expand_arithmetic(raw.as_bytes())
          .ok()
          .and_then(|s| s.to_str_lossy().parse::<i32>().ok())
          .unwrap_or(0);
        VarKind::Int(n)
      } else {
        VarKind::string(tree[*val].expand_no_split()?)
      };

      // Parse and expand array index BEFORE entering write_vars borrow
      let indexed = params::parse_arr_bracket(var_name.as_bytes())
        .map(|(name, idx_raw)| {
          params::expand_arr_index(idx_raw.as_bytes(), true).map(|idx| (name, idx))
        })
        .transpose()?;

      let indexed = if let Some((name, idx)) = indexed {
        let tag =
          Shed::vars(|v| v.try_get_var_kind_tag(&name.to_str_lossy())).unwrap_or(VarKindTag::Arr);
        Some((name, idx.resolve_for(tag)?))
      } else {
        None
      };

      if trace {
        let op = match kind {
          AssignKind::Eq => "=",
          AssignKind::PlusEq => "+=",
          AssignKind::MinusEq => "-=",
          AssignKind::MultEq => "*=",
          AssignKind::DivEq => "/=",
        };
        // Arrays render as `(a b c)`, matching bash's trace; scalars/ints use
        // their plain value.
        // bash quotes only the value (`x='a b'`, `a=('c d' e)`), not the
        // `name=` prefix, so quote the value/elements and emit the assembled
        // line directly (xtrace_line doesn't re-quote).
        let rhs = match &val {
          VarKind::Arr(items) => {
            let items = items
              .iter()
              .map(|i| escape::xtrace_quote(&i.to_str_lossy()))
              .join(" ");
            format!("({items})")
          }
          other => escape::xtrace_quote(&other.to_string()),
        };
        shopt::xtrace_line(&format!("{var_name}{op}{rhs}"));
      }

      match kind {
        AssignKind::Eq => {
          if let Some((name, idx)) = indexed {
            Shed::vars_mut(|v| {
              v.set_var_indexed(&name.to_str_lossy(), idx, val.to_string(), flags)
            })?;
          } else {
            Shed::vars_mut(|v| v.set_var(var_name, val.clone(), flags))?;
          }
        }
        op
        @ (AssignKind::PlusEq | AssignKind::MinusEq | AssignKind::MultEq | AssignKind::DivEq) => {
          if matches!(op, AssignKind::PlusEq)
            && indexed.is_none()
            && matches!(behavior, AssignBehavior::Set)
          {
            let took_fast = Shed::vars_mut(|v| -> ShResult<bool> {
              let Ok(items) = v.get_arr_mut(var_name) else {
                return Ok(false);
              };
              match &val {
                VarKind::Int(n) => items.push_back(n.to_string().into()),
                VarKind::Str(s) => items.push_back(s.clone()),
                VarKind::Arr(other) => items.extend(other.iter().cloned()),
                VarKind::Magic(n) => {
                  if let Some(s) = n() {
                    items.push_back(s);
                  }
                }
                VarKind::AssocArr(_) => {
                  return Err(sherr!(
                      InvalidAssignment @ span.clone(),
                      "cannot append associative array to indexed array"
                  ));
                }
                VarKind::Unset => {
                  return Err(sherr!(
                      InvalidAssignment @ span.clone(),
                      "cannot append unset value to indexed array"
                  ));
                }
              }
              Ok(true)
            })?;
            if took_fast {
              let status = if matches!(behavior, AssignBehavior::Set) {
                // Assignment-only command: exit status is the last command
                // substitution's status (or 0), not the pre-assignment status.
                Shed::meta(MetaTab::last_cmdsub_status).unwrap_or(0)
              } else {
                Shed::meta(MetaTab::last_cmdsub_status).unwrap_or(old_status)
              };

              Shed::set_status(status);
              continue;
            }
          }

          let mut var = if let Some((name, idx)) = &indexed {
            Shed::vars(|v| v.index_var(&name.to_str_lossy(), idx))?.into()
          } else {
            Shed::vars(|v| v.try_get_var_meta(var_name)).unwrap_or_else(|| {
              let kind = if is_arr {
                VarKind::Arr(VecDeque::new())
              } else {
                VarKind::string(VarStr::default())
              };
              Var::new(kind, VarFlags::empty())
            })
          };

          let op_name = match op {
            AssignKind::PlusEq => "add to",
            AssignKind::MinusEq => "subtract from",
            AssignKind::MultEq => "multiply",
            AssignKind::DivEq => "divide",
            AssignKind::Eq => unreachable!(),
          };

          let parse_rhs = |span: &Span| -> ShResult<i32> {
            val.to_string().parse::<i32>().map_err(
              |_| sherr!(InvalidAssignment @ span.clone(), "cannot {op_name} non-integer value"),
            )
          };

          let check_div_zero = |other: i32, span: &Span| -> ShResult<()> {
            if matches!(op, AssignKind::DivEq) && other == 0 {
              return Err(sherr!(InvalidAssignment @ span.clone(), "division by zero"));
            }
            Ok(())
          };

          // A declared-but-unset variable behaves as the zero value of its type
          // for compound assignment (`local x; x+=foo` -> "foo"; a `-i` var
          // starts from 0), so normalize it before applying the operator.
          if matches!(var.kind(), VarKind::Unset) {
            let zero = if var.flags().contains(VarFlags::INTEGER) {
              VarKind::Int(0)
            } else if is_arr {
              VarKind::Arr(VecDeque::new())
            } else {
              VarKind::string(VarStr::default())
            };
            *var.kind_mut() = zero;
          }

          match var.kind_mut() {
            VarKind::Str(s) => {
              if matches!(op, AssignKind::PlusEq) {
                let other = val.to_string();
                *s = format!("{}{other}", s.to_str_lossy()).into();
              } else {
                let n = s.to_str_lossy().parse::<i32>().map_err(
                  |_| sherr!(InvalidAssignment @ span.clone(), "cannot {op_name} string variable"),
                )?;
                let other = parse_rhs(&span)?;
                check_div_zero(other, &span)?;
                *s = match op {
                  AssignKind::MinusEq => (n - other).to_string().into(),
                  AssignKind::MultEq => (n * other).to_string().into(),
                  AssignKind::DivEq => (n / other).to_string().into(),
                  _ => unreachable!(),
                };
              }
            }
            VarKind::Int(n) => {
              let other = parse_rhs(&span)?;
              check_div_zero(other, &span)?;
              match op {
                AssignKind::PlusEq => *n += other,
                AssignKind::MinusEq => *n -= other,
                AssignKind::MultEq => *n *= other,
                AssignKind::DivEq => *n /= other,
                AssignKind::Eq => unreachable!(),
              }
            }
            VarKind::Arr(items) => {
              if matches!(op, AssignKind::PlusEq) {
                match &val {
                  VarKind::Int(n) => items.push_back(n.to_string().into()),
                  VarKind::Str(s) => items.push_back(s.clone()),
                  VarKind::Arr(other) => items.extend(other.clone()),
                  VarKind::Magic(n) => {
                    if let Some(s) = n() {
                      items.push_back(s);
                    }
                  }
                  VarKind::AssocArr(_) => {
                    return Err(sherr!(
                        InvalidAssignment @ span,
                        "cannot append associative array to indexed array"
                    ));
                  }
                  VarKind::Unset => {
                    return Err(sherr!(
                        InvalidAssignment @ span,
                        "cannot append unset value to indexed array"
                    ));
                  }
                }
              } else {
                return Err(sherr!(
                    InvalidAssignment @ span,
                    "cannot {op_name} array variable"
                ));
              }
            }
            VarKind::Magic(_) => {
              return Err(sherr!(
                  InvalidAssignment @ span,
                  "cannot {op_name} magic variable"
              ));
            }
            VarKind::AssocArr(_) => {
              return Err(sherr!(
                  InvalidAssignment @ span,
                  "cannot {op_name} associative array variable"
              ));
            }
            VarKind::Unset => {
              return Err(sherr!(
                  InvalidAssignment @ span,
                  "cannot {op_name} unset variable"
              ));
            }
          }

          let indexed = if let Some((name, idx)) = indexed {
            let tag = Shed::vars(|v| v.try_get_var_kind_tag(&name.to_str_lossy()))
              .unwrap_or(VarKindTag::Arr);
            Some((name, idx.resolve_for(tag)?))
          } else {
            None
          };

          if let Some((name, idx)) = indexed {
            Shed::vars_mut(|v| v.update_var_indexed(&name.to_str_lossy(), idx, var.to_string()))?;
          } else {
            Shed::vars_mut(|v| v.update_var(var_name, var.kind().clone()))?;
          }
        }
      }

      let status = if matches!(behavior, AssignBehavior::Set) {
        // Assignment-only command: exit status is the last command
        // substitution's status (or 0), not the pre-assignment status.
        Shed::meta(MetaTab::last_cmdsub_status).unwrap_or(0)
      } else {
        Shed::meta(MetaTab::last_cmdsub_status).unwrap_or(old_status)
      };
      Shed::set_status(status);

      if matches!(behavior, AssignBehavior::Export) {
        new_env_vars.push(var_name.as_bytes().into());
      }
    }

    Ok(new_env_vars)
  }
}
