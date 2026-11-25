use vee_core_api::cmd;

#[derive(cmd::Command)]
struct A {
    a: u8,
}

#[derive(cmd::CmdSet)]
#[repr(u16)]
enum E {
    X { a: A } = 1,
}

fn main() {}
