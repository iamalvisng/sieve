//! Ranking and traversal over the graph: who calls a node, and how well a
//! document matches a query.

pub mod ask;
pub mod blast;
pub mod callers;
pub mod grep;
pub mod map;
pub mod relations;
pub mod skeleton;
#[cfg(test)]
mod test_support;
pub mod viz;
pub mod workspace;

/// A placeholder ranking score. It always returns zero until real BM25 lands.
pub fn bm25_stub(_query: &str, _document: &str) -> f64 {
    0.0
}
