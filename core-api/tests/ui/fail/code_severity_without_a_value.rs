use vee_core_api::event;

#[derive(event::Code)]
#[repr(u16)]
enum E {
    #[severity]
    A,
}

fn main() {}
