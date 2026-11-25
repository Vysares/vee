use vee_core_api::cmd;

#[derive(cmd::Command)]
struct S {}

fn main() {
    assert_eq!(S::WIRE_LEN, 0);
}
