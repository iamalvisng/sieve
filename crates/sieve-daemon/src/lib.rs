//! The `sieve mcp` server: an NDJSON JSON-RPC 2.0 server over stdio that
//! answers the six canonical Sieve MCP tools (the `mcp-server.md` note).

pub mod names;
pub mod rpc;
mod templates;
pub mod tools;

pub use rpc::{serve, serve_with_upkeep};
