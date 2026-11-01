//! Reproducible cold conversion, load, and query measurements for local KB artifacts.
use std::io::Write as _;
use std::time::Instant;
use winx_code_agent::tools::call_graph::index::GraphIndex;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let directory = std::path::PathBuf::from(args.next().ok_or("expected artifact directory")?);
    let name = args.next().ok_or("expected query name")?;
    if args.next().as_deref() == Some("--dom") {
        let start = Instant::now();
        let kb: serde_json::Value = serde_json::from_reader(std::io::BufReader::with_capacity(
            64 * 1024,
            std::fs::File::open(directory.join("kb.json"))?,
        ))?;
        let edges: serde_json::Value = serde_json::from_reader(std::io::BufReader::with_capacity(
            64 * 1024,
            std::fs::File::open(directory.join("kb_call_graph.json"))?,
        ))?;
        std::hint::black_box((&kb, &edges));
        let result = serde_json::json!({"dom_read_ms":start.elapsed().as_secs_f64()*1000.0});
        serde_json::to_writer(std::io::stdout().lock(), &result)?;
        return Ok(());
    }
    let start = Instant::now();
    let graph = GraphIndex::from_readers(
        std::fs::File::open(directory.join("kb.json"))?,
        std::fs::File::open(directory.join("kb_call_graph.json"))?,
    )?;
    let convert = start.elapsed();
    let start = Instant::now();
    let bytes = graph.encode()?;
    let encode = start.elapsed();
    std::fs::write(directory.join("index.wcg"), &bytes)?;
    let mut loads = Vec::new();
    for _ in 0..100 {
        let start = Instant::now();
        let bytes = std::fs::read(directory.join("index.wcg"))?;
        std::hint::black_box(GraphIndex::decode(&bytes)?);
        loads.push(start.elapsed().as_nanos());
    }
    let mut queries = Vec::new();
    for _ in 0..10_000 {
        let start = Instant::now();
        std::hint::black_box(graph.query(&name, true, false, 100));
        queries.push(start.elapsed().as_nanos());
    }
    loads.sort_unstable();
    queries.sort_unstable();
    let result = serde_json::json!({"convert_ms":convert.as_secs_f64()*1000.0,"encode_ms":encode.as_secs_f64()*1000.0,
        "load_p50_ns":loads[50],"load_p95_ns":loads[95],"query_p50_ns":queries[5000],"query_p95_ns":queries[9500],
        "symbols":graph.symbol_count(),"edges":graph.edge_count(),"compact_bytes":bytes.len(),"resident_estimate_bytes":graph.memory_bytes(),
        "query_name":name,"query_hits":graph.query(&name,true,false,100).0.len()});
    let mut output = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut output, &result)?;
    writeln!(output)?;
    Ok(())
}
