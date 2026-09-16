use crate::{model::*, store::generation};
use anyhow::{Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    time::{Duration, Instant},
};

const MAX_SEEDS: usize = 20;
const MAX_EXAMINED: usize = 5_000;

// The handler belongs to this request, not the Store: stats and writes must
// not inherit a query's exhausted budget, including after an error.
struct QueryBudget<'a>(&'a Connection);
impl<'a> QueryBudget<'a> {
    fn install(conn: &'a Connection) -> Result<Self> {
        let start = Instant::now();
        let mut callbacks = 0;
        conn.progress_handler(
            1_000,
            Some(move || {
                callbacks += 1;
                callbacks >= 2_000 || start.elapsed() >= Duration::from_secs(2)
            }),
        )?;
        Ok(Self(conn))
    }
}
impl Drop for QueryBudget<'_> {
    fn drop(&mut self) {
        // Store connections are owned handles. The only possible error here
        // concerns externally borrowed handles, which Store never constructs.
        let _ = self.0.progress_handler(0, None::<fn() -> bool>);
    }
}
fn budgeted<T>(conn: &Connection, work: impl FnOnce() -> Result<T>) -> Result<T> {
    let _budget = QueryBudget::install(conn)?;
    work().map_err(|error| {
        if matches!(error.downcast_ref::<rusqlite::Error>(),
            Some(rusqlite::Error::SqliteFailure(code, _)) if code.code == rusqlite::ErrorCode::OperationInterrupted) {
            error.context("query exceeded its SQLite work/time budget; use a more specific symbol, fewer search terms, or a smaller depth/limit")
        } else { error }
    })
}

pub(crate) fn query(conn: &Connection, text: &str, options: &QueryOptions) -> Result<GraphResult> {
    budgeted(conn, || query_snapshot(conn, text, options))
}
pub(crate) fn neighbors(
    conn: &Connection,
    symbol: &str,
    options: &QueryOptions,
) -> Result<GraphResult> {
    budgeted(conn, || neighbors_snapshot(conn, symbol, options))
}
pub(crate) fn path(
    conn: &Connection,
    source: &str,
    target: &str,
    options: &QueryOptions,
) -> Result<PathResult> {
    budgeted(conn, || path_snapshot(conn, source, target, options))
}

fn validate(options: &QueryOptions) -> Result<()> {
    ensure!(options.depth <= 6, "depth must be between 0 and 6");
    ensure!(
        (1..=500).contains(&options.limit),
        "limit must be between 1 and 500"
    );
    Ok(())
}

fn empty(conn: &Connection) -> Result<GraphResult> {
    Ok(GraphResult {
        schema_version: SCHEMA_VERSION,
        generation: generation(conn)?,
        nodes: Vec::new(),
        edges: Vec::new(),
        unresolved: Vec::new(),
        truncated: false,
    })
}

fn node(conn: &Connection, id: &str) -> Result<Option<Node>> {
    let json: Option<String> = conn
        .query_row("SELECT payload FROM nodes WHERE id=?1", [id], |r| r.get(0))
        .optional()?;
    json.map(|s| serde_json::from_str(&s).map_err(Into::into))
        .transpose()
}

fn exact(conn: &Connection, text: &str, include_file: bool) -> Result<Vec<Node>> {
    if let Some(node) = node(conn, text)? {
        return Ok(vec![node]);
    }
    let mut matches = BTreeMap::new();
    let columns: &[&str] = if include_file {
        &["label", "qualified_name", "file"]
    } else {
        &["label", "qualified_name"]
    };
    for column in columns {
        // Each index range is limited before merging, including common labels.
        let sql = format!("SELECT payload FROM nodes WHERE {column}=?1 ORDER BY id LIMIT ?2");
        let mut stmt = conn.prepare(&sql)?;
        for json in stmt.query_map(params![text, (MAX_SEEDS + 1) as i64], |r| {
            r.get::<_, String>(0)
        })? {
            let n: Node = serde_json::from_str(&json?)?;
            matches.insert(n.id.clone(), n);
        }
    }
    Ok(matches.into_values().take(MAX_SEEDS + 1).collect())
}

fn unique(conn: &Connection, text: &str) -> Result<Node> {
    let nodes = exact(conn, text, false)?;
    match nodes.len() {
        0 => bail!("no symbol matches {text:?}"),
        1 => Ok(nodes.into_iter().next().unwrap()),
        _ => {
            let ids: Vec<_> = nodes.iter().map(|n| n.id.as_str()).collect();
            bail!(
                "ambiguous symbol {text:?}; use an exact ID: {}{}",
                ids.join(", "),
                if nodes.len() > MAX_SEEDS {
                    " (additional matches may exist)"
                } else {
                    ""
                }
            )
        }
    }
}

