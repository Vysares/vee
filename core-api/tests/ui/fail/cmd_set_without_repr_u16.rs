use vee_core_api::cmd;

#[derive(cmd::Command)]
struct A {
    a: u8,
}

#[derive(cmd::CmdSet)]
enum E {
    X(A),
}

fn main() {}
