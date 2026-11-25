use vee_core_api::cmd;

struct NotACommand;

#[derive(cmd::CmdSet)]
#[repr(u16)]
enum E {
    X(NotACommand) = 1,
}

fn main() {}
