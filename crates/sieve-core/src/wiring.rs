//! The `wiring.json` schema: the node, edge, and graph types Sieve writes
//! and reads. Field names and order match the parity spec (P2-06 to
//! P2-13).

use serde::{Deserialize, Serialize};

/// The kind of symbol a node names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    File,
    Class,
    Function,
    Method,
    Interface,
    Type,
    Enum,
    Struct,
    Trait,
    Module,
    Constant,
    Variable,
}

/// How a node's data was extracted: from a language grammar, or from the
/// generic breadth-tier walker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Ast,
    Generic,
}

/// The Tier-2 summary state of a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummaryState {
    Pending,
    Ready,
    Stale,
}

/// The kind of relationship an edge records between two nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    Contains,
    Calls,
    Imports,
    References,
    Implements,
    Extends,
}

impl Relation {
    /// Returns the serialized name of this relation, for the edge sort key.
    pub fn as_str(&self) -> &'static str {
        match self {
            Relation::Contains => "contains",
            Relation::Calls => "calls",
            Relation::Imports => "imports",
            Relation::References => "references",
            Relation::Implements => "implements",
            Relation::Extends => "extends",
        }
    }
}

/// How sure Sieve is that an edge's target is correct.
///
/// The schema declares `lsp_dispatch` (P2-12), but Sieve never
/// emits it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    LspResolved,
    LspDispatch,
    Extracted,
    Inferred,
}

/// The span of a symbol's definition, as `code` and a `span` string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Crux {
    pub code: String,
    pub span: String,
}

/// One symbol in the graph: a file, a class, a function, or another named
/// unit.
///
/// Field order matches the writer: `owner`, `arity`, and `variadic`
/// come after `crux`, because `serde_json` writes fields in this order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub path: String,
    pub span: String,
    pub signature: Option<String>,
    pub exported: bool,
    pub origin: Origin,
    pub body_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chars: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_text: Option<String>,
    pub summary_state: SummaryState,
    pub summary: Option<String>,
    pub crux: Option<Crux>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arity: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variadic: Option<bool>,
}

/// A directed relationship from one node to another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub source: String,
    pub target: String,
    pub relation: Relation,
    pub confidence: Confidence,
}

/// One project-marker directory scope, such as a package root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    pub prefix: String,
    pub label: String,
    pub markers: Vec<String>,
}

/// Whole-graph metadata: version, counts, languages, and scopes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    pub version: u32,
    #[serde(rename = "nodeCount")]
    pub node_count: usize,
    #[serde(rename = "edgeCount")]
    pub edge_count: usize,
    pub languages: Vec<String>,
    pub scopes: Vec<Scope>,
}

/// The symbol graph: every node and every edge for one indexed repo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Graph {
    pub meta: Meta,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

/// Formats a 1-indexed inclusive line span as `L<start>-L<end>` (P2-08).
pub fn span(start: u32, end: u32) -> String {
    format!("L{start}-L{end}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_formats_as_l_start_dash_l_end() {
        assert_eq!(span(1, 2), "L1-L2");
        assert_eq!(span(6, 11), "L6-L11");
    }

    #[test]
    fn kind_variants_serialize_as_lowercase_names() {
        let kinds = [
            (Kind::File, "\"file\""),
            (Kind::Class, "\"class\""),
            (Kind::Function, "\"function\""),
            (Kind::Method, "\"method\""),
            (Kind::Interface, "\"interface\""),
            (Kind::Type, "\"type\""),
            (Kind::Enum, "\"enum\""),
            (Kind::Struct, "\"struct\""),
            (Kind::Trait, "\"trait\""),
            (Kind::Module, "\"module\""),
            (Kind::Constant, "\"constant\""),
            (Kind::Variable, "\"variable\""),
        ];
        for (kind, expected) in kinds {
            let json = serde_json::to_string(&kind).expect("kind serializes");
            assert_eq!(json, expected);
        }
    }

    #[test]
    fn confidence_variants_serialize_as_lowercase_names() {
        assert_eq!(
            serde_json::to_string(&Confidence::LspResolved).expect("serializes"),
            "\"lsp_resolved\""
        );
        assert_eq!(
            serde_json::to_string(&Confidence::LspDispatch).expect("serializes"),
            "\"lsp_dispatch\""
        );
        assert_eq!(
            serde_json::to_string(&Confidence::Extracted).expect("serializes"),
            "\"extracted\""
        );
        assert_eq!(
            serde_json::to_string(&Confidence::Inferred).expect("serializes"),
            "\"inferred\""
        );
    }

    fn sample_node(id: &str, owner: Option<&str>) -> Node {
        Node {
            id: id.to_string(),
            name: "foo".to_string(),
            kind: Kind::Function,
            owner: owner.map(|o| o.to_string()),
            path: "a.rs".to_string(),
            span: span(1, 2),
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
    fn node_without_owner_skips_the_owner_key_and_writes_null_signature() {
        let node = sample_node("a.rs#foo", None);
        let json = serde_json::to_string(&node).expect("node serializes");
        assert!(!json.contains("\"owner\""));
        assert!(json.contains("\"signature\":null"));
    }

    #[test]
    fn node_with_owner_includes_the_owner_key() {
        let node = sample_node("a.rs#Owner.foo", Some("Owner"));
        let json = serde_json::to_string(&node).expect("node serializes");
        assert!(json.contains("\"owner\":\"Owner\""));
    }
}