fn query_snapshot(conn: &Connection, text: &str, options: &QueryOptions) -> Result<GraphResult> {
    validate(options)?;
    let text = text.trim();
    ensure!(!text.is_empty(), "query cannot be empty");
    ensure!(text.len() <= 1024, "query exceeds 1024 bytes");
    let tx = conn.unchecked_transaction()?;
    // Reading generation pins the snapshot before any seeds are selected.
    let mut result = empty(&tx)?;
    let mut seeds = exact(&tx, text, true)?;
    if seeds.is_empty() {
        let tokens: Vec<_> = text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|s| !s.is_empty())
            .take(9)
            .collect();
        ensure!(!tokens.is_empty(), "query must contain a searchable word");
        ensure!(
            tokens.len() <= 8,
            "query must contain at most 8 search terms"
        );
        // Literal prefix tokens only: callers cannot inject FTS operators.
        // Rowid order can stop at the candidate cap; no global relevance sort.
        let expression = tokens
            .iter()
            .map(|t| format!("\"{t}\"*"))
            .collect::<Vec<_>>()
            .join(" OR ");
        let mut stmt = tx.prepare("SELECT n.payload FROM node_search s JOIN nodes n ON n.rowid=s.rowid WHERE node_search MATCH ?1 ORDER BY s.rowid LIMIT ?2")?;
        seeds = stmt
            .query_map(params![expression, (MAX_SEEDS + 1) as i64], |r| {
                r.get::<_, String>(0)
            })?
            .map(|json| Ok(serde_json::from_str(&json?)?))
            .collect::<Result<Vec<_>>>()?;
    }
    if seeds.len() > MAX_SEEDS {
        result.truncated = true;
        seeds.truncate(MAX_SEEDS);
    }
    traverse(&tx, seeds, options, &mut result)?;
    tx.commit()?;
    Ok(result)
}

fn neighbors_snapshot(
    conn: &Connection,
    symbol: &str,
    options: &QueryOptions,
) -> Result<GraphResult> {
    validate(options)?;
    let tx = conn.unchecked_transaction()?;
    let mut result = empty(&tx)?;
    let seed = unique(&tx, symbol)?;
    traverse(&tx, vec![seed], options, &mut result)?;
    tx.commit()?;
    Ok(result)
}

/// Fetch at most `budget` edge rows across indexed directional streams. The
/// stored orientation is never changed, even when traversal walks backwards.
fn adjacency(
    conn: &Connection,
    id: &str,
    options: &QueryOptions,
    budget: usize,
) -> Result<Vec<Edge>> {
    let mut edges = Vec::new();
    let streams = if options.direction == Direction::Incoming {
        [false, true]
    } else {
        [true, false]
    };
    for outgoing in streams {
        let remaining = budget - edges.len();
        if remaining == 0 {
            break;
        }
        let column = if outgoing { "source" } else { "target" };
        let undirected_only = matches!(
            (options.direction, outgoing),
            (Direction::Incoming, true) | (Direction::Outgoing, false)
        );
        let mut sql = format!("SELECT payload FROM edges WHERE {column}=?1");
        if undirected_only {
            sql.push_str(" AND directed=0");
        }
        // Do not filter self-loops in SQL: even rejected rows could make a
        // LIMIT scan an entire hub. Duplicates consume the budget and are
        // removed only after bounded retrieval.
        if options.relation.is_some() {
            sql.push_str(" AND relation=?2 ORDER BY id LIMIT ?3");
        } else {
            sql.push_str(" ORDER BY id LIMIT ?2");
        }
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = if let Some(relation) = &options.relation {
            stmt.query(params![id, relation, remaining as i64])?
        } else {
            stmt.query(params![id, remaining as i64])?
        };
        while let Some(row) = rows.next()? {
            let json: String = row.get(0)?;
            edges.push(serde_json::from_str(&json)?);
        }
    }
    Ok(edges)
}

fn opposite<'a>(edge: &'a Edge, id: &str) -> &'a str {
    if edge.source == id {
        &edge.target
    } else {
        &edge.source
    }
}

fn unresolved(
    conn: &Connection,
    id: &str,
    options: &QueryOptions,
    result: &mut GraphResult,
) -> Result<()> {
    if options.direction == Direction::Incoming {
        return Ok(());
    }
    let remaining = options.limit - result.unresolved.len();
    let mut sql =
        "SELECT payload,resolution_reason FROM refs WHERE source=?1 AND resolved_target IS NULL"
            .to_owned();
    if options.relation.is_some() {
        sql.push_str(" AND relation=?2 ORDER BY id LIMIT ?3");
    } else {
        sql.push_str(" ORDER BY id LIMIT ?2");
    }
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = if let Some(relation) = &options.relation {
        stmt.query(params![id, relation, (remaining + 1) as i64])?
    } else {
        stmt.query(params![id, (remaining + 1) as i64])?
    };
    let mut seen = 0;
    while let Some(row) = rows.next()? {
        if seen == remaining {
            result.truncated = true;
            break;
        }
        let json: String = row.get(0)?;
        let reference: Reference = serde_json::from_str(&json)?;
        result.unresolved.push(UnresolvedReference {
            source: reference.source,
            label: reference.label,
            relation: reference.relation,
            file: reference.file,
            line: reference.line,
            reason: row.get(1)?,
        });
        seen += 1;
    }
    Ok(())
}

