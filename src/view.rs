//! `nostdb view`, and the viewer exchange it writes.
//!
//! # Why this format and not the graph JSON
//!
//! [`crate::plugin_run`] hands a plugin a JSON graph, which exists so the handoff can be exercised.
//! A viewer needs something else: `docs/PRD.md` section 24.3 requires instanced rendering and
//! incremental decoding, and a renderer uploads a buffer per attribute. So this is columnar, and an
//! edge names its endpoints by *index* rather than by opaque identifier — an identifier would make a
//! renderer build a hash map over every node before drawing a single line.
//!
//! The identifier is still carried, in its own section, because source navigation needs it. What
//! changes is that drawing never has to resolve one.
//!
//! # This is the first action that needs a plugin the user did not name
//!
//! Every plugin command so far took a name. `view` wants a viewer, whichever one is installed, which
//! is the flow `docs/PRD.md` section 23.4 describes and the point at which `PLUGIN_REQUIRED` carries
//! a recommended source rather than only a name.

use crate::exit::ExitClass;
use crate::plugin_install::{Scope, read_record};
use crate::plugin_run::{ProtocolCode, converse, preflight};
use nostdb_core::crc::crc32c;
use nostdb_core::sync::digest_bytes;
use std::io::Write;
use std::path::{Path, PathBuf};

/// The media type a viewer receives.
pub const VIEW_MEDIA_TYPE: &str = "application/vnd.nostdb.view+bin";

/// The container version this build writes.
pub const VIEW_EXCHANGE_VERSION: u16 = 1;

/// The action a viewer plugin implements.
pub const VIEW_ACTION: &str = "view";

/// The plugin the product contract's own example publishes, offered when none is installed.
///
/// A name and a source, because section 23.4 requires a refusal to identify the exact recommended
/// plugin and its pinned source. A refusal naming neither would leave a user to search for one.
pub const RECOMMENDED: (&str, &str) = (
    "org.nostdb.view-webgpu",
    "https://github.com/nostdb/plugins?ref=main#reference/view-webgpu",
);

/// The magic every exchange container opens with.
pub const MAGIC: &[u8; 8] = b"NOSTVIEW";

const HEADER_BYTES: usize = 32;
const ENTRY_BYTES: usize = 16;

/// A section kind, as the contract numbers them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Strings = 1,
    NodeIds = 2,
    NodeLabels = 3,
    NodeSources = 4,
    EdgeEndpoints = 5,
    EdgeRelations = 6,
    Sources = 7,
    Evidence = 8,
}

/// Interns strings and hands back the index the container stores.
///
/// Index 0 is the empty string, emitted whether or not anything uses it, so a reader has an index
/// meaning "nothing stated" without a sentinel that could be mistaken for a real one.
#[derive(Debug, Default)]
struct Strings {
    values: Vec<String>,
    seen: std::collections::HashMap<String, u32>,
}

impl Strings {
    fn new() -> Self {
        let mut table = Self::default();
        table.intern("");
        table
    }

    fn intern(&mut self, text: &str) -> u32 {
        if let Some(index) = self.seen.get(text) {
            return *index;
        }
        let index = u32::try_from(self.values.len()).unwrap_or(u32::MAX);
        self.values.push(text.to_owned());
        self.seen.insert(text.to_owned(), index);
        index
    }

