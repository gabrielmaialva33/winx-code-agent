//! Streaming KB ingestion. Unused metadata and AST fields never enter a JSON DOM.

use std::collections::HashMap;
use std::fmt;
use std::io::{BufReader, Read};

use serde::de::{DeserializeSeed, Error as _, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

use super::index::{
    safe_source_path, Csr, Edge, GraphIndex, IndexError, Symbol, MAX_EDGES, MAX_KB_BYTES,
    MAX_STRING_BYTES, MAX_SYMBOLS,
};

#[derive(Default)]
struct Builder {
    graph: GraphIndex,
    edges: Vec<(u32, u32, u32)>,
    has_graph: bool,
}

impl Builder {
    fn function(&mut self, file: u32, function: &Function) -> Result<(), IndexError> {
        if self.graph.symbols.len() >= MAX_SYMBOLS
            || function.line_start == 0
            || function.line_end < function.line_start
            || function.id.is_empty()
            || function.name.is_empty()
        {
            return Err(IndexError::Invalid("invalid symbol or symbol limit exceeded"));
        }
        let id = self.graph.intern(&function.id)?;
        let name = self.graph.intern(&function.name)?;
        self.graph.symbols.push(Symbol {
            id,
            name,
            file,
            line: function.line_start,
            end: function.line_end,
        });
        Ok(())
    }

    fn finish(mut self) -> Result<GraphIndex, IndexError> {
        if self.graph.strings.iter().map(|s| s.len()).sum::<usize>() > MAX_STRING_BYTES {
            return Err(IndexError::Invalid("index string bytes exceeded"));
        }
        self.graph.symbols.sort_unstable_by_key(|s| (s.file, s.line, s.id));
        self.graph.symbols.dedup_by_key(|symbol| (symbol.file, symbol.line, symbol.id));
        let mut ids: HashMap<u32, Vec<u32>> = HashMap::new();
        for (index, symbol) in self.graph.symbols.iter().enumerate() {
            ids.entry(symbol.id)
                .or_default()
                .push(u32::try_from(index).map_err(|_| IndexError::Invalid("symbol overflow"))?);
        }
        let mut forward = Vec::new();
        let mut reverse = Vec::new();
        for (from, to, line) in self.edges {
            if let (Some(sources), Some(targets)) = (ids.get(&from), ids.get(&to)) {
                for &from in sources {
                    let caller = self.graph.symbols[from as usize];
                    if line < caller.line || line > caller.end {
                        continue;
                    }
                    for &to in targets {
                        if forward.len() >= MAX_EDGES {
                            return Err(IndexError::Invalid("resolved edge limit exceeded"));
                        }
                        forward.push((from, Edge { target: to, line }));
                        reverse.push((to, Edge { target: from, line }));
                    }
                }
            }
        }
        self.graph.forward = Csr::build(self.graph.symbols.len(), forward);
        self.graph.reverse = Csr::build(self.graph.symbols.len(), reverse);
        self.graph.rebuild_lookups();
        Ok(self.graph)
    }
}

#[derive(Deserialize)]
struct Function {
    id: String,
    name: String,
    line_start: u32,
    line_end: u32,
}
#[derive(Deserialize)]
struct RawEdge {
    from: String,
    to: String,
    edge_type: String,
    call_site_line: u32,
}

#[derive(Clone, Copy)]
enum Section {
    Root,
    Structure,
    File(u32),
    Functions(u32),
    Classes(u32),
    Class(u32),
    Graph,
    Edges,
}
struct Seed<'a> {
    builder: &'a mut Builder,
    section: Section,
}

impl<'de> DeserializeSeed<'de> for Seed<'_> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, de: D) -> Result<(), D::Error> {
        match self.section {
            Section::Functions(_) | Section::Classes(_) | Section::Edges => {
                de.deserialize_seq(self)
            }
            _ => de.deserialize_map(self),
        }
    }
}

impl<'de> Visitor<'de> for Seed<'_> {
    type Value = ();
    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("Eulix knowledge base object or array")
    }
    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<(), M::Error> {
        let mut structure = false;

        while let Some(key) = map.next_key::<String>()? {
            let section = match (self.section, key.as_str()) {
                (Section::Root, "structure") => {
                    structure = true;
                    Some(Section::Structure)
                }
                (Section::Root, "call_graph") => Some(Section::Graph),
                (Section::Structure, path) => {
                    if !safe_source_path(path) {
                        return Err(M::Error::custom("invalid source path"));
                    }
                    let file = self.builder.graph.intern(path).map_err(M::Error::custom)?;
                    Some(Section::File(file))
                }
                (Section::File(file), "functions") | (Section::Class(file), "methods") => {
                    Some(Section::Functions(file))
                }
                (Section::File(file), "classes") => Some(Section::Classes(file)),
                (Section::Graph, "edges") => {
                    self.builder.has_graph = true;
                    Some(Section::Edges)
                }
                _ => None,
            };
            if let Some(section) = section {
                map.next_value_seed(Seed { builder: self.builder, section })?;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        if matches!(self.section, Section::Root) && !structure {
            return Err(M::Error::custom("structure is required"));
        }
        Ok(())
    }
    fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<(), S::Error> {
        match self.section {
            Section::Functions(file) => {
                while let Some(function) = seq.next_element::<Function>()? {
                    self.builder.function(file, &function).map_err(S::Error::custom)?;
                }
            }
            Section::Classes(file) => {
                while let Some(()) = seq.next_element_seed(Seed {
                    builder: self.builder,
                    section: Section::Class(file),
                })? {}
            }
            Section::Edges => {
                while let Some(edge) = seq.next_element::<RawEdge>()? {
                    if edge.edge_type != "calls" && edge.edge_type != "call" {
                        continue;
                    }
                    if self.builder.edges.len() >= MAX_EDGES {
                        return Err(S::Error::custom("edge limit exceeded"));
                    }
                    let from = self.builder.graph.intern(&edge.from).map_err(S::Error::custom)?;
                    let to = self.builder.graph.intern(&edge.to).map_err(S::Error::custom)?;
                    self.builder.edges.push((from, to, edge.call_site_line));
                }
            }
            _ => return Err(S::Error::custom("unexpected sequence")),
        }
        Ok(())
    }
}

fn read_part(reader: impl Read, builder: &mut Builder, section: Section) -> Result<(), IndexError> {
    let mut limited = reader.take(MAX_KB_BYTES + 1);
    let mut de =
        serde_json::Deserializer::from_reader(BufReader::with_capacity(64 * 1024, &mut limited));
    Seed { builder, section }.deserialize(&mut de)?;
    de.end()?;
    if limited.limit() == 0 {
        return Err(IndexError::Invalid("knowledge base exceeds size limit"));
    }
    Ok(())
}

pub(super) fn read(reader: impl Read) -> Result<GraphIndex, IndexError> {
    let mut builder = Builder::default();
    read_part(reader, &mut builder, Section::Root)?;
    if !builder.has_graph {
        return Err(IndexError::Invalid("call graph is required"));
    }
    builder.finish()
}

pub(super) fn read_pair(kb: impl Read, edges: impl Read) -> Result<GraphIndex, IndexError> {
    let mut builder = Builder::default();
    read_part(kb, &mut builder, Section::Root)?;
    read_part(edges, &mut builder, Section::Graph)?;
    if !builder.has_graph {
        return Err(IndexError::Invalid("call graph is required"));
    }
    builder.finish()
}
