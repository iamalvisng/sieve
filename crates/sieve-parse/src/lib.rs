//! Extraction: turns source text into graph nodes, and builds the whole
//! repo graph.

pub mod bindings;
pub mod build;
pub mod cache;
pub mod check;
pub mod comments;
pub mod container;
mod cs_index;
mod cs_proof;
mod cs_tree;
pub mod extract;
pub mod generic;
mod go_index;
mod go_proof;
mod go_tree;
pub mod guard;
mod java_index;
mod java_proof;
mod java_tree;
mod kt_index;
mod kt_proof;
mod kt_stdlib;
mod kt_tree;
pub mod lsp;
mod py_proof;
mod py_stdlib;
mod py_tree;
pub mod refresh;
pub mod resolve;
mod rust_items;
mod rust_proof;
mod rust_tree;
pub mod stamp;
pub mod timing;
pub use build::{
    build_graph, build_graph_cached, build_graph_cached_with, build_graph_with_raw, BuildOptions,
    BuildReport,
};
pub use cache::{CacheEntry, ExtractCache};
pub use check::{
    behind_files, check_context, check_graph, check_graph_lookup, federated_check_text,
    format_check_report, format_graph_check_report, ContentDrift, ContextCheck, GraphCheck,
    LookupCheck, LOOKUP_DRIFT_CAP, PENDING_SAMPLE,
};
pub use container::{container_lang_of, extract_container, ContainerLang, CONTAINER_LANGS};
pub use extract::{Extractor, RawEdge, ReExport};
pub use generic::{generic_lang_of, GenericExtractor, GenericLang, GENERIC_LANGS};
pub use lsp::{enrich_with_lsp, enrich_with_lsp_report, LspReport};
pub use refresh::{
    ensure_fresh_graph, env_truthy, lookup_path, rebuild_graph_only, refresh_note,
    write_wiring_and_lookup, RebuildReport, RefreshOptions, RefreshOutcome,
};
pub use resolve::resolve_edges;
pub use rust_tree::RustPackage;
pub use stamp::extractor_stamp;