    fn payload(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(
            &u32::try_from(self.values.len())
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        for value in &self.values {
            bytes.extend_from_slice(&u32::try_from(value.len()).unwrap_or(u32::MAX).to_le_bytes());
            bytes.extend_from_slice(value.as_bytes());
        }
        bytes
    }
}

/// One source a viewer can attribute an item to.
#[derive(Clone, Debug)]
pub struct Source {
    /// The canonical locator.
    pub locator: String,
    /// The alias the link declared, when it declared one.
    pub alias: Option<String>,
    /// 0 root, 1 local link, 2 remote link.
    pub kind: u8,
    /// 0 available, 1 unavailable.
    pub status: u8,
}

/// What was written, and where.
#[derive(Clone, Debug)]
pub struct Written {
    /// The container's bytes.
    pub bytes: Vec<u8>,
    /// Its digest, which travels in the handoff.
    pub digest: String,
}

/// Renders a federation as the viewer exchange container.
///
/// Every source's records are carried, and every node names the source it came from. That is the
/// scoped source identity `docs/PRD.md` section 24.2 requires, and it has to be built here rather
/// than inferred later: once the columns are flat, nothing else knows which graph a row came from.
///
/// # Errors
///
/// Returns a reason when the graph is larger than the container's fixed bounds can address, which is
/// a refusal rather than a truncation: a viewer shown a truncated graph would report on a graph
/// nobody has.
pub fn write_exchange(
    federation: &nostdb_core::federation::Federation,
    root_locator: &str,
) -> Result<Written, String> {
    let mut strings = Strings::new();

    // The root is index 0 by definition. Then one entry per source that contributed records, then
    // one per link that could not be reached — which keeps its entry, because the product contract
    // requires an unavailable source to stay declared.
    let mut sources = vec![Source {
        locator: root_locator.to_owned(),
        alias: None,
        kind: 0,
        status: 0,
    }];
    for source in federation.sources.iter().skip(1) {
        let locator = source
            .locator
            .as_ref()
            .map(|locator| locator.as_str().to_owned())
            .unwrap_or_default();
        let alias = federation
            .statuses
            .iter()
            .find(|status| status.locator.as_str() == locator)
            .and_then(|status| status.alias.clone());
        sources.push(Source {
            kind: u8::from(locator.starts_with("github://")) + 1,
            alias,
            status: 0,
            locator,
        });
    }
    for status in &federation.statuses {
        if status.is_available() {
            continue;
        }
        sources.push(Source {
            kind: u8::from(status.locator.as_str().starts_with("github://")) + 1,
            alias: status.alias.clone(),
            status: 1,
            locator: status.locator.as_str().to_owned(),
        });
    }

    // Endpoints are indices, so every node's position has to be known before any edge is written.
    // Positions are keyed by source *and* identifier: a record is identified across databases by
    // that pair, and keying on the identifier alone would let two sources' nodes collide.
    let mut position: std::collections::HashMap<(usize, String), u32> =
        std::collections::HashMap::new();
    let mut node_ids: Vec<u8> = Vec::new();
    let mut node_labels: Vec<u8> = Vec::new();
    let mut node_sources: Vec<u8> = Vec::new();
    let mut node_count = 0u32;

    for (source_index, source) in federation.sources.iter().enumerate() {
        let column = u32::try_from(source_index).map_err(|_| "more sources than fit")?;
        for node in &source.graph.nodes {
            let id = node.id.to_string();
            position.insert((source_index, id.clone()), node_count);
            node_ids.extend_from_slice(&strings.intern(&id).to_le_bytes());
            let label = node
                .labels
                .first()
                .map(ToString::to_string)
                .unwrap_or_default();
            node_labels.extend_from_slice(&strings.intern(&label).to_le_bytes());
            node_sources.extend_from_slice(&column.to_le_bytes());
            node_count = node_count
                .checked_add(1)
                .ok_or("more nodes than the container addresses")?;
        }
    }

    let mut endpoints: Vec<u8> = Vec::new();
    let mut relations: Vec<u8> = Vec::new();
    let mut edge_count = 0u32;

    for (source_index, source) in federation.sources.iter().enumerate() {
        for edge in &source.graph.edges {
            // An endpoint this container cannot address names a record no source sent. Dropped
            // rather than written as an index that would read out of a renderer's buffer.
            let Some(from) = endpoint_index(&edge.source, source_index, &position) else {
                continue;
            };
            let Some(to) = endpoint_index(&edge.target, source_index, &position) else {
                continue;
            };
            endpoints.extend_from_slice(&from.to_le_bytes());
            endpoints.extend_from_slice(&to.to_le_bytes());
            relations.extend_from_slice(&strings.intern(&edge.relation.to_string()).to_le_bytes());
            edge_count = edge_count
                .checked_add(1)
                .ok_or("more edges than the container addresses")?;
        }
    }

    let mut source_payload = Vec::with_capacity(sources.len() * 12);
    for source in &sources {
        let locator = strings.intern(&source.locator);
        let alias = source
            .alias
            .as_deref()
            .map_or(0, |alias| strings.intern(alias));
        source_payload.extend_from_slice(&locator.to_le_bytes());
        source_payload.extend_from_slice(&alias.to_le_bytes());
        source_payload.push(source.kind);
        source_payload.push(source.status);
        source_payload.extend_from_slice(&0u16.to_le_bytes());
    }

    // Sparse, and ordered by node index, so a viewer may binary search it on a click. Most nodes
    // in a large graph carry none, and a dense column would spend four bytes per node saying so.
    let mut evidence: Vec<u8> = Vec::new();
    let mut evidence_count = 0u32;
    for (source_index, source) in federation.sources.iter().enumerate() {
        for node in &source.graph.nodes {
            let Some(index) = position.get(&(source_index, node.id.to_string())) else {
                continue;
            };
            // The first evidence that names a path and a position. A node may carry several, from
            // several contributors; navigation needs one place to open, and offering a list would
            // make a viewer choose without anything to choose on.
            let found = node
                .contributions
                .iter()
                .flat_map(|contribution| contribution.evidence.iter())
                .find_map(|evidence| {
                    let path = evidence.path.as_ref()?;
                    Some((path.as_str().to_owned(), evidence.range))
                });
            let Some((path, range)) = found else { continue };
            let start = range.map(|range| range.start());
            evidence.extend_from_slice(&index.to_le_bytes());
            evidence.extend_from_slice(&strings.intern(&path).to_le_bytes());
            evidence.extend_from_slice(&start.map_or(0, |start| start.line).to_le_bytes());
            evidence.extend_from_slice(&start.map_or(0, |start| start.column).to_le_bytes());
            evidence_count += 1;
        }
    }

    // The string table is written last, because interning happens while the other sections are
    // built. Its *section* still comes first in the table, which is the order a reader wants.
    let mut sections: Vec<(Kind, Vec<u8>)> = vec![
        (Kind::Strings, strings.payload()),
        (Kind::NodeIds, node_ids),
        (Kind::NodeLabels, node_labels),
        (Kind::NodeSources, node_sources),
        (Kind::EdgeEndpoints, endpoints),
        (Kind::EdgeRelations, relations),
        (Kind::Sources, source_payload),
    ];
    if evidence_count > 0 {
        // Absent rather than empty when nothing carries evidence. An empty section and an absent
        // one are different statements, and the contract keeps them different.
        let mut payload = evidence_count.to_le_bytes().to_vec();
        payload.extend_from_slice(&evidence);
        sections.push((Kind::Evidence, payload));
    }

    let source_count =
        u32::try_from(sources.len()).map_err(|_| "more sources than the container addresses")?;
    let bytes = assemble(node_count, edge_count, source_count, &sections)?;
    Ok(Written {
        digest: digest_bytes(&bytes).to_string(),
        bytes,
    })
}

/// The index a node reference resolves to, or none when this container does not carry it.
fn endpoint_index(
    reference: &nostdb_core::graph::NodeReference,
    within: usize,
    position: &std::collections::HashMap<(usize, String), u32>,
) -> Option<u32> {
    use nostdb_core::graph::NodeReference;
    match reference {
        // Local means local to the source that declared the edge, not to the root. Which is the
        // whole reason the map is keyed by the pair.
        NodeReference::Local(id) => position.get(&(within, id.to_string())).copied(),
        NodeReference::External(scoped) => {
            let local = scoped.local.to_string();
            position
                .iter()
                .find(|((_, id), _)| *id == local)
                .map(|(_, index)| *index)
        }
    }
}

/// Lays out the header, the section table, and the payloads.
fn assemble(
    nodes: u32,
    edges: u32,
    sources: u32,
    sections: &[(Kind, Vec<u8>)],
) -> Result<Vec<u8>, String> {
    let count = u16::try_from(sections.len()).map_err(|_| "too many sections")?;

    let mut header = vec![0u8; HEADER_BYTES];
    header[0..8].copy_from_slice(MAGIC);
    header[8..10].copy_from_slice(&VIEW_EXCHANGE_VERSION.to_le_bytes());
    header[10..12].copy_from_slice(&count.to_le_bytes());
    header[12..16].copy_from_slice(&nodes.to_le_bytes());
    header[16..20].copy_from_slice(&edges.to_le_bytes());
    header[20..24].copy_from_slice(&sources.to_le_bytes());
    let checksum = crc32c(&header[0..24]);
    header[24..28].copy_from_slice(&checksum.to_le_bytes());
    // Bytes 28..32 stay zero. A reserved field is where a later version puts something, and a
    // writer setting it now would make that version unable to tell an old file from a new one.

    let mut table = Vec::with_capacity(sections.len() * ENTRY_BYTES);
    let mut offset = u32::try_from(HEADER_BYTES + sections.len() * ENTRY_BYTES)
        .map_err(|_| "the section table does not fit")?;
    for (kind, payload) in sections {
        let length = u32::try_from(payload.len()).map_err(|_| "a section does not fit")?;
        table.extend_from_slice(&(*kind as u16).to_le_bytes());
        table.extend_from_slice(&0u16.to_le_bytes());
        table.extend_from_slice(&offset.to_le_bytes());
        table.extend_from_slice(&length.to_le_bytes());
        table.extend_from_slice(&crc32c(payload).to_le_bytes());
        offset = offset
            .checked_add(length)
            .ok_or("the container does not fit in its own offsets")?;
    }

    let mut bytes = header;
    bytes.extend_from_slice(&table);
    for (_, payload) in sections {
        bytes.extend_from_slice(payload);
    }
    Ok(bytes)
}

/// Runs `nostdb view`.
pub fn run(from: &Path, standalone: bool, out: &mut dyn Write, err: &mut dyn Write) -> ExitClass {
    let engine = match crate::plugin_install::engine_version() {
        Ok(version) => version,
        Err(reason) => {
            let _ = writeln!(err, "this build reports no readable version: {reason}");
            return ExitClass::Internal;
        }
    };

    let project = match nostdb_core::project::Project::discover(from, None) {
        Ok(project) => project,
        Err(error) => {
            let _ = writeln!(err, "{error}");
            return ExitClass::for_project_error(&error);
        }
    };
    let root = project.root().to_owned();

    // A viewer, whichever one is installed. Project scope first, for the reason it is preferred
    // everywhere else: a project that pinned something did so on purpose.
    let mut viewer = None;
    for scope in [Scope::Project, Scope::Global] {
        let scope_root = match scope {
            Scope::Project => Some(root.clone()),
            Scope::Global => std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from),
        };
        let Some(scope_root) = scope_root else {
            continue;
        };
        let record = match read_record(&scope_root, scope) {
            Ok(record) => record,
            Err(error) => {
                let _ = writeln!(err, "{}: {}", error.code, error.reason);
                return error.code.exit_class();
            }
        };
        if let Some(found) = record
            .installed()
            .iter()
            .find(|entry| declares_view(entry))
            .map(|entry| entry.name.clone())
        {
            viewer = Some((scope, scope_root, record, found));
            break;
        }
    }

