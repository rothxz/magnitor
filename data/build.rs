//! Write the view's shapes into `OUT_DIR/shapes.rs` as Rust structs, so a field added to `app.contract` is a
//! compile error here rather than a record built one position off.

fn main() {
    let path = std::path::Path::new("../app.contract");
    // The contract and every file it uses.
    contract::rerun_if_changed(path);
    let (plan, calls) = contract::compile_path_for_rust(path)
        .unwrap_or_else(|e| panic!("app.contract does not compile: {e:?}"));
    let rust = contract::rust(&plan, &calls).expect("contract rust");
    let out = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("shapes.rs");
    std::fs::write(out, rust).unwrap();
}
