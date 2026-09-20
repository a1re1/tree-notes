//! Rust adapter: `fn`, `struct`, `enum`, `trait`, `impl`, `mod`, `const`/`static`, `type` aliases
//! and `macro_rules!`. Items inside a function body are locals and are never members.

use super::{Language, MemberRule, MemberSpec};

/// The Rust language entry.
pub const RUST: Language = Language {
    id: "rust",
    extensions: &["rs"],
    grammar,
    separator: "::",
    members: RUST_MEMBERS,
};

/// Node kinds that become annotatable members, in match order.
///
/// `impl` blocks take their name from the `type` field, so `impl Cache { fn put() {} }` yields the
/// member `Cache` and the nested `Cache::put`.
pub const RUST_MEMBERS: &[MemberSpec] = &[
    MemberSpec {
        node_kind: "mod_item",
        member_kind: "module",
        name_field: Some("name"),
        rule: MemberRule::Plain,
        container: true,
        body_kind: None,
    },
    MemberSpec {
        node_kind: "impl_item",
        member_kind: "impl",
        name_field: Some("type"),
        rule: MemberRule::Plain,
        container: true,
        body_kind: None,
    },
    MemberSpec {
        node_kind: "trait_item",
        member_kind: "trait",
        name_field: Some("name"),
        rule: MemberRule::Plain,
        container: true,
        body_kind: None,
    },
    MemberSpec {
        node_kind: "struct_item",
        member_kind: "struct",
        name_field: Some("name"),
        rule: MemberRule::Plain,
        container: false,
        body_kind: Some("field_declaration_list"),
    },
    MemberSpec {
        node_kind: "enum_item",
        member_kind: "enum",
        name_field: Some("name"),
        rule: MemberRule::Plain,
        container: false,
        body_kind: Some("enum_variant_list"),
    },
    MemberSpec {
        node_kind: "function_item",
        member_kind: "function",
        name_field: Some("name"),
        rule: MemberRule::Plain,
        container: false,
        body_kind: Some("block"),
    },
    MemberSpec {
        node_kind: "const_item",
        member_kind: "constant",
        name_field: Some("name"),
        rule: MemberRule::Plain,
        container: false,
        body_kind: None,
    },
    MemberSpec {
        node_kind: "static_item",
        member_kind: "constant",
        name_field: Some("name"),
        rule: MemberRule::Plain,
        container: false,
        body_kind: None,
    },
    MemberSpec {
        node_kind: "type_item",
        member_kind: "type",
        name_field: Some("name"),
        rule: MemberRule::Plain,
        container: false,
        body_kind: None,
    },
    MemberSpec {
        node_kind: "macro_definition",
        member_kind: "macro",
        name_field: Some("name"),
        rule: MemberRule::Plain,
        container: false,
        body_kind: None,
    },
];

fn grammar() -> tree_sitter::Language {
    tree_sitter_rust::LANGUAGE.into()
}