    let Some((scope, scope_root, record, name)) = viewer else {
        // The full section 23.4 refusal: the exact recommended plugin, its pinned source, and the
        // command. Nothing is installed without being asked, and nothing is asked here because this
        // is the non-interactive branch — a caller gets the code and the command.
        let (recommended, source) = RECOMMENDED;
        let _ = writeln!(
            err,
            "{}: no installed plugin implements `{VIEW_ACTION}`",
            ProtocolCode::Required
        );
        let _ = writeln!(err, "recommended: {recommended}");
        let _ = writeln!(err, "  nostdb plugin add '{source}'");
        let _ = writeln!(err, "  nostdb view {}", from.display());
        return ProtocolCode::Required.exit_class();
    };

    let ready = match preflight(&record, scope, &scope_root, &name, VIEW_ACTION, &engine) {
        Ok(ready) => ready,
        Err(error) => {
            let _ = writeln!(err, "{error}");
            return error.class;
        }
    };

    // Resolved rather than read: section 24.2 requires the root graph *and* recursively reachable
    // linked graphs, with a marker for each link that could not be reached.
    let federation = match project.resolve_links() {
        Ok(federation) => federation,
        Err(error) => {
            let _ = writeln!(err, "{error}");
            return ExitClass::for_project_error(&error);
        }
    };
    let nodes: usize = federation.sources.iter().map(|s| s.graph.nodes.len()).sum();
    let edges: usize = federation.sources.iter().map(|s| s.graph.edges.len()).sum();
    // Reported before anything is rendered, and on stderr, so a viewer's output stays pipeable
    // while an unreachable link is still said out loud rather than absorbed into a smaller graph.
    for warning in federation.warnings() {
        let _ = writeln!(
            err,
            "warning: {}: {}",
            warning.code.as_str(),
            warning.message.as_str()
        );
    }

