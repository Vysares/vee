use vee_core_api::cmd;

#[derive(cmd::Command)]
#[validate]
struct A {
    a: u8,
}

impl A {
    fn validate(&self) -> Result<(), cmd::CmdErr> {
        if self.a > 0 { Ok(()) } else { Err(cmd::CmdErr::Invalid("zero")) }
    }
}

fn main() {
    assert!(A { a: 1 }.validate().is_ok());
}
