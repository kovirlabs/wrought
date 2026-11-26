// The examples are embedded for `engine new`; rebuild when they change.
fn main() {
    println!("cargo:rerun-if-changed=../../examples/static-blog");
    println!("cargo:rerun-if-changed=../../examples/cms-site");
}
