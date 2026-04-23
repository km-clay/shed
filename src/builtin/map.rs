use std::{collections::HashMap, fmt::Display};

use bitflags::bitflags;
use nix::{libc::STDOUT_FILENO, unistd::write};
use serde_json::{Map, Value};

use crate::sherr;
use crate::util::strops::{split_tk, split_tk_at};
use crate::{
  expand::expand_cmd_sub,
  getopt::{Opt, OptArg, OptSpec, get_opts_from_tokens_raw},
  parse::{
    NdRule, Node,
    lex::{self, LexFlags, LexStream},
  },
  procio::borrow_fd,
  state::{self, read_vars, write_vars},
  util::error::ShResult,
};

/*
 * NOTE: this is a wip builtin, not actually part of the usable set right now.
 */

#[derive(Debug, Clone, Eq, PartialEq, PartialOrd, Ord, Hash)]
pub enum BranchKey {
  Static(String),
  Wild,
}

impl From<BranchKey> for String {
  fn from(val: BranchKey) -> Self {
    match val {
      BranchKey::Static(s) => s,
      BranchKey::Wild => "%".to_string(),
    }
  }
}

impl From<String> for BranchKey {
  fn from(s: String) -> Self {
    if s == "%" {
      BranchKey::Wild
    } else {
      BranchKey::Static(s)
    }
  }
}

impl Display for BranchKey {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      BranchKey::Static(s) => write!(f, "{}", s),
      BranchKey::Wild => write!(f, "%"),
    }
  }
}

#[derive(Debug, Clone)]
pub enum MapNode {
  DynamicLeaf(String), // eval'd on access
  StaticLeaf(String),  // static value
  Array(Vec<MapNode>),
  Branch(HashMap<BranchKey, MapNode>),
}

impl Default for MapNode {
  fn default() -> Self {
    Self::Branch(HashMap::new())
  }
}

impl From<MapNode> for serde_json::Value {
  fn from(val: MapNode) -> Self {
    match val {
      MapNode::Branch(map) => {
        let val_map = map
          .into_iter()
          .map(|(k, v)| (k.into(), v.into()))
          .collect::<Map<String, Value>>();

        Value::Object(val_map)
      }
      MapNode::Array(nodes) => {
        let arr = nodes.into_iter().map(|node| node.into()).collect();
        Value::Array(arr)
      }
      MapNode::StaticLeaf(leaf) | MapNode::DynamicLeaf(leaf) => Value::String(leaf),
    }
  }
}

impl From<Value> for MapNode {
  fn from(value: Value) -> Self {
    match value {
      Value::Object(map) => {
        let node_map = map
          .into_iter()
          .map(|(k, v)| (k.into(), v.into()))
          .collect::<HashMap<BranchKey, MapNode>>();

        MapNode::Branch(node_map)
      }
      Value::Array(arr) => {
        let nodes = arr.into_iter().map(|v| v.into()).collect();
        MapNode::Array(nodes)
      }
      Value::String(s) => MapNode::StaticLeaf(s),
      v => MapNode::StaticLeaf(v.to_string()),
    }
  }
}

impl MapNode {
  fn get(&self, path: &[String]) -> Option<&MapNode> {
    match path {
      [] => Some(self),
      [key, rest @ ..] => match self {
        MapNode::StaticLeaf(_) | MapNode::DynamicLeaf(_) => None,
        MapNode::Array(map_nodes) => {
          let idx: usize = key.parse().ok()?;
          map_nodes.get(idx)?.get(rest)
        }
        MapNode::Branch(map) => map
          .get(&BranchKey::Static(key.to_string()))
          .or_else(|| map.get(&BranchKey::Wild))?
          .get(rest),
      },
    }
  }

  fn set(&mut self, path: &[String], value: MapNode) {
    match path {
      [] => *self = value,
      [key, rest @ ..] => {
        if matches!(self, MapNode::StaticLeaf(_) | MapNode::DynamicLeaf(_)) {
          // promote leaf to branch if we still have path left to traverse
          *self = Self::default();
        }
        match self {
          MapNode::Branch(map) => {
            let bkey = BranchKey::from(key.to_string());
            let child = map.entry(bkey).or_insert_with(Self::default);
            child.set(rest, value);
          }
          MapNode::Array(map_nodes) => {
            let idx: usize = key.parse().expect("expected array index");
            if idx >= map_nodes.len() {
              map_nodes.resize(idx + 1, Self::default());
            }
            map_nodes[idx].set(rest, value);
          }
          _ => unreachable!(),
        }
      }
    }
  }

