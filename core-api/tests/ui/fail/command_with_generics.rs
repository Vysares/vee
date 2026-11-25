use vee_core_api::cmd;

#[derive(cmd::Command)]
struct S<const N: usize> {
    a: u8,
}

fn main() {}
