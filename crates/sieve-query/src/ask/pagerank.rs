//! Personalized PageRank over the walk-relation edges
//!.

use std::collections::{HashMap, HashSet};

use sieve_core::{Graph, Node};

use crate::relations::WALK_RELATIONS;

/// The undirected adjacency list over the walk relations, restricted to
/// `active_ids`.
pub(crate) fn build_adjacency(
    graph: &Graph,
    active_ids: &HashSet<&str>,
) -> HashMap<String, Vec<String>> {
    let mut adjacency: HashMap<String, Vec<String>> = HashMap::new();
    for edge in &graph.edges {
        if !WALK_RELATIONS.contains(&edge.relation) {
            continue;
        }
        if !active_ids.contains(edge.source.as_str()) || !active_ids.contains(edge.target.as_str())
        {
            continue;
        }
        adjacency
            .entry(edge.source.clone())
            .or_default()
            .push(edge.target.clone());
        adjacency
            .entry(edge.target.clone())
            .or_default()
            .push(edge.source.clone());
    }
    adjacency
}

/// The walk's fixed shape over one node set: the node order, and each
/// node's neighbours as positions. It depends on the graph and the node set
/// only, so a warm caller builds it once and reuses it for every query.
#[derive(Debug)]
pub(crate) struct Topology {
    ids: Vec<String>,
    neighbours: Vec<Vec<usize>>,
}

