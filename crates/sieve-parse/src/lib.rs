//! Extraction: turns source text into graph nodes, and builds the whole
//! repo graph.

pub mod bindings;
pub mod build;
pub mod cache;
pub mod check;
pub mod comments;
pub mod container;
pub mod extract;
pub mod generic;
pub mod lsp;
pub mod refresh;
pub mod resolve;
pub mod stamp;
pub use build::{
    build_graph, build_graph_cached, build_graph_cached_with, build_graph_with_raw, BuildOptions,
    BuildReport,
};
pub use cache::{CacheEntry, ExtractCache};
pub use check::{
    check_context, check_graph, federated_check_text, format_check_report,
    format_graph_check_report, ContentDrift, ContextCheck, GraphCheck, PENDING_SAMPLE,
};
pub use container::{container_lang_of, extract_container, ContainerLang, CONTAINER_LANGS};
pub use extract::{Extractor, RawEdge};
pub use generic::{generic_lang_of, GenericExtractor, GenericLang, GENERIC_LANGS};
pub use lsp::{enrich_with_lsp, enrich_with_lsp_report, LspReport};
pub use refresh::{
    ensure_fresh_graph, env_truthy, rebuild_graph_only, refresh_note, RebuildReport,
    RefreshOptions, RefreshOutcome,
};
pub use resolve::resolve_edges;
pub use stamp::extractor_stamp;
