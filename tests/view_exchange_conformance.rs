//! Conformance against the `nostdb-spec` viewer exchange fixtures.
//!
//! Twenty-one containers, and this reads all of them — including the payload rules the
//! specification harness leaves to a decoder, which is what makes the two suites complementary
//! rather than duplicated.
//!
//! It also reads a container this build *wrote*, through the same reader. A writer whose own reader
//! refuses its output would be two implementations of one contract, and the file is the one a
//! browser fetches.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 8] = b"NOSTVIEW";
const HEADER_BYTES: usize = 32;
const ENTRY_BYTES: usize = 16;

const MAX_FILE_BYTES: usize = 512 * 1024 * 1024;
const MAX_SECTIONS: usize = 16;
const MAX_NODES: u32 = 4_194_304;
const MAX_EDGES: u32 = 33_554_432;
const MAX_SOURCES: u32 = 65_536;
const MAX_STRINGS: u32 = 4_194_304;
const MAX_STRING_BYTES: u32 = 65_536;

fn fixture_root() -> Option<PathBuf> {
    let raw = std::env::var("NOSTDB_SPEC_FIXTURES").ok()?;
    let directory = PathBuf::from(raw).join("view-exchange").join("container");
    directory.is_dir().then_some(directory)
}

fn expectations(path: &Path) -> BTreeMap<String, String> {
    let text = std::fs::read_to_string(path.with_extension("expected")).unwrap_or_else(|error| {
        panic!(
            "cannot read the expectation for {}: {error}",
            path.display()
        )
    });
    text.lines()
        .filter_map(|line| line.split_once(" = "))
        .map(|(key, value)| (key.trim().to_owned(), value.trim().to_owned()))
        .collect()
}