impl Topology {
    /// Builds the shape over `nodes`, in the caller's own node order.
    ///
    /// The topology keeps the ids in a `Set`: a repeated id counts once, at
    /// its first position. Every `adjacency` id is one of `nodes`, since
    /// `adjacency` was built from the same node set (`build_adjacency`'s
    /// `active_ids`). A neighbour outside `nodes` is skipped.
    pub(crate) fn new(nodes: &[&Node], adjacency: &HashMap<String, Vec<String>>) -> Topology {
        let mut index_of: HashMap<&str, usize> = HashMap::with_capacity(nodes.len());
        let mut ids: Vec<String> = Vec::with_capacity(nodes.len());
        for n in nodes {
            if !index_of.contains_key(n.id.as_str()) {
                index_of.insert(n.id.as_str(), ids.len());
                ids.push(n.id.clone());
            }
        }
        let neighbours = ids
            .iter()
            .map(|id| {
                adjacency
                    .get(id)
                    .map(|list| {
                        list.iter()
                            .filter_map(|nb| index_of.get(nb.as_str()).copied())
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .collect();
        Topology { ids, neighbours }
    }

    /// Runs 25 fixed iterations of personalized PageRank, seeded from `lex`,
    /// and top-normalizes the result.
    ///
    /// Summing floats is not associative, so the order candidate nodes are
    /// folded in can shift the last bit of the result. The walk keeps the
    /// caller's node order, and each node's neighbours in their edge order,
    /// rather than a `HashMap`'s iteration order. A fixed order gives the same bits on every run.
    ///
    /// Returns an empty map when the seed weights sum to zero.
    pub(crate) fn run(&self, lex: &HashMap<String, f64>) -> HashMap<String, f64> {
        let restart: Vec<(usize, f64)> = self
            .ids
            .iter()
            .enumerate()
            .filter_map(|(i, id)| lex.get(id).filter(|&&w| w > 0.0).map(|&w| (i, w)))
            .collect();
        let sum: f64 = restart.iter().map(|(_, w)| w).sum();
        if sum <= 0.0 {
            return HashMap::new();
        }
        let restart: Vec<(usize, f64)> = restart.into_iter().map(|(i, w)| (i, w / sum)).collect();

        let n = self.ids.len();
        let mut mass = vec![0.0_f64; n];
        for (i, w) in &restart {
            mass[*i] = *w;
        }
        // The `rank` is a `Map`: the seeds come first, then every node in the order
        // the walk first touched it. The `dangling` sum and the `next[nb] +=` sums
        // add in that order, so `order` keeps it. A node index sits in `order`
        // once, when first set.
        let mut order: Vec<usize> = restart.iter().map(|(i, _)| *i).collect();
        let mut next = vec![0.0_f64; n];
        let mut seen = vec![false; n];
        let mut next_order: Vec<usize> = Vec::with_capacity(order.len());

        for _ in 0..super::ITERS {
            next.fill(0.0);
            seen.fill(false);
            next_order.clear();
            for (i, w) in &restart {
                next[*i] += super::ALPHA * w;
                seen[*i] = true;
                next_order.push(*i);
            }
            let mut dangling = 0.0_f64;
            for &i in &order {
                // A zero-mass entry is still visited: the walk inserts its
                // neighbours into the `Map` at this point.
                let m = mass[i];
                let neighbours = &self.neighbours[i];
                if neighbours.is_empty() {
                    dangling += m;
                    continue;
                }
                let share = (1.0 - super::ALPHA) * m / neighbours.len() as f64;
                for &j in neighbours {
                    if !seen[j] {
                        seen[j] = true;
                        next_order.push(j);
                    }
                    next[j] += share;
                }
            }
            if dangling > 0.0 {
                for (i, w) in &restart {
                    next[*i] += (1.0 - super::ALPHA) * dangling * w;
                }
            }
            std::mem::swap(&mut mass, &mut next);
            std::mem::swap(&mut order, &mut next_order);
        }

        let max = mass.iter().cloned().fold(0.0_f64, f64::max);
        let mut out = HashMap::with_capacity(order.len());
        if max > 0.0 {
            for &i in &order {
                if mass[i] > 0.0 {
                    out.insert(self.ids[i].clone(), mass[i] / max);
                }
            }
        }
        out
    }
}

/// Runs the walk once over `nodes`, seeded from `lex`. A cold caller uses
/// this; a warm caller keeps a [`Topology`] and calls its `run`.
#[cfg(test)]
pub(crate) fn run(
    nodes: &[&Node],
    lex: &HashMap<String, f64>,
    adjacency: &HashMap<String, Vec<String>>,
) -> HashMap<String, f64> {
    Topology::new(nodes, adjacency).run(lex)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sieve_core::{Kind, Origin, SummaryState};

    fn node(id: &str) -> Node {
        Node {
            id: id.to_string(),
            name: id.to_string(),
            kind: Kind::Function,
            owner: None,
            path: format!("{id}.rs"),
            span: "L1-L2".to_string(),
            signature: None,
            exported: true,
            origin: Origin::Ast,
            body_hash: "0".repeat(64),
            chars: None,
            body_text: None,
            arity: None,
            variadic: None,
            summary_state: SummaryState::Pending,
            summary: None,
            crux: None,
        }
    }

    #[test]
    fn run_on_a_two_node_graph_matches_a_hand_computation_after_25_iterations() {
        // Two nodes, one edge, one seed at "a" with weight 1.
        let mut adjacency = HashMap::new();
        adjacency.insert("a".to_string(), vec!["b".to_string()]);
        adjacency.insert("b".to_string(), vec!["a".to_string()]);
        let mut lex = HashMap::new();
        lex.insert("a".to_string(), 1.0);
        let (a_node, b_node) = (node("a"), node("b"));
        let nodes = [&a_node, &b_node];

        let pr = run(&nodes, &lex, &adjacency);

        // Hand-run the same 25 fixed iterations, alpha = 0.25.
        let alpha = super::super::ALPHA;
        let (mut a, mut b) = (1.0_f64, 0.0_f64);
        for _ in 0..super::super::ITERS {
            let next_a = alpha * 1.0 + (1.0 - alpha) * b;
            let next_b = (1.0 - alpha) * a;
            a = next_a;
            b = next_b;
        }
        let max = a.max(b);
        assert!((pr["a"] - a / max).abs() < 1e-12);
        assert!((pr["b"] - b / max).abs() < 1e-12);
        assert!((pr["a"] - 1.0).abs() < 1e-12); // "a" is the seed, so it tops out at 1.
    }

    /// The walk keeps its ids in a `Set` and the seeds in a `Map`, so a
    /// node id listed twice (a merge conflict) walks as one node. `ask "c
    /// function"` on that repo ranked `a` before `c`, the order a deduped walk
    /// gives.
    #[test]
    fn test_p2_31_run_counts_a_repeated_node_id_once() {
        let mut adjacency = HashMap::new();
        adjacency.insert("a".to_string(), vec!["b".to_string()]);
        adjacency.insert("b".to_string(), vec!["a".to_string()]);
        let mut lex = HashMap::new();
        lex.insert("a".to_string(), 1.0);
        lex.insert("b".to_string(), 3.0);
        let (a_node, b_node) = (node("a"), node("b"));

        let once = run(&[&a_node, &b_node], &lex, &adjacency);
        let twice = run(&[&a_node, &b_node, &b_node], &lex, &adjacency);

        assert_eq!(once.len(), 2);
        assert_eq!(once, twice);
    }

    /// The run sums over its `rank` Map in insertion order, the seeds
    /// first and then each node in the order the walk first touched it. Summing
    /// in node order moves the last bits. The expected values come from a
    /// JS-order simulation of the loop.
    #[test]
    fn test_p3_05_run_sums_in_the_rank_map_insertion_order() {
        let mut adjacency: HashMap<String, Vec<String>> = HashMap::new();
        for (a, b) in [
            ("n0", vec!["n3", "n4", "n3", "n1"]),
            ("n3", vec!["n0", "n1", "n0"]),
            ("n1", vec!["n3", "n0"]),
            ("n4", vec!["n0", "n2"]),
            ("n2", vec!["n4"]),
        ] {
            adjacency.insert(a.to_string(), b.iter().map(|s| s.to_string()).collect());
        }
        let mut lex = HashMap::new();
        lex.insert("n1".to_string(), 1.0);
        lex.insert("n3".to_string(), 0.5);
        let owned: Vec<Node> = (0..5).map(|i| node(&format!("n{i}"))).collect();
        let nodes: Vec<&Node> = owned.iter().collect();

        let pr = run(&nodes, &lex, &adjacency);

        assert_eq!(pr["n0"], 0.9603394986183972);
        assert_eq!(pr["n1"], 0.9770370757477801);
        assert_eq!(pr["n4"], 0.2505190863784907);
    }

    #[test]
    fn run_with_no_positive_seed_returns_an_empty_map() {
        let adjacency = HashMap::new();
        let lex = HashMap::new();
        assert!(run(&[], &lex, &adjacency).is_empty());
    }

    #[test]
    fn run_rescues_a_zero_lexical_node_above_the_rescue_floor() {
        // A chain a-b-c, seed only at "a": "c" should still clear the
        // 0.15 rescue floor after 25 iterations.
        let mut adjacency = HashMap::new();
        adjacency.insert("a".to_string(), vec!["b".to_string()]);
        adjacency.insert("b".to_string(), vec!["a".to_string(), "c".to_string()]);
        adjacency.insert("c".to_string(), vec!["b".to_string()]);
        let mut lex = HashMap::new();
        lex.insert("a".to_string(), 1.0);
        let (a_node, b_node, c_node) = (node("a"), node("b"), node("c"));
        let nodes = [&a_node, &b_node, &c_node];

        let pr = run(&nodes, &lex, &adjacency);
        assert!(pr["c"] >= super::super::RESCUE_FLOOR);
    }
}
