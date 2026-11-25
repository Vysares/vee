use vee_core_api::cmd;

#[derive(cmd::Command)]
struct S {
    a: u64,
    b: u64,
    c: u64,
    d: u64,
}

fn main() {
    assert_eq!(S::WIRE_LEN, cmd::CMD_ARGS_MAX_BYTES);
}
