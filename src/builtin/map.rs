use std::collections::HashMap;

use bitflags::bitflags;
use nix::{libc::STDOUT_FILENO, unistd::write};
use serde_json::{Map, Value};

use crate::{
  getopt::{Opt, OptSpec, get_opts_from_tokens}, jobs::JobBldr, libsh::error::{ShErr, ShErrKind, ShResult}, parse::{NdRule, Node, lex::{split_all_unescaped, split_at_unescaped}}, procio::{IoStack, borrow_fd}, state::{self, read_vars, write_vars}
};

#[derive(Debug, Clone)]
pub enum MapNode {
	Leaf(String),
	Array(Vec<MapNode>),
	Branch(HashMap<String, MapNode>),
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
				let val_map = map.into_iter()
					.map(|(k,v)| {
						(k,v.into())
					})
				.collect::<Map<String,Value>>();

				Value::Object(val_map)
			}
			MapNode::Array(nodes) => {
				let arr = nodes
					.into_iter()
					.map(|node| node.into())
					.collect();
				Value::Array(arr)
			}
			MapNode::Leaf(leaf) => {
				Value::String(leaf)
			}
		}
	}
}

impl From<Value> for MapNode {
	fn from(value: Value) -> Self {
		match value {
			Value::Object(map) => {
				let node_map = map.into_iter()
					.map(|(k,v)| {
						(k, v.into())
					})
				.collect::<HashMap<String, MapNode>>();

				MapNode::Branch(node_map)
			}
			Value::Array(arr) => {
				let nodes = arr
					.into_iter()
					.map(|v| v.into())
					.collect();
				MapNode::Array(nodes)
			}
			Value::String(s) => MapNode::Leaf(s),
			v => MapNode::Leaf(v.to_string())
		}
	}
}

impl MapNode {
	fn get(&self, path: &[String]) -> Option<&MapNode> {
		match path {
			[] => Some(self),
			[key, rest @ ..] => match self {
				MapNode::Leaf(_) => None,
				MapNode::Array(map_nodes) => {
					let idx: usize = key.parse().ok()?;
					map_nodes.get(idx)?.get(rest)
				}
				MapNode::Branch(map) => map.get(key)?.get(rest)
			}
		}
	}

	fn set(&mut self, path: &[String], value: MapNode) {
		match path {
			[] => *self = value,
			[key, rest @ ..] => {
				if matches!(self, MapNode::Leaf(_)) {
					// promote leaf to branch if we still have path left to traverse
					*self = Self::default();
				}
				match self {
					MapNode::Branch(map) => {
						let child = map
							.entry(key.to_string())
							.or_insert_with(Self::default);
						child.set(rest, value);
					}
					MapNode::Array(map_nodes) => {
						let idx: usize = key.parse().expect("expected array index");
						if idx >= map_nodes.len() {
							map_nodes.resize(idx + 1, Self::default());
						}
						map_nodes[idx].set(rest, value);
					}
					_ => unreachable!()
				}
			}
		}
	}

	fn remove(&mut self, path: &[String]) -> Option<MapNode> {
		match path {
			[] => None,
			[key] => match self {
				MapNode::Branch(map) => map.remove(key),
				MapNode::Array(nodes) => {
					let idx: usize = key.parse().ok()?;
					if idx >= nodes.len() {
						return None;
					}
					Some(nodes.remove(idx))
				}
				_ => None
			}
			[key, rest @ ..] => match self {
				MapNode::Branch(map) => map.get_mut(key)?.remove(rest),
				MapNode::Array(nodes) => {
					let idx: usize = key.parse().ok()?;
					if idx >= nodes.len() {
						return None;
					}
					nodes[idx].remove(rest)
				}
				_ => None
			}
		}
	}

	fn keys(&self) -> Vec<String> {
		match self {
			MapNode::Branch(map) => map.keys().map(|k| k.to_string()).collect(),
			MapNode::Array(nodes) => nodes.iter().filter_map(|n| n.display(false, false).ok()).collect(),
			MapNode::Leaf(s) => vec![],
		}
	}

	fn display(&self, json: bool, pretty: bool) -> ShResult<String> {
		if json || matches!(self, MapNode::Branch(_)) {
			let val: Value = self.clone().into();
			if pretty {
				match serde_json::to_string_pretty(&val) {
					Ok(s) => Ok(s),
					Err(e) => Err(ShErr::simple(
							ShErrKind::InternalErr,
							format!("failed to serialize map: {e}")
					))
				}
			} else {
				match serde_json::to_string(&val) {
					Ok(s) => Ok(s),
					Err(e) => Err(ShErr::simple(
							ShErrKind::InternalErr,
							format!("failed to serialize map: {e}")
					))
				}
			}
		} else {
			match self {
				MapNode::Leaf(leaf) => Ok(leaf.clone()),
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
				_ => unreachable!()
			}
		}
	}
}

