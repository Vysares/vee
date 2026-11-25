use vee_core_api::cmd;

// `#[validate]` promises a hand-written `validate`. On its own that compiles (nothing in the
// command's own code calls it); the missing method is found where a `CmdSet` calls it.
#[derive(cmd::Command)]
#[validate]
struct A {
    a: u8,
}

#[derive(cmd::CmdSet)]
#[repr(u16)]
enum E {
    X(A) = 1,
}

fn main() {}
