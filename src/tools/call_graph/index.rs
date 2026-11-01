//! Interned symbols and forward/reverse compressed sparse row adjacency.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io::Read;
use std::sync::Arc;

use serde::Serialize;

use super::{codec, ingest};

pub const MAX_INDEX_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_KB_BYTES: u64 = 512 * 1024 * 1024;
pub(super) const MAX_SYMBOLS: usize = 1_000_000;
pub(super) const MAX_EDGES: usize = 4_000_000;
pub(super) const MAX_STRINGS: usize = 3_000_000;
pub(super) const MAX_STRING_BYTES: usize = 64 * 1024 * 1024;
pub(super) const MAX_STRING: usize = 16_384;

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("index I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid Eulix JSON")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Invalid(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct GraphHit {
    pub id: String,
    pub name: String,
    pub file: String,
    pub line: u32,
    pub end_line: u32,
    /// The edge's evidence is always owned by the caller's source file.
    pub call_site_file: String,
    pub call_site_line: u32,
    pub depth: u32,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Symbol {
    pub id: u32,
    pub name: u32,
    pub file: u32,
    pub line: u32,
    pub end: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Edge {
    pub target: u32,
    pub line: u32,
}

#[derive(Debug, Default)]
pub(super) struct Csr {
    pub offsets: Vec<u32>,
    pub edges: Vec<Edge>,
}

impl Csr {
    pub fn build(count: usize, mut edges: Vec<(u32, Edge)>) -> Self {
        edges.sort_unstable();
        edges.dedup();
        let mut offsets = vec![0; count + 1];
        for (from, _) in &edges {
            offsets[*from as usize + 1] += 1;
        }
        for index in 1..offsets.len() {
            offsets[index] += offsets[index - 1];
        }
        Self { offsets, edges: edges.into_iter().map(|(_, edge)| edge).collect() }
    }

    pub fn neighbors(&self, id: u32) -> &[Edge] {
        let id = id as usize;
        &self.edges[self.offsets[id] as usize..self.offsets[id + 1] as usize]
    }
}

#[derive(Debug, Default)]
pub struct GraphIndex {
    pub(super) string_bytes: usize,
    pub(super) strings: Vec<Arc<str>>,
    pub(super) lookup: HashMap<Arc<str>, u32>,
    pub(super) symbols: Vec<Symbol>,
    pub(super) names: HashMap<u32, Vec<u32>>,
    pub(super) files: BTreeMap<u32, std::ops::Range<usize>>,
    pub(super) forward: Csr,
    pub(super) reverse: Csr,
    pub(super) snapshot: String,
    pub(super) built_at: u64,
    pub(super) parser_version: String,
}

impl GraphIndex {
    pub fn from_reader(reader: impl Read) -> Result<Self, IndexError> {
        ingest::read(reader)
    }

    pub fn from_readers(kb: impl Read, edges: impl Read) -> Result<Self, IndexError> {
        ingest::read_pair(kb, edges)
    }

    pub fn encode(&self) -> Result<Vec<u8>, IndexError> {
        codec::encode(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, IndexError> {
        codec::decode(bytes)
    }

    pub fn symbol_count(&self) -> usize {
        self.symbols.len()
    }

    pub fn edge_count(&self) -> usize {
        self.forward.edges.len()
    }

    /// Approximate resident allocations, including hash table and Arc overhead.
    pub fn memory_bytes(&self) -> usize {
        self.strings.iter().map(|s| s.len() + 80).sum::<usize>()
            + self.symbols.capacity() * 20
            + (self.forward.edges.capacity() + self.reverse.edges.capacity()) * 8
            + (self.forward.offsets.capacity() + self.reverse.offsets.capacity()) * 4
            + self.symbols.len() * 80
    }

    pub(super) fn intern(&mut self, value: &str) -> Result<u32, IndexError> {
        if let Some(id) = self.lookup.get(value) {
            return Ok(*id);
        }
        if value.len() > MAX_STRING
            || self.strings.len() >= MAX_STRINGS
            || self.string_bytes.saturating_add(value.len()) > MAX_STRING_BYTES
        {
            return Err(IndexError::Invalid("index string limit exceeded"));
        }
        let id = u32::try_from(self.strings.len())
            .map_err(|_| IndexError::Invalid("too many strings"))?;
        self.string_bytes += value.len();
        let value: Arc<str> = value.into();
        self.strings.push(value.clone());
        self.lookup.insert(value, id);
        Ok(id)
    }

    pub(super) fn rebuild_lookups(&mut self) {
        self.names.clear();
        self.files.clear();
        for (id, symbol) in self.symbols.iter().enumerate() {
            let id = u32::try_from(id).unwrap_or(u32::MAX);
            self.names.entry(symbol.name).or_default().push(id);
            self.names.entry(symbol.id).or_default().push(id);
            self.files
                .entry(symbol.file)
                .and_modify(|range| range.end += 1)
                .or_insert(id as usize..id as usize + 1);
        }
        for ids in self.names.values_mut() {
            ids.sort_unstable();
            ids.dedup();
        }
    }

    /// Exact short name or full Eulix ID; ambiguous short names include all matches.
    /// Traversal is bounded by result count, depth (8), and visited nodes (10,000).
    pub fn query(
        &self,
        name: &str,
        callers: bool,
        impact: bool,
        limit: usize,
    ) -> (Vec<GraphHit>, bool) {
        self.query_scoped(name, callers, impact, limit, None)
    }

    pub(super) fn query_scoped(
        &self,
        name: &str,
        callers: bool,
        impact: bool,
        limit: usize,
        scope: Option<&str>,
    ) -> (Vec<GraphHit>, bool) {
        let Some(ids) = self.lookup.get(name).and_then(|id| self.names.get(id)) else {
            return (Vec::new(), false);
        };
        let ids: Vec<_> = ids
            .iter()
            .copied()
            .filter(|id| {
                scope.is_none_or(|scope| {
                    let file = &self.strings[self.symbols[*id as usize].file as usize];
                    file.as_ref() == scope
                        || file.strip_prefix(scope).is_some_and(|tail| tail.starts_with('/'))
                })
            })
            .take(10_001)
            .collect();
        let too_many_seeds = ids.len() > 10_000;
        let ids = &ids[..ids.len().min(10_000)];
        let limit = limit.min(1_000);
        let adjacency = if callers { &self.reverse } else { &self.forward };
        let mut queue: VecDeque<_> = ids.iter().map(|id| (*id, 0)).collect();
        let mut visited: HashSet<_> = ids.iter().copied().collect();
        let mut hits = Vec::new();
        let mut truncated = too_many_seeds;
        while let Some((id, depth)) = queue.pop_front() {
            if depth >= 8 || (impact && visited.len() >= 10_000) {
                truncated |= !adjacency.neighbors(id).is_empty();
                continue;
            }
            for edge in adjacency.neighbors(id) {
                if impact && !visited.insert(edge.target) {
                    continue;
                }
                if hits.len() >= limit {
                    return (hits, true);
                }
                let symbol = self.symbols[edge.target as usize];
                let caller = if callers { symbol } else { self.symbols[id as usize] };
                hits.push(GraphHit {
                    id: self.strings[symbol.id as usize].to_string(),
                    name: self.strings[symbol.name as usize].to_string(),
                    file: self.strings[symbol.file as usize].to_string(),
                    line: symbol.line,
                    end_line: symbol.end,
                    call_site_file: self.strings[caller.file as usize].to_string(),
                    call_site_line: edge.line,
                    depth: depth + 1,
                });
                if impact {
                    queue.push_back((edge.target, depth + 1));
                }
            }
        }
        (hits, truncated)
    }
}

pub(super) fn safe_source_path(path: &str) -> bool {
    !path.is_empty() && path.len() <= 4_096 && !path.contains(['\\', ':', '\0'])
        && std::path::Path::new(path).components().all(|part| {
            matches!(part, std::path::Component::Normal(value) if value != ".winx" && value != ".git")
        })
}
