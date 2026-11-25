use vee_core_api::cmd;

#[derive(cmd::Command)]
struct S {
    bytes: [u8; 4],
}

fn main() {}
