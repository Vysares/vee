use vee_core_api::cmd;

#[derive(cmd::Command)]
#[validate(always)]
struct S {
    a: u8,
}

fn main() {}