fn containers(directory: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", directory.display()))
        .map(|entry| entry.expect("directory entry").path())
        .filter(|path| path.extension().and_then(|s| s.to_str()) == Some("bin"))
        .collect();
    paths.sort();
    paths
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// What a fully decoded container holds.
#[derive(Debug)]
struct Decoded {
    nodes: u32,
    edges: u32,
    sources: u32,
    evidence: u32,
}

/// Decodes a container, applying every rule the contract states.
///
/// Bounds are checked before allocating on any count read from the file, which is the whole point of
/// section 6: a hostile or truncated file becomes a refusal rather than an allocation.
fn decode(bytes: &[u8]) -> Result<Decoded, String> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err("over the file bound".to_owned());
    }
    if bytes.len() < HEADER_BYTES {
        return Err("shorter than its own header".to_owned());
    }
    if &bytes[0..8] != MAGIC {
        return Err("the magic is not NOSTVIEW".to_owned());
    }
    let version = u16_at(bytes, 8);
    if version != 1 {
        return Err(format!("view_exchange_version {version}"));
    }
    if nostdb_core::crc::crc32c(&bytes[0..24]) != u32_at(bytes, 24) {
        return Err("the header checksum does not match".to_owned());
    }
    if u32_at(bytes, 28) != 0 {
        return Err("the reserved field is not zero".to_owned());
    }

    let nodes = u32_at(bytes, 12);
    let edges = u32_at(bytes, 16);
    let sources = u32_at(bytes, 20);
    if nodes > MAX_NODES || edges > MAX_EDGES || sources > MAX_SOURCES {
        return Err("a count is over its bound".to_owned());
    }
    if sources == 0 {
        return Err("every node names a source, so there is always at least the root".to_owned());
    }

    let count = usize::from(u16_at(bytes, 10));
    if count > MAX_SECTIONS {
        return Err(format!("{count} sections, over the bound"));
    }
    let table_end = HEADER_BYTES + count * ENTRY_BYTES;
    if bytes.len() < table_end {
        return Err("the section table runs past the end".to_owned());
    }

    let mut found: BTreeMap<u16, (usize, usize)> = BTreeMap::new();
    let mut spans: Vec<(usize, usize, u16)> = Vec::new();
    for index in 0..count {
        let at = HEADER_BYTES + index * ENTRY_BYTES;
        let kind = u16_at(bytes, at);
        if u16_at(bytes, at + 2) != 0 {
            return Err("an entry's reserved field is not zero".to_owned());
        }
        if !(1..=8).contains(&kind) {
            return Err(format!("section kind {kind} is not one version 1 defines"));
        }
        let offset = u32_at(bytes, at + 4) as usize;
        let length = u32_at(bytes, at + 8) as usize;
        let end = offset
            .checked_add(length)
            .ok_or("a section length overflows")?;
        if offset < table_end || end > bytes.len() {
            return Err(format!("section {kind} lies outside the file"));
        }
        if nostdb_core::crc::crc32c(&bytes[offset..end]) != u32_at(bytes, at + 12) {
            return Err(format!("section {kind} does not match its checksum"));
        }
        if found.insert(kind, (offset, length)).is_some() {
            return Err(format!("section kind {kind} appears twice"));
        }
        spans.push((offset, end, kind));
    }

    spans.sort_by_key(|(offset, _, _)| *offset);
    for pair in spans.windows(2) {
        if pair[0].1 > pair[1].0 {
            return Err(format!("sections {} and {} overlap", pair[0].2, pair[1].2));
        }
    }

    for (kind, name) in [
        (1, "strings"),
        (2, "node_ids"),
        (3, "node_labels"),
        (4, "node_sources"),
        (5, "edge_endpoints"),
        (6, "edge_relations"),
        (7, "sources"),
    ] {
        if !found.contains_key(&kind) {
            return Err(format!("the required section {name} is absent"));
        }
    }

    // Section 3.1: a length that disagrees with the header's counts is a disagreement.
    for (kind, stride, expected) in [
        (2u16, 4usize, nodes),
        (3, 4, nodes),
        (4, 4, nodes),
        (5, 8, edges),
        (6, 4, edges),
        (7, 12, sources),
    ] {
        let (_, length) = found[&kind];
        if length != stride * expected as usize {
            return Err(format!(
                "section {kind} is {length} bytes for {expected} entries"
            ));
        }
    }

    // The string table, which every other section indexes into.
    let (strings_at, strings_length) = found[&1];
    if strings_length < 4 {
        return Err("the string table states no count".to_owned());
    }
    let string_count = u32_at(bytes, strings_at);
    if string_count == 0 {
        return Err("index 0 is the empty string, so a table always has one".to_owned());
    }
    if string_count > MAX_STRINGS {
        return Err("more strings than the bound".to_owned());
    }
    let mut cursor = strings_at + 4;
    let strings_end = strings_at + strings_length;
    for index in 0..string_count {
        if cursor + 4 > strings_end {
            return Err("the string table ends inside a length".to_owned());
        }
        let length = u32_at(bytes, cursor);
        if length > MAX_STRING_BYTES {
            return Err("a string is longer than the bound".to_owned());
        }
        cursor += 4;
        let end = cursor + length as usize;
        if end > strings_end {
            return Err("a string runs past the table".to_owned());
        }
        if index == 0 && length != 0 {
            return Err("index 0 must be the empty string".to_owned());
        }
        std::str::from_utf8(&bytes[cursor..end]).map_err(|_| "a string is not UTF-8")?;
        cursor = end;
    }
    if cursor != strings_end {
        return Err("the string table has bytes after its last string".to_owned());
    }

    let in_range = |index: u32| index < string_count;
    for kind in [2u16, 3, 6] {
        let (at, length) = found[&kind];
        for offset in (at..at + length).step_by(4) {
            if !in_range(u32_at(bytes, offset)) {
                return Err(format!("section {kind} holds a string index out of range"));
            }
        }
    }

    // Every node names a source, and every endpoint names a node this container carries.
    let (sources_column_at, sources_column_length) = found[&4];
    for offset in (sources_column_at..sources_column_at + sources_column_length).step_by(4) {
        if u32_at(bytes, offset) >= sources {
            return Err("a node names a source index out of range".to_owned());
        }
    }
    let (endpoints_at, endpoints_length) = found[&5];
    for offset in (endpoints_at..endpoints_at + endpoints_length).step_by(4) {
        if u32_at(bytes, offset) >= nodes {
            return Err("an edge endpoint is out of range".to_owned());
        }
    }

    // Source 0 is the root by definition.
    let (source_table_at, _) = found[&7];
    if bytes[source_table_at + 8] != 0 {
        return Err("source 0 must be the root".to_owned());
    }
    for index in 0..sources as usize {
        let at = source_table_at + index * 12;
        if !in_range(u32_at(bytes, at)) || !in_range(u32_at(bytes, at + 4)) {
            return Err("a source names a string index out of range".to_owned());
        }
        if bytes[at + 8] > 2 || bytes[at + 9] > 1 {
            return Err("a source states an unknown kind or status".to_owned());
        }
        if u16_at(bytes, at + 10) != 0 {
            return Err("a source entry's reserved field is not zero".to_owned());
        }
    }

    // Evidence: optional, sparse, and ascending by node index so a viewer may binary search it.
    let mut evidence = 0u32;
    if let Some(&(at, length)) = found.get(&8) {
        if length < 4 {
            return Err("the evidence section states no count".to_owned());
        }
        evidence = u32_at(bytes, at);
        if length != 4 + evidence as usize * 16 {
            return Err("the evidence section's length disagrees with its count".to_owned());
        }
        let mut previous: Option<u32> = None;
        for index in 0..evidence as usize {
            let entry = at + 4 + index * 16;
            let node = u32_at(bytes, entry);
            if node >= nodes {
                return Err("evidence names a node out of range".to_owned());
            }
            if let Some(previous) = previous {
                if node <= previous {
                    return Err("evidence is not ascending by node index".to_owned());
                }
            }
            previous = Some(node);
            if !in_range(u32_at(bytes, entry + 4)) {
                return Err("evidence names a path string out of range".to_owned());
            }
        }
    }

    Ok(Decoded {
        nodes,
        edges,
        sources,
        evidence,
    })
}