    let written = match write_exchange(&federation, &root.display().to_string()) {
        Ok(written) => written,
        Err(reason) => {
            let _ = writeln!(err, "the exchange could not be written: {reason}");
            return ExitClass::Io;
        }
    };

    let mut directory = std::env::temp_dir();
    directory.push(format!("nostdb-view-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    if let Err(error) = std::fs::create_dir_all(&directory) {
        let _ = writeln!(err, "{}: {error}", directory.display());
        return ExitClass::Io;
    }
    let artifact = directory.join("view.data.bin");
    if let Err(error) = std::fs::write(&artifact, &written.bytes) {
        let _ = writeln!(err, "{}: {error}", artifact.display());
        let _ = std::fs::remove_dir_all(&directory);
        return ExitClass::Io;
    }

    let output_directory = root.join(".nostdb").join("out");
    if let Err(error) = std::fs::create_dir_all(&output_directory) {
        let _ = writeln!(err, "{}: {error}", output_directory.display());
        let _ = std::fs::remove_dir_all(&directory);
        return ExitClass::Io;
    }

    let exchange = crate::plugin_run::Exchange {
        media_type: VIEW_MEDIA_TYPE.to_owned(),
        path: artifact,
        bytes: written.bytes.len() as u64,
        content_digest: written.digest,
    };

    let program = ready.directory.join(&ready.command[0]);
    let arguments: Vec<&str> = ready.command[1..].iter().map(String::as_str).collect();
    let process = match nostdb_core::provider_process::ProviderProcess::start(&program, &arguments)
    {
        Ok(process) => process,
        Err(reason) => {
            let _ = writeln!(err, "{}: {reason}", ProtocolCode::Failed);
            let _ = std::fs::remove_dir_all(&directory);
            return ProtocolCode::Failed.exit_class();
        }
    };
    let mut transport = process;
    let outcome = converse(
        &mut transport,
        &ready,
        VIEW_ACTION,
        &output_directory,
        Some(&exchange),
    );
    // Removed whether the invocation succeeded or not: an artifact left behind is authorized graph
    // data sitting in a temporary directory after the authorization ended.
    let _ = std::fs::remove_dir_all(&directory);

    match outcome {
        Ok((handshake, invoked)) => {
            let _ = writeln!(
                out,
                "{} {} rendered {} nodes and {} edges: {}",
                handshake.plugin, handshake.plugin_version, nodes, edges, invoked.status
            );
            for output in &invoked.outputs {
                let _ = writeln!(out, "    {}", output_directory.join(output).display());
            }
            if standalone
                && !invoked
                    .outputs
                    .iter()
                    .any(|output| output.ends_with(".html"))
            {
                // `--standalone` asks for one file. A viewer that wrote none cannot have honoured
                // it, and reporting success would leave a user looking for a file nobody wrote.
                let _ = writeln!(
                    err,
                    "{}: --standalone was asked for and no HTML was written",
                    ProtocolCode::Failed
                );
                return ProtocolCode::Failed.exit_class();
            }
            if invoked.is_complete() {
                ExitClass::Success
            } else {
                let _ = writeln!(
                    err,
                    "{}: {} reported partial work",
                    ProtocolCode::Failed,
                    handshake.plugin
                );
                ProtocolCode::Failed.exit_class()
            }
        }
        Err(error) => {
            let _ = writeln!(err, "{error}");
            error.class
        }
    }
}

/// Reports whether an installation's approved permissions leave a viewer able to work.
///
/// The record does not carry the declared actions, and reading the manifest is not permitted before
/// its digest holds — so a candidate is one that could be a viewer, and `preflight` is what settles
/// whether it declares the action. Guessing from the name would let a plugin called
/// `org.example.view` be tried and a plugin called anything else be missed.
fn declares_view(entry: &crate::plugin_install::Installation) -> bool {
    entry.approved_permissions["graph_read"]
        .as_bool()
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostdb_core::encoding::Graph;

    /// A federation holding only an empty root, which is what a configured project starts as.
    fn empty_federation() -> nostdb_core::federation::Federation {
        nostdb_core::federation::Federation {
            sources: vec![nostdb_core::federation::FederatedSource {
                locator: None,
                path: std::path::PathBuf::from("/project/.nostdb/root.nostdb"),
                depth: 0,
                graph: Graph::default(),
            }],
            statuses: Vec::new(),
        }
    }

    fn u16_at(bytes: &[u8], at: usize) -> u16 {
        u16::from_le_bytes([bytes[at], bytes[at + 1]])
    }

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
    }

