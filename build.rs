fn main() {
    let grammar_dir = "grammar/src";
    let parser = format!("{grammar_dir}/parser.c");

    cc::Build::new()
        .include(grammar_dir)
        .file(&parser)
        .compile("tree-sitter-cr");

    println!("cargo:rerun-if-changed=grammar/grammar.js");
    println!("cargo:rerun-if-changed=grammar/src/parser.c");
    println!("cargo:rerun-if-changed=grammar/src/tree_sitter/parser.h");
}