  fn remove(&mut self, path: &[String]) -> Option<MapNode> {
    match path {
      [] => None,
      [key] => match self {
        MapNode::Branch(map) => map.remove(&BranchKey::Static(key.into())),
        MapNode::Array(nodes) => {
          let idx: usize = key.parse().ok()?;
          if idx >= nodes.len() {
            return None;
          }
          Some(nodes.remove(idx))
        }
        _ => None,
      },
      [key, rest @ ..] => match self {
        MapNode::Branch(map) => {
          if let Some(child) = map.get_mut(&BranchKey::Static(key.into())) {
            child.remove(rest)
          } else if let Some(child) = map.get_mut(&BranchKey::Wild) {
            child.remove(rest)
          } else {
            None
          }
        }
        MapNode::Array(nodes) => {
          let idx: usize = key.parse().ok()?;
          if idx >= nodes.len() {
            return None;
          }
          nodes[idx].remove(rest)
        }
        _ => None,
      },
    }
  }

  fn keys(&self) -> Vec<String> {
    match self {
      MapNode::Branch(map) => map.keys().map(|k| k.to_string()).collect(),
      MapNode::Array(nodes) => nodes
        .iter()
        .filter_map(|n| n.display(false, false).ok())
        .collect(),
      MapNode::StaticLeaf(_) | MapNode::DynamicLeaf(_) => vec![],
    }
  }

  fn display(&self, json: bool, pretty: bool) -> ShResult<String> {
    if json || matches!(self, MapNode::Branch(_)) {
      let val: Value = self.clone().into();
      if pretty {
        match serde_json::to_string_pretty(&val) {
          Ok(s) => Ok(s),
          Err(e) => Err(sherr!(InternalErr, "failed to serialize map: {e}")),
        }
      } else {
        match serde_json::to_string(&val) {
          Ok(s) => Ok(s),
          Err(e) => Err(sherr!(InternalErr, "failed to serialize map: {e}")),
        }
      }
    } else {
      match self {
        MapNode::StaticLeaf(leaf) => Ok(leaf.clone()),
        MapNode::DynamicLeaf(cmd) => expand_cmd_sub(cmd),
        MapNode::Array(nodes) => {
          let mut s = String::new();
          for node in nodes {
            let display = node.display(json, pretty)?;
            if matches!(node, MapNode::Branch(_)) {
              s.push_str(&format!("'{}'", display));
            } else {
              s.push_str(&node.display(json, pretty)?);
            }
            s.push('\n');
          }
          Ok(s.trim_end_matches('\n').to_string())
        }
        _ => unreachable!(),
      }
    }
  }
}

fn map_opts_spec() -> [OptSpec; 6] {
  [
    OptSpec {
      opt: Opt::Short('r'),
      takes_arg: OptArg::None,
    },
    OptSpec {
      opt: Opt::Short('j'),
      takes_arg: OptArg::None,
    },
    OptSpec {
      opt: Opt::Short('k'),
      takes_arg: OptArg::None,
    },
    OptSpec {
      opt: Opt::Long("pretty".into()),
      takes_arg: OptArg::None,
    },
    OptSpec {
      opt: Opt::Short('F'),
      takes_arg: OptArg::None,
    },
    OptSpec {
      opt: Opt::Short('l'),
      takes_arg: OptArg::None,
    },
  ]
}

#[derive(Debug, Clone, Copy)]
pub struct MapOpts {
  flags: MapFlags,
}

bitflags! {
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
  pub struct MapFlags: u32 {
    const REMOVE = 0b000001;
    const KEYS   = 0b000010;
    const JSON   = 0b000100;
    const LOCAL	 = 0b001000;
    const PRETTY = 0b010000;
    const FUNC   = 0b100000;
  }
}

