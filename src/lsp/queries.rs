use std::sync::LazyLock;

use tree_sitter::Query;

pub static SEARCH: LazyLock<Query> = LazyLock::new(|| {
    const QS: &str = r#"
        [
            (functional_notation (function: atom (_) @name)
            (arg_list) @args
        ]
    "#;
    Query::new(&prolog_grammar::LANGUAGE.into(), QS).unwrap()
});

pub static COMPLETE_ON: LazyLock<Query> = LazyLock::new(|| {
    const QS: &str = r#"
        [
            (atom (_) @name)
            (variable_term) @var
        ]
    "#;
    Query::new(&prolog_grammar::LANGUAGE.into(), QS).unwrap()
});
