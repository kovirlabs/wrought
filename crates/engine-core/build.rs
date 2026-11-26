// Built-in templates and vendored assets are embedded; rebuild when they change.
fn main() {
    println!("cargo:rerun-if-changed=builtin");
    println!("cargo:rerun-if-changed=vendor");
}