    #[test]
    fn an_empty_graph_is_a_readable_container() {
        // Empty is a graph, not an error. A viewer that refused one would refuse every new project.
        let written = write_exchange(&empty_federation(), "/project").expect("written");
        let bytes = &written.bytes;
        assert_eq!(&bytes[0..8], MAGIC);
        assert_eq!(u16_at(bytes, 8), VIEW_EXCHANGE_VERSION);
        assert_eq!(u16_at(bytes, 10), 7, "seven required sections");
        assert_eq!(u32_at(bytes, 12), 0);
        assert_eq!(u32_at(bytes, 16), 0);
        assert_eq!(u32_at(bytes, 20), 1, "the root is always a source");
        assert_eq!(crc32c(&bytes[0..24]), u32_at(bytes, 24));
        assert_eq!(u32_at(bytes, 28), 0, "reserved stays zero");
        assert!(written.digest.starts_with("sha256:"));
    }

    #[test]
    fn every_section_is_in_range_and_matches_its_checksum() {
        let written = write_exchange(&empty_federation(), "/project").expect("written");
        let bytes = &written.bytes;
        let count = usize::from(u16_at(bytes, 10));
        let table_end = HEADER_BYTES + count * ENTRY_BYTES;
        let mut previous_end = table_end;

        for index in 0..count {
            let at = HEADER_BYTES + index * ENTRY_BYTES;
            assert_eq!(u16_at(bytes, at + 2), 0, "an entry's reserved field");
            let offset = u32_at(bytes, at + 4) as usize;
            let length = u32_at(bytes, at + 8) as usize;
            assert!(offset >= table_end, "a payload overlaps the table");
            assert!(
                offset + length <= bytes.len(),
                "a payload runs past the end"
            );
            // Written in table order and contiguously, so nothing overlaps.
            assert_eq!(offset, previous_end, "sections are contiguous");
            previous_end = offset + length;
            assert_eq!(
                crc32c(&bytes[offset..offset + length]),
                u32_at(bytes, at + 12)
            );
        }
        assert_eq!(
            previous_end,
            bytes.len(),
            "trailing bytes after the last section"
        );
    }

