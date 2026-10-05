//! The graph model for Sieve. It holds the `wiring.json` schema and the
//! byte-stable write. Other crates build the graph and query it.

pub mod askindex;
pub mod cards;
pub mod collate;
pub mod concept;
pub mod covers;
pub mod fingerprint;
pub mod ignore;
pub mod lang;
pub mod lock;
pub mod node_error;
pub mod product;
pub mod session;
#[cfg(test)]
mod test_support;
pub mod walk;
pub mod wiring;
pub mod workspace;
pub mod write;

pub use product::{product, Product, SIEVE};
pub use wiring::{
    span, Confidence, Crux, Edge, Graph, Kind, Meta, Node, Origin, Relation, Scope, SummaryState,
};
pub use write::write_graph;
