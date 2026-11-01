//! Versioned little-endian encoding; all lengths, references and CSR rows checked.
use sha2::{Digest, Sha256};

use super::index::{
    safe_source_path, Csr, Edge, GraphIndex, IndexError, Symbol, MAX_EDGES, MAX_INDEX_BYTES,
    MAX_STRING, MAX_STRINGS, MAX_STRING_BYTES, MAX_SYMBOLS,
};

const MAGIC: &[u8; 8] = b"WNXCG001";
fn invalid() -> IndexError {
    IndexError::Invalid("corrupt or incompatible call graph index")
}
fn number(bytes: &mut Vec<u8>, n: usize) -> Result<(), IndexError> {
    bytes.extend_from_slice(&u32::try_from(n).map_err(|_| invalid())?.to_le_bytes());
    Ok(())
}
fn string(bytes: &mut Vec<u8>, value: &str) -> Result<(), IndexError> {
    number(bytes, value.len())?;
    bytes.extend_from_slice(value.as_bytes());
    Ok(())
}

pub(super) fn encode(graph: &GraphIndex) -> Result<Vec<u8>, IndexError> {
    let mut bytes = MAGIC.to_vec();
    string(&mut bytes, &graph.snapshot)?;
    string(&mut bytes, &graph.parser_version)?;
    bytes.extend_from_slice(&graph.built_at.to_le_bytes());
    number(&mut bytes, graph.strings.len())?;
    for value in &graph.strings {
        string(&mut bytes, value)?;
    }
    number(&mut bytes, graph.symbols.len())?;
    for symbol in &graph.symbols {
        for value in [symbol.id, symbol.name, symbol.file, symbol.line, symbol.end] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    for csr in [&graph.forward, &graph.reverse] {
        number(&mut bytes, csr.edges.len())?;
        for offset in &csr.offsets {
            bytes.extend_from_slice(&offset.to_le_bytes());
        }
        for edge in &csr.edges {
            bytes.extend_from_slice(&edge.target.to_le_bytes());
            bytes.extend_from_slice(&edge.line.to_le_bytes());
        }
    }
    if bytes.len() + 32 > MAX_INDEX_BYTES {
        return Err(IndexError::Invalid("compact index exceeds size limit"));
    }
    bytes.extend_from_slice(&Sha256::digest(&bytes));
    Ok(bytes)
}

struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], IndexError> {
        let value = self.0.get(..n).ok_or_else(invalid)?;
        self.0 = &self.0[n..];
        Ok(value)
    }
    fn u32(&mut self) -> Result<u32, IndexError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().map_err(|_| invalid())?))
    }
    fn count(&mut self, max: usize, unit: usize) -> Result<usize, IndexError> {
        let n = self.u32()? as usize;
        if n > max || n > self.0.len() / unit {
            return Err(invalid());
        }
        Ok(n)
    }
    fn string(&mut self) -> Result<&'a str, IndexError> {
        let n = self.count(MAX_STRING, 1)?;
        std::str::from_utf8(self.take(n)?).map_err(|_| invalid())
    }
    fn csr(&mut self, symbols: usize) -> Result<Csr, IndexError> {
        let count = self.count(MAX_EDGES, 8)?;
        if (symbols + 1) * 4 + count * 8 > self.0.len() {
            return Err(invalid());
        }
        let offsets: Vec<_> = (0..=symbols).map(|_| self.u32()).collect::<Result<_, _>>()?;
        if offsets.first() != Some(&0)
            || offsets.last().copied().map(|n| n as usize) != Some(count)
            || offsets.windows(2).any(|pair| pair[0] > pair[1])
        {
            return Err(invalid());
        }
        let mut edges = Vec::with_capacity(count);
        for _ in 0..count {
            let edge = Edge { target: self.u32()?, line: self.u32()? };
            if edge.target as usize >= symbols || edge.line == 0 {
                return Err(invalid());
            }
            edges.push(edge);
        }
        let csr = Csr { offsets, edges };
        for id in 0..symbols {
            if csr
                .neighbors(u32::try_from(id).map_err(|_| invalid())?)
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
            {
                return Err(invalid());
            }
        }
        Ok(csr)
    }
}

pub(super) fn decode(bytes: &[u8]) -> Result<GraphIndex, IndexError> {
    if bytes.len() > MAX_INDEX_BYTES || bytes.len() < 40 {
        return Err(invalid());
    }
    let (body, checksum) = bytes.split_at(bytes.len() - 32);
    if Sha256::digest(body).as_slice() != checksum {
        return Err(invalid());
    }
    let mut reader = Reader(body);
    if reader.take(8)? != MAGIC {
        return Err(invalid());
    }
    let mut graph = GraphIndex {
        snapshot: reader.string()?.to_string(),
        parser_version: reader.string()?.to_string(),
        built_at: u64::from_le_bytes(reader.take(8)?.try_into().map_err(|_| invalid())?),
        ..GraphIndex::default()
    };
    let count = reader.count(MAX_STRINGS, 4)?;
    let mut total = 0usize;
    for _ in 0..count {
        let value = reader.string()?;
        total += value.len();
        if total > MAX_STRING_BYTES || graph.lookup.contains_key(value) {
            return Err(invalid());
        }
        graph.intern(value)?;
    }
    let count = reader.count(MAX_SYMBOLS, 20)?;
    for _ in 0..count {
        let symbol = Symbol {
            id: reader.u32()?,
            name: reader.u32()?,
            file: reader.u32()?,
            line: reader.u32()?,
            end: reader.u32()?,
        };
        if [symbol.id, symbol.name, symbol.file]
            .iter()
            .any(|id| *id as usize >= graph.strings.len())
            || symbol.line == 0
            || symbol.end < symbol.line
            || !safe_source_path(&graph.strings[symbol.file as usize])
        {
            return Err(invalid());
        }
        if graph.symbols.last().is_some_and(|last: &Symbol| {
            (last.file, last.line, last.id) >= (symbol.file, symbol.line, symbol.id)
        }) {
            return Err(invalid());
        }
        graph.symbols.push(symbol);
    }
    graph.forward = reader.csr(count)?;
    graph.reverse = reader.csr(count)?;
    if !reader.0.is_empty() {
        return Err(invalid());
    }
    validate_edges(&graph)?;
    graph.rebuild_lookups();
    Ok(graph)
}

fn validate_edges(graph: &GraphIndex) -> Result<(), IndexError> {
    let mut inverse = Vec::with_capacity(graph.forward.edges.len());
    for (id, symbol) in graph.symbols.iter().enumerate() {
        let id = u32::try_from(id).map_err(|_| invalid())?;
        for edge in graph.forward.neighbors(id) {
            if edge.line < symbol.line || edge.line > symbol.end {
                return Err(invalid());
            }
            inverse.push((edge.target, Edge { target: id, line: edge.line }));
        }
    }
    let expected = Csr::build(graph.symbols.len(), inverse);
    if expected.offsets != graph.reverse.offsets || expected.edges != graph.reverse.edges {
        return Err(invalid());
    }
    Ok(())
}
