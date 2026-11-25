use vee_core_api::event;

#[derive(event::Code)]
#[repr(u8)]
enum E {
    #[severity(Info)]
    A,
}

fn main() {}
