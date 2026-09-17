//! Explicit node-link and Graphify-export snapshot import.
//!
//! In strict node-link mode the root requires `nodes`, exactly one of `links`/`edges`, and boolean
//! `directed`/`multigraph`. Nodes require a string or integer `id`; edges require
//! `source`/`target` (or agreeing `from`/`to` aliases). Strings remain unchanged;
//! integers in i64/u64 range become `graphify:integer:<decimal>`. Floating-point
//! IDs and collisions with that string namespace are rejected.
//!
//! Missing labels default to the ID, file types to `concept`, node source files
//! to an empty string, relations to `related`, and confidence to `UNKNOWN`.
//! Missing edge source files remain absent. Present
//! values must have the correct type. Node aliases are `name` for `label` and
//! `source`/`path` for `source_file`; edge `type` aliases `relation`. Aliases must
//! agree. Confidence is a nonempty string; optional confidence_score is 0..=1.
//!
//! `_src` and `_tgt` must appear together and name the same endpoint pair. They
//! orient an undirected legacy edge; on a directed graph they must agree with
//! source/target order. An optional edge `directed` must agree with this result.
//! Simple graphs reject repeated endpoint pairs; multigraphs require distinct
//! string/integer keys per pair. Imported edge IDs are input-order ordinals.
//!
//! Source locations are null or strings. `L12`, `L12-L15`, and `L12-15` produce
//! line numbers; other textual locations remain opaque. All original node/edge
//! fields are retained in metadata, as are all root fields except the node/edge
//! arrays. `graph`, if present, must be an object. Hyperedges at the root or in
//! `graph` must be empty arrays. Duplicate JSON keys are errors at every depth.
//! Only the supplied regular file is read, with a hard 256 MiB limit.
//!
//! [`read_graphify_export`] explicitly selects Graphify producer semantics:
//! missing root flags are permitted (default false for storage identity).
//! Every edge is directed, using `_src`/`_tgt` when present, otherwise ordered
//! source/target. Markers must name the same unordered endpoint pair, even if
//! the root says directed=true. Root and edge direction flags describe storage;
//! present flags must still be booleans, but do not override export direction.
//! Without multigraph=true, keys are forbidden and every unkeyed fact survives,
//! including repeated pairs/relations. With multigraph=true, every edge needs a
//! string/integer key unique per serialized endpoint pair: ordered when the
//! root directed=true, unordered otherwise. All modes assign ordinal edge IDs
//! and retain original metadata without inserting or rewriting flags.

use std::collections::HashSet;
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Number, Value};

use crate::model::{Edge, ImportedGraph, Node};

const MAX_BYTES: u64 = 256 * 1024 * 1024;

pub fn read_graphify(path: &Path) -> Result<ImportedGraph> {
    read_snapshot(path, false)
}

/// Read a Graphify-produced export, including raw no-cluster extraction/update.
/// See the module contract for direction and parallel-edge interpretation.
pub fn read_graphify_export(path: &Path) -> Result<ImportedGraph> {
    read_snapshot(path, true)
}