#[test]
fn every_accepted_container_decodes_with_its_declared_counts() {
    let Some(root) = fixture_root() else {
        println!("view exchange conformance: skipped, NOSTDB_SPEC_FIXTURES is unset");
        return;
    };
    let paths = containers(&root.join("valid"));
    assert!(!paths.is_empty(), "no accepted containers were found");

    for path in &paths {
        let bytes = std::fs::read(path).expect("fixture is readable");
        let decoded = decode(&bytes).unwrap_or_else(|reason| {
            panic!(
                "{} is accepted by the specification and refused here: {reason}",
                path.display()
            )
        });
        let expected = expectations(path);
        for (key, found) in [
            ("nodes", decoded.nodes),
            ("edges", decoded.edges),
            ("sources", decoded.sources),
            ("evidence", decoded.evidence),
        ] {
            let declared: u32 = expected
                .get(key)
                .unwrap_or_else(|| panic!("{} declares no {key}", path.display()))
                .parse()
                .expect("a number");
            assert_eq!(declared, found, "{}: {key}", path.display());
        }
    }
    println!(
        "view exchange conformance: {} accepted containers verified",
        paths.len()
    );
}

#[test]
fn every_rejected_container_is_refused() {
    let Some(root) = fixture_root() else {
        println!("view exchange conformance: skipped, NOSTDB_SPEC_FIXTURES is unset");
        return;
    };
    let paths = containers(&root.join("invalid"));
    assert!(!paths.is_empty(), "no rejected containers were found");

    // Every one, including the payload rules the specification harness cannot reach without a
    // decoder. This is the half of the published suite that only an implementation can run.
    let mut refused: BTreeSet<String> = BTreeSet::new();
    for path in &paths {
        let bytes = std::fs::read(path).expect("fixture is readable");
        let stem = path.file_stem().and_then(|s| s.to_str()).expect("a name");
        let reason = decode(&bytes).err().unwrap_or_else(|| {
            panic!(
                "{} is rejected by the specification and decoded here",
                path.display()
            )
        });
        let declared = expectations(path)
            .get("code")
            .cloned()
            .unwrap_or_else(|| panic!("{} declares no code", path.display()));
        assert_eq!(
            declared,
            "VIEW_EXCHANGE_INVALID",
            "{}: {reason}",
            path.display()
        );
        refused.insert(stem.to_owned());
    }

    // The four payload rules exist to be checked here rather than in the specification harness,
    // which stops at the header. Naming them makes the split visible instead of implied.
    for payload_rule in [
        "string_index_out_of_range",
        "edge_endpoint_out_of_range",
        "source_zero_is_not_the_root",
        "evidence_is_out_of_order",
    ] {
        assert!(
            refused.contains(payload_rule),
            "{payload_rule} has no fixture, so the decoder-only half of the suite is incomplete"
        );
    }
    println!(
        "view exchange conformance: {} rejected containers verified",
        paths.len()
    );
}

#[test]
fn a_container_this_build_writes_is_one_this_reader_accepts() {
    // A writer whose own reader refuses its output would be two implementations of one contract,
    // and this file is the one a browser fetches.
    let federation = nostdb_core::federation::Federation {
        sources: vec![nostdb_core::federation::FederatedSource {
            locator: None,
            path: PathBuf::from("/project/.nostdb/root.nostdb"),
            depth: 0,
            graph: nostdb_core::encoding::Graph::default(),
        }],
        statuses: Vec::new(),
    };
    let written =
        nostdb_cli::view::write_exchange(&federation, "/project").expect("the writer produces one");
    let decoded = decode(&written.bytes).expect("and this reader accepts it");
    assert_eq!(decoded.nodes, 0);
    assert_eq!(decoded.edges, 0);
    assert_eq!(decoded.sources, 1, "the root is always a source");
    assert_eq!(decoded.evidence, 0, "and evidence is absent, not empty");
}
