//! The in-degree map: how many edges of the walk relations point at each
//! node id.

use std::collections::HashMap;

use sieve_core::{Graph, Relation};

/// The relations `grep` walks for the in-degree count. `contains` is not
/// one of them.
pub const WALK_RELATIONS: [Relation; 5] = [
    Relation::Calls,
    Relation::References,
    Relation::Imports,
    Relation::Implements,
    Relation::Extends,
];

/// Counts, for every edge target, how many edges of a walk relation point
/// at it. A self-loop counts. `contains` edges are not counted.
pub fn compute_in_degree(graph: &Graph) -> HashMap<String, usize> {
    let mut in_degree = HashMap::new();
    for edge in &graph.edges {
        if WALK_RELATIONS.contains(&edge.relation) {
            *in_degree.entry(edge.target.clone()).or_insert(0) += 1;
        }
    }
    in_degree
}

#[cfg(test)]
mod tests {
    use super::*;
    use sieve_core::{Confidence, Edge, Meta};

    fn graph_with_edges(edges: Vec<Edge>) -> Graph {
        Graph {
            meta: Meta {
                version: 1,
                node_count: 0,
                edge_count: edges.len(),
                languages: Vec::new(),
                scopes: Vec::new(),
            },
            nodes: Vec::new(),
            edges,
        }
    }

    fn edge(source: &str, target: &str, relation: Relation) -> Edge {
        Edge {
            source: source.to_string(),
            target: target.to_string(),
            relation,
            confidence: Confidence::Extracted,
        }
    }

    #[test]
    fn in_degree_counts_a_self_loop_and_skips_contains() {
        let graph = graph_with_edges(vec![
            edge("a", "a", Relation::Calls),
            edge("b", "a", Relation::Contains),
            edge("c", "a", Relation::References),
        ]);
        let in_degree = compute_in_degree(&graph);
        assert_eq!(in_degree.get("a"), Some(&2));
    }
}
