fn main() {
    let src_dir = std::path::Path::new("tree-sitter-prolog/grammars/prolog/src");
    let mut c_config = cc::Build::new();
    c_config
        .std("c11")
        .include(&src_dir)
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-unused-but-set-variable")
        .flag_if_supported("-Wno-trigraphs");
    #[cfg(target_env = "msvc")]
    c_config.flag("-utf-8");
    c_config.file(&src_dir.join("parser.c"));
    c_config.compile("tree-sitter-prolog");
}
