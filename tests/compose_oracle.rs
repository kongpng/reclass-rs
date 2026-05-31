//! Golden-output integration test for the compose engine.
//!
//! Ports `tests/test_compose.cpp::testCompactColumnsEprocess` — load
//! `EPROCESS.rcx`, compose with normal + compact columns, and assert the
//! rendered text is byte-exact against the captured golden fixtures
//! (`_oracle/fixtures/eprocess_{normal,compact}.txt`). This is the strongest
//! parity check for the renderer.

use reclass::compose::{compose, K_COMPACT_TYPE_W};
use reclass::core::NodeTree;
use reclass::provider::NullProvider;

fn load_tree() -> NodeTree {
    let json_str = include_str!("fixtures/EPROCESS.rcx");
    let value: serde_json::Value = serde_json::from_str(json_str).expect("valid EPROCESS.rcx");
    NodeTree::from_json(&value)
}

#[test]
fn compose_eprocess_normal_golden() {
    let tree = load_tree();
    let prov = NullProvider;
    // compose(tree, prov, 0, false) → normal columns.
    let r = compose(
        &tree, &prov, 0, false, false, false, false, true, true, true,
    );
    let expected = include_str!("fixtures/eprocess_normal.txt");
    assert_eq!(
        r.text, expected,
        "normal-column EPROCESS dump must match golden"
    );
}

#[test]
fn compose_eprocess_compact_golden() {
    let tree = load_tree();
    let prov = NullProvider;
    // compose(tree, prov, 0, true) → compact columns.
    let r = compose(&tree, &prov, 0, true, false, false, false, true, true, true);
    let expected = include_str!("fixtures/eprocess_compact.txt");
    assert_eq!(
        r.text, expected,
        "compact-column EPROCESS dump must match golden"
    );
}

#[test]
fn compose_eprocess_compact_typew_capped() {
    let tree = load_tree();
    let prov = NullProvider;
    let normal = compose(
        &tree, &prov, 0, false, false, false, false, true, true, true,
    );
    let compact = compose(&tree, &prov, 0, true, false, false, false, true, true, true);
    assert!(
        compact.layout.type_w <= K_COMPACT_TYPE_W,
        "compact typeW={} should be <= {}",
        compact.layout.type_w,
        K_COMPACT_TYPE_W
    );
    assert!(
        normal.layout.type_w > compact.layout.type_w,
        "normal typeW={} should exceed compact typeW={}",
        normal.layout.type_w,
        compact.layout.type_w
    );
    // Long type prints in full in compact mode (overflow, no truncation).
    assert!(compact
        .text
        .lines()
        .any(|l| l.contains("_PS_DYNAMIC_ENFORCED_ADDRESS_RANGES")));
}