fn traverse(
    conn: &Connection,
    seeds: Vec<Node>,
    options: &QueryOptions,
    result: &mut GraphResult,
) -> Result<()> {
    let mut visited = BTreeSet::new();
    let mut queue = VecDeque::new();
    let mut edge_ids = BTreeSet::new();
    for n in seeds {
        if result.nodes.len() == options.limit {
            result.truncated = true;
            break;
        }
        if visited.insert(n.id.clone()) {
            queue.push_back((n.id.clone(), 0));
            result.nodes.push(n);
        }
    }
    let mut examined = 0;
    while let Some((id, depth)) = queue.pop_front() {
        unresolved(conn, &id, options, result)?;
        if depth >= options.depth {
            continue;
        }
        if examined == MAX_EXAMINED {
            result.truncated = true;
            break;
        }
        let edges = adjacency(conn, &id, options, MAX_EXAMINED - examined)?;
        examined += edges.len();
        if examined == MAX_EXAMINED {
            result.truncated = true;
        }
        for edge in edges {
            if edge_ids.contains(&edge.id) {
                continue;
            }
            let next = opposite(&edge, &id);
            if !visited.contains(next) {
                if result.nodes.len() == options.limit {
                    result.truncated = true;
                    continue;
                }
                let n = node(conn, next)?
                    .ok_or_else(|| anyhow::anyhow!("edge points to missing node {next}"))?;
                visited.insert(next.to_owned());
                queue.push_back((next.to_owned(), depth + 1));
                result.nodes.push(n);
            }
            edge_ids.insert(edge.id.clone());
            result.edges.push(edge);
        }
    }
    Ok(())
}

fn path_snapshot(
    conn: &Connection,
    source: &str,
    target: &str,
    options: &QueryOptions,
) -> Result<PathResult> {
    validate(options)?;
    let tx = conn.unchecked_transaction()?;
    let mut result = empty(&tx)?;
    let start = unique(&tx, source)?;
    let end = unique(&tx, target)?;
    let mut visited = BTreeSet::from([start.id.clone()]);
    let mut queue = VecDeque::from([(start.id.clone(), 0)]);
    let mut parents: BTreeMap<String, (String, Edge)> = BTreeMap::new();
    let mut examined = 0;
    let mut found = start.id == end.id;
    'search: while !found {
        let Some((id, depth)) = queue.pop_front() else {
            break;
        };
        if examined == MAX_EXAMINED {
            result.truncated = true;
            break;
        }
        let edges = adjacency(&tx, &id, options, MAX_EXAMINED - examined)?;
        examined += edges.len();
        if examined == MAX_EXAMINED {
            result.truncated = true;
        }
        for edge in edges {
            let next = opposite(&edge, &id).to_owned();
            if visited.contains(&next) {
                continue;
            }
            if depth >= options.depth || visited.len() >= options.limit {
                result.truncated = true;
                continue;
            }
            visited.insert(next.clone());
            parents.insert(next.clone(), (id.clone(), edge));
            if next == end.id {
                found = true;
                break 'search;
            }
            queue.push_back((next, depth + 1));
        }
    }
    if found {
        let mut id = end.id.clone();
        let mut ids = vec![id.clone()];
        while id != start.id {
            let (previous, edge) = parents.remove(&id).expect("visited path has a predecessor");
            result.edges.push(edge);
            ids.push(previous.clone());
            id = previous;
        }
        result.edges.reverse();
        ids.reverse();
        for id in ids {
            result
                .nodes
                .push(node(&tx, &id)?.expect("path node exists in this snapshot"));
        }
    } else {
        // Include the resolved endpoints for an actionable unsuccessful result,
        // without implying they are connected or exceeding the node limit.
        result.nodes.push(start);
        if options.limit > 1 && result.nodes[0].id != end.id {
            result.nodes.push(end);
        }
    }
    tx.commit()?;
    Ok(PathResult {
        found,
        graph: result,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_query_removes_its_handler() -> Result<()> {
        let conn = Connection::open_in_memory()?;
        let sql = "WITH RECURSIVE numbers(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM numbers WHERE n<1000000) SELECT sum(n) FROM numbers";
        let error = budgeted(&conn, || {
            Ok(conn.query_row(sql, [], |row| row.get::<_, i64>(0))?)
        })
        .unwrap_err();
        assert!(error.to_string().contains("work/time budget"));
        // The identical operation succeeds after the request guard drops.
        let sum: i64 = conn.query_row(sql, [], |row| row.get(0))?;
        assert_eq!(sum, 500000500000);
        Ok(())
    }
}
