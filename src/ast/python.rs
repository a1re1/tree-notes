//! Python adapter: `def`, `async def`, `class`, decorated definitions and module/class-level
//! constants.
//!
//! A `decorated_definition` is recorded as the member (its span and hash include the decorators, so
//! adding one re-stales the member) but takes its kind, name and children from the decorated
//! definition. Nested `def`s are members qualified by their enclosing function (`outer.inner`);
//! ordinary locals such as `x = 1` are not members.

use super::{Language, MemberRule, MemberSpec};

/// The Python language entry.
pub const PYTHON: Language = Language {
    id: "python",
    extensions: &["py", "pyi"],
    grammar,
    separator: ".",
    members: PYTHON_MEMBERS,
};

/// Node kinds that become annotatable members, in match order.
pub const PYTHON_MEMBERS: &[MemberSpec] = &[
    MemberSpec {
        node_kind: "class_definition",
        member_kind: "class",
        name_field: Some("name"),
        rule: MemberRule::Plain,
        container: true,
        body_kind: None,
    },
    MemberSpec {
        node_kind: "function_definition",
        member_kind: "function",
        name_field: Some("name"),
        rule: MemberRule::Plain,
        container: true,
        body_kind: None,
    },
    MemberSpec {
        node_kind: "decorated_definition",
        member_kind: "function",
        name_field: None,
        rule: MemberRule::Decorated,
        container: false,
        body_kind: None,
    },
    MemberSpec {
        node_kind: "assignment",
        member_kind: "constant",
        name_field: Some("left"),
        rule: MemberRule::FunctionValue,
        container: false,
        body_kind: None,
    },
];

fn grammar() -> tree_sitter::Language {
    tree_sitter_python::LANGUAGE.into()
}