    #[test]
    fn the_string_table_reserves_index_zero_for_the_empty_string() {
        let written = write_exchange(&empty_federation(), "/project").expect("written");
        let bytes = &written.bytes;
        // The strings section is the first entry.
        let offset = u32_at(bytes, HEADER_BYTES + 4) as usize;
        assert!(
            u32_at(bytes, offset) >= 1,
            "the table holds at least one string"
        );
        assert_eq!(u32_at(bytes, offset + 4), 0, "index 0 is the empty string");
    }

    #[test]
    fn the_root_is_always_source_zero() {
        // Not a refusal but a construction: the writer places the root first, so no caller can
        // produce a container whose source 0 is a link.
        let written = write_exchange(&empty_federation(), "/project").expect("written");
        let bytes = &written.bytes;
        // The sources section is the seventh entry.
        let offset = u32_at(bytes, HEADER_BYTES + 6 * ENTRY_BYTES + 4) as usize;
        assert_eq!(bytes[offset + 8], 0, "source 0 must be kind 0, the root");
        assert_eq!(bytes[offset + 9], 0, "and available");
    }

    #[test]
    fn a_recommended_plugin_names_a_pinned_source() {
        // Section 23.4 requires a refusal to identify the plugin *and* its pinned source. A source
        // with no ref would be one the manager could not install.
        let (name, source) = RECOMMENDED;
        assert!(name.contains('.'), "a namespaced name");
        assert!(source.starts_with("https://github.com/"), "{source}");
        assert!(
            source.contains("?ref="),
            "the source must be pinned: {source}"
        );
    }
}