use super::setup_builtin;

fn map_opts_spec() -> [OptSpec; 5] {
	[
		OptSpec {
			opt: Opt::Short('r'),
			takes_arg: false
		},
		OptSpec {
			opt: Opt::Short('j'),
			takes_arg: false
		},
		OptSpec {
			opt: Opt::Short('k'),
			takes_arg: false
		},
		OptSpec {
			opt: Opt::Long("pretty".into()),
			takes_arg: false
		},
		OptSpec {
			opt: Opt::Short('l'),
			takes_arg: false
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
	}
}

pub fn map(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

	let (argv, opts) = get_opts_from_tokens(argv, &map_opts_spec())?;
	let map_opts = get_map_opts(opts);
  let (argv, _guard) = setup_builtin(argv, job, Some((io_stack, node.redirs)))?;

	for (arg,_) in argv {
		if let Some((lhs,rhs)) = split_at_unescaped(&arg, "=") {
			let path = split_all_unescaped(&lhs, ".");
			let Some(name) = path.first() else {
				return Err(ShErr::simple(
					ShErrKind::InternalErr,
					format!("invalid map path: {}", lhs)
				));
			};

			let is_json = map_opts.flags.contains(MapFlags::JSON);
			let found = write_vars(|v| {
				if let Some(map) = v.get_map_mut(name) {
					if is_json {
						if let Ok(parsed) = serde_json::from_str::<Value>(&rhs) {
							map.set(&path[1..], parsed.into());
						} else {
							map.set(&path[1..], MapNode::Leaf(rhs.clone()));
						}
					} else {
						map.set(&path[1..], MapNode::Leaf(rhs.clone()));
					}
					true
				} else {
					false
				}
			});

			if !found {
				let mut new = MapNode::default();
				if is_json && let Ok(parsed) = serde_json::from_str::<Value>(&rhs) {
					let node: MapNode = parsed.into();
					new.set(&path[1..], node);
				} else {
					new.set(&path[1..], MapNode::Leaf(rhs));
				}
				write_vars(|v| v.set_map(name, new, map_opts.flags.contains(MapFlags::LOCAL)));
			}
		} else {
			let path = split_all_unescaped(&arg, ".");
			let Some(name) = path.first() else {
				return Err(ShErr::simple(
					ShErrKind::InternalErr,
					format!("invalid map path: {}", &arg)
				));
			};

			if map_opts.flags.contains(MapFlags::REMOVE) {
				write_vars(|v| {
					if path.len() == 1 {
						v.remove_map(name);
					} else {
						let Some(map) = v.get_map_mut(name) else {
							return Err(ShErr::simple(
								ShErrKind::ExecFail,
								format!("map not found: {}", name)
							));
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
				return Err(ShErr::simple(
					ShErrKind::ExecFail,
					format!("map not found: {}", name)
				));
			}
			let Some(output) = read_vars(|v| {
				v.get_map(name)
					.and_then(|map| map.get(&path[1..])
						.and_then(|n| {
							if keys {
								Some(n.keys().join(" "))
							} else {
								n.display(json, pretty).ok()
							}
						}))
			}) else {
				state::set_status(1);
				continue;
			};

			let stdout = borrow_fd(STDOUT_FILENO);
			write(stdout, output.as_bytes())?;
			write(stdout, b"\n")?;
		}
	}

  state::set_status(0);
  Ok(())
}

pub fn get_map_opts(opts: Vec<Opt>) -> MapOpts {
	let mut map_opts = MapOpts {
		flags: MapFlags::empty()
	};

	for opt in opts {
		match opt {
			Opt::Short('r') => map_opts.flags |= MapFlags::REMOVE,
			Opt::Short('j') => map_opts.flags |= MapFlags::JSON,
			Opt::Short('k') => map_opts.flags |= MapFlags::KEYS,
			Opt::Short('l') => map_opts.flags |= MapFlags::LOCAL,
			Opt::Long(ref s) if s == "pretty" => map_opts.flags |= MapFlags::PRETTY,
			_ => unreachable!()
		}
	}
	map_opts
}
