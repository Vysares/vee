use vee_core_api::cmd;

#[derive(cmd::Command)]
struct A {
    a: u8,
}

#[derive(cmd::CmdSet)]
#[repr(u8)]
enum E {
    X(A) = 1,
}

fn main() {}
