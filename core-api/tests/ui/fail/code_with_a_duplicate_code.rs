use vee_core_api::event;

#[derive(event::Code)]
#[repr(u16)]
enum E {
    #[severity(Info)]
    A = 1,
    #[severity(Info)]
    B = 1,
}

fn main() {}
