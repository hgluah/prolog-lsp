use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    unsafe fn tree_sitter_prolog() -> *const ();
}

pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_prolog) };
pub const NODE_TYPES: &str = include_str!("../tree-sitter-prolog/grammars/prolog/src/node-types.json");
pub const HIGHLIGHTS_QUERY: &str = include_str!("../tree-sitter-prolog/grammars/prolog/queries/highlights.scm");
pub const INJECTIONS_QUERY: &str = include_str!("../tree-sitter-prolog/queries/injections.scm");
