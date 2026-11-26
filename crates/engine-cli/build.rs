// The static-blog example is embedded for `engine new`; rebuild when it changes.
fn main() {
    println!("cargo:rerun-if-changed=../../examples/static-blog");
}