fn read_snapshot(path: &Path, export: bool) -> Result<ImportedGraph> {
    // Check before open: opening a FIFO with no writer would otherwise block.
    // Symlinks to regular files remain supported. This is not an atomic defense
    // against a path being replaced with a FIFO between metadata and open.
    ensure!(
        std::fs::metadata(path)
            .context("cannot inspect Graphify snapshot")?
            .is_file(),
        "Graphify snapshot must be a regular file"
    );
    let file = File::open(path).context("cannot open Graphify snapshot")?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file(),
        "Graphify snapshot must be a regular file"
    );
    ensure!(
        metadata.len() <= MAX_BYTES,
        "Graphify snapshot exceeds 256 MiB limit"
    );
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("cannot read Graphify snapshot")?;
    ensure!(
        bytes.len() as u64 <= MAX_BYTES,
        "Graphify snapshot exceeds 256 MiB limit"
    );
    let StrictJson(value) = serde_json::from_slice(&bytes).context("invalid Graphify JSON")?;
    let mut root = into_object(value).context("Graphify root must be an object")?;
    let directed = if export && !root.contains_key("directed") {
        false
    } else {
        required_bool(&root, "directed")?
    };
    let multigraph = if export && !root.contains_key("multigraph") {
        false
    } else {
        required_bool(&root, "multigraph")?
    };
    empty_hyperedges(&root)?;
    if let Some(graph) = root.get("graph") {
        empty_hyperedges(graph.as_object().context("graph must be an object")?)?;
    }
    ensure!(
        root.contains_key("links") != root.contains_key("edges"),
        "expected exactly one of links or edges"
    );
    let nodes = take_array(&mut root, "nodes")?;
    let edge_field = if root.contains_key("links") {
        "links"
    } else {
        "edges"
    };
    let edges = take_array(&mut root, edge_field)?;
    let mut ids = HashSet::new();
    let mut mapped_ids = HashSet::new();
    let mut imported_nodes = Vec::with_capacity(nodes.len());
    for (index, value) in nodes.into_iter().enumerate() {
        let node = (|| -> Result<Node> {
            let attrs = into_object(value)?;
            let id = Id::parse(attrs.get("id").context("missing id")?)?;
            ensure!(ids.insert(id.clone()), "duplicate node ID");
            let mapped = id.mapped();
            ensure!(
                mapped_ids.insert(mapped.clone()),
                "numeric ID mapping collides with a string ID"
            );
            let label = text_alias(&attrs, &["label", "name"])?
                .unwrap_or(&mapped)
                .to_owned();
            let kind = text_alias(&attrs, &["file_type"])?
                .unwrap_or("concept")
                .to_owned();
            let file = text_alias(&attrs, &["source_file", "source", "path"])?
                .unwrap_or("")
                .to_owned();
            let (line, end_line) = location(&attrs)?;
            let qualified_name = text_alias(&attrs, &["qualified_name"])?.map(str::to_owned);
            Ok(Node {
                id: mapped,
                label,
                kind,
                file,
                line,
                end_line,
                qualified_name,
                binding_key: None,
                metadata: Value::Object(attrs),
            })
        })()
        .with_context(|| format!("nodes[{index}]"))?;
        imported_nodes.push(node);
    }
    let mut identities = HashSet::new();
    let mut imported_edges = Vec::with_capacity(edges.len());
    for (index, value) in edges.into_iter().enumerate() {
        let edge = (|| -> Result<Edge> {
            let attrs = into_object(value)?;
            let source = endpoint(&attrs, &["source", "from"], &ids)?;
            let target = endpoint(&attrs, &["target", "to"], &ids)?;
            let key = if multigraph {
                Some(Id::parse(
                    attrs.get("key").context("multigraph edge requires key")?,
                )?)
            } else {
                ensure!(!attrs.contains_key("key"), "key requires multigraph=true");
                None
            };
            // Identity follows the serialized graph's direction, even when legacy
            // markers recover a direction from an undirected storage format.
            let pair = if !directed && source > target {
                (target.clone(), source.clone())
            } else {
                (source.clone(), target.clone())
            };
            if !export || multigraph {
                ensure!(identities.insert((pair, key)), "duplicate edge identity");
            }
            let (source, target, edge_directed) = match (attrs.get("_src"), attrs.get("_tgt")) {
                (None, None) => (source, target, export || directed),
                (Some(_), Some(_)) => {
                    let src = endpoint(&attrs, &["_src"], &ids)?;
                    let tgt = endpoint(&attrs, &["_tgt"], &ids)?;
                    let same = src == source && tgt == target;
                    let reversed = src == target && tgt == source;
                    ensure!(
                        same || ((export || !directed) && reversed),
                        "conflicting _src/_tgt orientation"
                    );
                    (src, tgt, true)
                }
                _ => bail!("_src and _tgt must appear together"),
            };
            if attrs.contains_key("directed") {
                let edge_flag = required_bool(&attrs, "directed")?;
                ensure!(
                    export || edge_flag == edge_directed,
                    "edge directed conflicts with graph direction or _src/_tgt"
                );
            }
            let relation = text_alias(&attrs, &["relation", "type"])?
                .unwrap_or("related")
                .to_owned();
            ensure!(!relation.is_empty(), "relation must not be empty");
            let confidence = text_alias(&attrs, &["confidence"])?
                .unwrap_or("UNKNOWN")
                .to_owned();
            ensure!(!confidence.is_empty(), "confidence must not be empty");
            if let Some(score) = attrs.get("confidence_score") {
                let score = score
                    .as_f64()
                    .context("confidence_score must be a number")?;
                ensure!(
                    (0.0..=1.0).contains(&score),
                    "confidence_score must be between 0 and 1"
                );
            }
            let file = text_alias(&attrs, &["source_file"])?.map(str::to_owned);
            let (line, _) = location(&attrs)?;
            Ok(Edge {
                id: format!("graphify:edge:{index}"),
                source: source.mapped(),
                target: target.mapped(),
                relation,
                directed: edge_directed,
                file,
                line,
                confidence,
                metadata: Value::Object(attrs),
            })
        })()
        .with_context(|| format!("{edge_field}[{index}]"))?;
        imported_edges.push(edge);
    }
    Ok(ImportedGraph {
        nodes: imported_nodes,
        edges: imported_edges,
        metadata: Value::Object(root),
    })
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum Id {
    Text(String),
    Integer(String),
}

impl Id {
    fn parse(value: &Value) -> Result<Self> {
        match value {
            Value::String(s) => Ok(Self::Text(s.clone())),
            Value::Number(n) if n.is_i64() || n.is_u64() => Ok(Self::Integer(n.to_string())),
            _ => bail!(
                "ID/key must be a string or an integer in i64/u64 range; floating-point IDs are unsupported"
            ),
        }
    }

