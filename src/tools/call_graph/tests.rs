#![allow(clippy::unwrap_used)]

use super::index::GraphIndex;

pub(super) fn fixture() -> String {
    serde_json::json!({
        "metadata": {"ignored": [1, 2, 3]},
        "structure": {
            "src/a.rs": {"functions": [
                {"id":"a:root", "name":"root", "line_start":1, "line_end":9},
                {"id":"a:leaf", "name":"leaf", "line_start":12, "line_end":14}
            ], "classes":[{"methods":[
                {"id":"a:Type::run", "name":"run", "line_start":20, "line_end":24}
            ]}]},
            "src/b.rs": {"functions":[
                {"id":"b:root", "name":"root", "line_start":3, "line_end":6}
            ]}
        },
        "call_graph": {"nodes":[], "edges":[
            {"from":"a:root", "to":"a:leaf", "edge_type":"calls", "call_site_line":4},
            {"from":"a:Type::run", "to":"a:root", "edge_type":"calls", "call_site_line":22},
            {"from":"b:root", "to":"a:leaf", "edge_type":"calls", "call_site_line":5},
            {"from":"a:root", "to":"external", "edge_type":"calls", "call_site_line":6},
            {"from":"a:leaf", "to":"a:root", "edge_type":"inherits", "call_site_line":12}
        ]}
    })
    .to_string()
}

#[test]
fn streaming_graph_preserves_names_methods_reverse_edges_and_provenance() {
    let graph = GraphIndex::from_reader(fixture().as_bytes()).unwrap();
    assert_eq!(graph.symbol_count(), 4);
    let (hits, truncated) = graph.query("leaf", true, false, 20);
    assert!(!truncated);
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].file, "src/a.rs");
    assert_eq!(hits[0].call_site_line, 4);
    let (hits, _) = graph.query("root", false, false, 20);
    assert_eq!(hits.len(), 2);
    let (hits, _) = graph.query("a:Type::run", false, false, 20);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].name, "root");
    let (hits, _) = graph.query("leaf", true, true, 20);
    assert_eq!(hits.len(), 3);
    assert!(hits.iter().any(|hit| hit.depth == 2 && hit.name == "run"));
    let (_, truncated) = graph.query("leaf", true, true, 1);
    assert!(truncated);
}

#[test]
fn compact_roundtrip_rejects_corruption_and_all_truncated_prefixes() {
    let graph = GraphIndex::from_reader(fixture().as_bytes()).unwrap();
    let encoded = graph.encode().unwrap();
    let decoded = GraphIndex::decode(&encoded).unwrap();
    assert_eq!(decoded.query("leaf", true, true, 20), graph.query("leaf", true, true, 20));
    for end in 0..encoded.len() {
        assert!(GraphIndex::decode(&encoded[..end]).is_err(), "accepted prefix {end}");
    }
    let mut corrupt = encoded;
    corrupt[0] ^= 255;
    assert!(GraphIndex::decode(&corrupt).is_err());
    assert!(GraphIndex::decode(&[255; 128]).is_err());
}

#[test]
fn untrusted_schema_paths_and_limits_are_rejected() {
    for path in
        ["../secret.rs", "/etc/passwd", ".winx/tmp/session/x.rs", "a/../../b.rs", "C:\\secret.rs"]
    {
        let input = fixture().replace("src/a.rs", path);
        assert!(GraphIndex::from_reader(input.as_bytes()).is_err(), "accepted {path}");
    }
    assert!(GraphIndex::from_reader(b"{}".as_slice()).is_err());
    assert!(GraphIndex::from_reader(b"{broken".as_slice()).is_err());
    assert!(GraphIndex::from_reader(format!("{} trailing", fixture()).as_bytes()).is_err());
    let oversized = fixture().replace("a:root", &"x".repeat(16_385));
    assert!(GraphIndex::from_reader(oversized.as_bytes()).is_err());
}

#[test]
fn current_eulix_split_artifacts_and_call_edge_spelling_are_supported() {
    let value: serde_json::Value = serde_json::from_str(&fixture()).unwrap();
    let kb = serde_json::json!({"metadata": {}, "structure":value["structure"]}).to_string();
    let sidecar = value["call_graph"].to_string().replace("\"calls\"", "\"call\"");
    let graph = GraphIndex::from_readers(kb.as_bytes(), sidecar.as_bytes()).unwrap();
    assert_eq!(graph.query("leaf", true, false, 20).0.len(), 2);
}

#[test]
fn repeated_eulix_ids_from_cfg_variants_keep_both_definitions() {
    let mut value: serde_json::Value = serde_json::from_str(&fixture()).unwrap();
    value["structure"]["src/a.rs"]["functions"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"id":"a:leaf","name":"leaf","line_start":30,"line_end":32}));
    let graph = GraphIndex::from_reader(value.to_string().as_bytes()).unwrap();
    let hits = graph.query("a:root", false, false, 20).0;
    assert_eq!(hits.len(), 2);
    assert_eq!((hits[0].line, hits[1].line), (12, 30));
    assert!(GraphIndex::decode(&graph.encode().unwrap()).is_ok());
}

#[test]
fn checked_decoder_rejects_forged_out_of_range_call_site_lines() {
    let mut graph = GraphIndex::from_reader(fixture().as_bytes()).unwrap();
    graph.forward.edges[0].line = 999;
    for edge in &mut graph.reverse.edges {
        if edge.line == 4 {
            edge.line = 999;
        }
    }
    let encoded = graph.encode().unwrap();
    assert!(GraphIndex::decode(&encoded).is_err());
}
