// The parser is the one `tree-sitter generate` produced from
// `bison_flex/grammar.tsg`; compiling it into this crate is what every
// tree-sitter grammar crate does, and it is the whole integration surface.
fn main() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = dir.parent().expect("bison_flex").join("src");
    if !src.join("parser.c").exists() {
        panic!(
            "{} is missing: run bison_flex/check.sh to generate it from grammar.tsg",
            src.join("parser.c").display()
        );
    }
    cc::Build::new()
        .include(&src)
        .file(src.join("parser.c"))
        .flag_if_supported("-Wno-unused-parameter")
        .compile("tree-sitter-bison-flex");
    println!("cargo:rerun-if-changed={}", src.join("parser.c").display());
    println!(
        "cargo:rerun-if-changed={}",
        src.join("tree_sitter/parser.h").display()
    );
}
