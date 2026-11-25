use vee_core_api::cmd;

#[derive(cmd::Command)]
struct Inner {
    a: u8,
}

#[derive(cmd::Command)]
struct S {
    inner: Inner,
}

fn main() {}
