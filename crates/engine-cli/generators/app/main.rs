//! __Label__: the site's own `engine` binary, the engine's command line
//! plus what `src/lib.rs` registers (docs: "App crates").

fn main() -> std::process::ExitCode {
    __crate_ident__::app().run()
}