    fn mapped(&self) -> String {
        match self {
            Self::Text(s) => s.clone(),
            Self::Integer(n) => format!("graphify:integer:{n}"),
        }
    }
}

fn into_object(value: Value) -> Result<Map<String, Value>> {
    match value {
        Value::Object(map) => Ok(map),
        _ => bail!("expected an object"),
    }
}

fn take_array(root: &mut Map<String, Value>, key: &str) -> Result<Vec<Value>> {
    match root.remove(key) {
        Some(Value::Array(items)) => Ok(items),
        _ => bail!("{key} must be an array"),
    }
}

fn required_bool(attrs: &Map<String, Value>, key: &str) -> Result<bool> {
    attrs
        .get(key)
        .and_then(Value::as_bool)
        .with_context(|| format!("{key} must be a boolean"))
}

fn empty_hyperedges(attrs: &Map<String, Value>) -> Result<()> {
    if let Some(value) = attrs.get("hyperedges") {
        ensure!(
            value.as_array().is_some_and(Vec::is_empty),
            "hyperedges must be an empty array; hyperedge import is unsupported"
        );
    }
    Ok(())
}

fn alias<'a>(attrs: &'a Map<String, Value>, names: &[&str]) -> Result<Option<&'a Value>> {
    let mut value = None;
    for name in names {
        if let Some(next) = attrs.get(*name) {
            ensure!(
                value.is_none_or(|previous| previous == next),
                "conflicting aliases: {}",
                names.join("/")
            );
            value = Some(next);
        }
    }
    Ok(value)
}

fn text_alias<'a>(attrs: &'a Map<String, Value>, names: &[&str]) -> Result<Option<&'a str>> {
    alias(attrs, names)?
        .map(|v| {
            v.as_str()
                .with_context(|| format!("{} must be a string", names[0]))
        })
        .transpose()
}

fn endpoint(attrs: &Map<String, Value>, names: &[&str], ids: &HashSet<Id>) -> Result<Id> {
    let value = alias(attrs, names)?.with_context(|| format!("missing {}", names[0]))?;
    let id = Id::parse(value)?;
    ensure!(
        ids.contains(&id),
        "{} references an unknown node ID",
        names[0]
    );
    Ok(id)
}

fn location(attrs: &Map<String, Value>) -> Result<(Option<u32>, Option<u32>)> {
    let text = match attrs.get("source_location") {
        None | Some(Value::Null) => return Ok((None, None)),
        Some(Value::String(s)) => s,
        _ => bail!("source_location must be a string or null"),
    };
    let Some(body) = text
        .strip_prefix('L')
        .filter(|s| s.starts_with(|c: char| c.is_ascii_digit()))
    else {
        return Ok((None, None));
    };
    let (start, end) = body.split_once('-').map_or((body, None), |(a, b)| {
        (a, Some(b.strip_prefix('L').unwrap_or(b)))
    });
    let parse = |s: &str| -> Result<u32> {
        ensure!(
            !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()),
            "invalid source line number"
        );
        let n: u32 = s.parse().context("source line number exceeds u32 range")?;
        ensure!(n > 0, "source line numbers are 1-based");
        Ok(n)
    };
    let start = parse(start)?;
    let end = end.map(parse).transpose()?;
    ensure!(
        end.is_none_or(|end| end >= start),
        "source location range is reversed"
    );
    Ok((Some(start), end))
}

// serde_json::Value alone silently overwrites duplicate object keys. Retain its
// ordinary JSON representation, but reject ambiguous objects while deserializing.
struct StrictJson(Value);

impl<'de> Deserialize<'de> for StrictJson {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct JsonVisitor;
        impl<'de> Visitor<'de> for JsonVisitor {
            type Value = StrictJson;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("JSON without duplicate object keys")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> std::result::Result<StrictJson, E> {
                Ok(StrictJson(Value::Bool(v)))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<StrictJson, E> {
                Ok(StrictJson(Value::Number(v.into())))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<StrictJson, E> {
                Ok(StrictJson(Value::Number(v.into())))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<StrictJson, E> {
                Number::from_f64(v)
                    .map(|n| StrictJson(Value::Number(n)))
                    .ok_or_else(|| E::custom("non-finite number"))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<StrictJson, E> {
                Ok(StrictJson(Value::String(v.into())))
            }
            fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<StrictJson, E> {
                Ok(StrictJson(Value::String(v)))
            }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<StrictJson, E> {
                Ok(StrictJson(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<StrictJson, A::Error> {
                let mut values = Vec::new();
                while let Some(StrictJson(value)) = seq.next_element()? {
                    values.push(value);
                }
                Ok(StrictJson(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<StrictJson, A::Error> {
                let mut values = Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(de::Error::custom("duplicate JSON object key"));
                    }
                    let StrictJson(value) = map.next_value()?;
                    values.insert(key, value);
                }
                Ok(StrictJson(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(JsonVisitor)
    }
}