pub fn map(node: Node) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  state::set_status(0);
  let (mut argv, opts) = get_opts_from_tokens_raw(argv, &map_opts_spec())?;
  let map_opts = get_map_opts(opts);
  if !argv.is_empty() {
    argv.remove(0); // remove "map" command from argv
  }

  for arg in argv {
    if let Some((lhs, rhs)) = split_tk_at(&arg, "=") {
      let path = split_tk(&lhs, ".")
        .into_iter()
        .map(|s| s.expand().map(|exp| exp.get_words().join(" ")))
        .collect::<ShResult<Vec<String>>>()?;
      let Some(name) = path.first() else {
        return Err(sherr!(InternalErr, "invalid map path: {}", lhs.as_str()));
      };

      let is_json = map_opts.flags.contains(MapFlags::JSON);
      let is_func = map_opts.flags.contains(MapFlags::FUNC);
      let is_arr = rhs.as_str().starts_with('(') && rhs.as_str().ends_with(')');
      let make_leaf = |s: String| {
        if is_func {
          MapNode::DynamicLeaf(s)
        } else {
          MapNode::StaticLeaf(s)
        }
      };
      let expanded = if is_json {
        serde_json::from_str::<Value>(rhs.as_str())
          .map_err(|e| sherr!(InternalErr, "failed to parse JSON: {e}"))?
          .into()
      } else if is_arr {
        let raw = rhs.as_str();
        let raw = raw[1..raw.len() - 1].to_string();
        let tokens = LexStream::new(raw.into(), LexFlags::empty())
          .filter(lex::not_marker)
          .try_fold(vec![], |mut acc, tk| -> ShResult<Vec<MapNode>> {
            for word in tk?.expand()?.get_words() {
              acc.push(make_leaf(word));
            }
            Ok(acc)
          })?;

        MapNode::Array(tokens)
      } else {
        make_leaf(rhs.expand()?.get_words().join(" "))
      };
      let found = write_vars(|v| -> ShResult<bool> {
        if let Some(map) = v.get_map_mut(name) {
          map.set(&path[1..], expanded.clone());
          Ok(true)
        } else {
          Ok(false)
        }
      });

      if !found? {
        let mut new = MapNode::default();
        new.set(&path[1..], expanded);
        write_vars(|v| v.set_map(name, new, map_opts.flags.contains(MapFlags::LOCAL)));
      }
    } else {
      let expanded = arg.expand()?.get_words().join(" ");
      let path: Vec<String> = expanded.split('.').map(|s| s.to_string()).collect();
      let Some(name) = path.first() else {
        return Err(sherr!(InternalErr, "invalid map path: {}", expanded));
      };

      if map_opts.flags.contains(MapFlags::REMOVE) {
        write_vars(|v| {
          if path.len() == 1 {
            v.remove_map(name);
          } else {
            let Some(map) = v.get_map_mut(name) else {
              return Err(sherr!(ExecFail, "map not found: {}", name));
            };
            map.remove(&path[1..]);
          }

          Ok(())
        })?;
        continue;
      }

      let json = map_opts.flags.contains(MapFlags::JSON);
      let pretty = map_opts.flags.contains(MapFlags::PRETTY);
      let keys = map_opts.flags.contains(MapFlags::KEYS);
      let has_map = read_vars(|v| v.get_map(name).is_some());
      if !has_map {
        return Err(sherr!(ExecFail, "map not found: {}", name));
      }
      let Some(node) = read_vars(|v| v.get_map(name).and_then(|map| map.get(&path[1..]).cloned()))
      else {
        state::set_status(1);
        continue;
      };
      let output = if !keys {
        node.display(json, pretty)?
      } else {
        let k = node.keys();
        if k.is_empty() {
          state::set_status(1);
          node.display(json, pretty)?
        } else {
          k.join(" ")
        }
      };

      let stdout = borrow_fd(STDOUT_FILENO);
      write(stdout, output.as_bytes())?;
      write(stdout, b"\n")?;
    }
  }

  Ok(())
}

pub fn get_map_opts(opts: Vec<Opt>) -> MapOpts {
  let mut map_opts = MapOpts {
    flags: MapFlags::empty(),
  };

  for opt in opts {
    match opt {
      Opt::Short('r') => map_opts.flags |= MapFlags::REMOVE,
      Opt::Short('j') => map_opts.flags |= MapFlags::JSON,
      Opt::Short('k') => map_opts.flags |= MapFlags::KEYS,
      Opt::Short('l') => map_opts.flags |= MapFlags::LOCAL,
      Opt::Long(ref s) if s == "pretty" => map_opts.flags |= MapFlags::PRETTY,
      Opt::Short('F') => map_opts.flags |= MapFlags::FUNC,
      _ => unreachable!(),
    }
  }
  map_opts
}
