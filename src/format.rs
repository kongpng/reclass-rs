//! Value formatting — `typeName`, scalar/pointer formatters, value
//! read/parse/validate, hex/ASCII previews, enum/bitfield member rendering.
//!
//! Port of `src/format.cpp` (the `fmt::` namespace declared in `core.h`).
//! **SKELETON** — bodies are filled in by the dedicated `format` workflow
//! (ARCHITECTURE.md §9). Key signatures are in place.

use crate::core::{Node, NodeKind};
use crate::provider::Provider;

/// `fmt::TypeNameFn` (`core.h:1474`) — pluggable type-name override hook.
pub type TypeNameFn = fn(NodeKind) -> String;

/// `fmt::setTypeNameProvider(fn)` (`core.h:1475`). SKELETON.
pub fn set_type_name_provider(_f: Option<TypeNameFn>) {
    todo!("port format.cpp setTypeNameProvider (workflow: format-render)")
}

/// `fmt::typeName(kind)` (`core.h`). SKELETON.
pub fn type_name(_kind: NodeKind) -> String {
    todo!("port format.cpp typeName (workflow: format-render)")
}

/// `fmt::readValue(node, prov, addr, subLine)` (`core.h`) — render a field's
/// current value as display text. SKELETON.
pub fn read_value(_node: &Node, _prov: &dyn Provider, _addr: u64, _sub_line: i32) -> String {
    todo!("port format.cpp readValue (workflow: format-render)")
}

/// `fmt::parseValue(node, text, &ok)` (`core.h`) — parse user-entered text into
/// raw bytes for the node's kind. Returns `None` on parse failure. SKELETON.
pub fn parse_value(_node: &Node, _text: &str) -> Option<Vec<u8>> {
    todo!("port format.cpp parseValue (workflow: format-render)")
}

/// `fmt::parseAsciiValue(text, size, &ok)` (`core.h`). SKELETON.
pub fn parse_ascii_value(_text: &str, _size: i32) -> Option<Vec<u8>> {
    todo!("port format.cpp parseAsciiValue (workflow: format-render)")
}

/// `fmt::validateBaseAddress(text)` (`core.h`). SKELETON.
pub fn validate_base_address(_text: &str) -> bool {
    todo!("port format.cpp validateBaseAddress (workflow: format-render)")
}
