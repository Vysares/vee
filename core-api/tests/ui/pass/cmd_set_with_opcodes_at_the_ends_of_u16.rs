use vee_core_api::cmd;

#[derive(cmd::Command)]
struct A {
    a: u8,
}

#[derive(cmd::CmdSet)]
#[repr(u16)]
enum E {
    Low(A) = 0,
    High(A) = 65535,
}

fn main() {
    assert_eq!(E::Low(A { a: 0 }).opcode(), 0);
    assert_eq!(E::High(A { a: 0 }).opcode(), 65535);
}
