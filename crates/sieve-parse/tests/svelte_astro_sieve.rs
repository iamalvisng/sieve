//! The sieve product indexes `.svelte` and `.astro`. A call from a Svelte
//! script to a `.ts` function becomes a call edge.

use std::fs;
use std::path::Path;

use sieve_core::Kind;
use sieve_parse::build_graph;

/// Writes a repo with one `.ts`, one `.svelte` and one `.astro` file.
fn write_repo(root: &Path) {
    fs::create_dir_all(root).expect("create scratch dir");
    fs::write(
        root.join("lib.ts"),
        "export function helper(): number {\n  return 1;\n}\n",
    )
    .expect("write lib.ts");
    fs::write(
        root.join("A.svelte"),
        "<script lang=\"ts\">\n  import { helper } from './lib';\n  export function go(): number {\n    return helper();\n  }\n</script>\n<p>x</p>\n",
    )
    .expect("write A.svelte");
    fs::write(
        root.join("P.astro"),
        "---\nexport const t = 1;\n---\n<p>x</p>\n",
    )
    .expect("write P.astro");
}

#[test]
fn test_svelte_call_to_a_ts_function_gives_a_call_edge() {
    let root = std::env::temp_dir().join(format!("sieve-svelte-edge-{}", std::process::id()));
    write_repo(&root);
    let graph = build_graph(&root, &root.join(".ctx")).expect("build graph");
    let _ = fs::remove_dir_all(&root);
    assert!(graph
        .nodes
        .iter()
        .any(|n| n.path == "P.astro" && n.kind == Kind::File));
    let go = graph
        .nodes
        .iter()
        .find(|n| n.name == "go" && n.path == "A.svelte")
        .expect("go node");
    let helper = graph
        .nodes
        .iter()
        .find(|n| n.name == "helper")
        .expect("helper node");
    assert!(
        graph
            .edges
            .iter()
            .any(|e| e.source == go.id && e.target == helper.id),
        "no go -> helper edge"
    );
}

#[test]
fn test_svelte_astro_sieve_product_claims_both_extensions() {
    assert!(sieve_parse::container_lang_of("a.svelte").is_some());
    assert!(sieve_parse::container_lang_of("a.astro").is_some());
    assert!(sieve_parse::container_lang_of("a.ts").is_none());
}

#[test]
fn test_svelte_comment_blocks_use_file_lines() {
    let src = "<p>x</p>\n<script>\n  // because the cache is cold\n  function a() {}\n</script>\n";
    let got = sieve_parse::comments::comment_blocks("A.svelte", src, 1, 5).expect("svelte parses");
    assert_eq!(got, vec![(3, "because the cache is cold".to_string())]);
}
