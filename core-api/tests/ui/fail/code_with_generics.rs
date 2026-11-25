use vee_core_api::event;

#[derive(event::Code)]
#[repr(u16)]
enum Generic<const N: usize> {
    #[severity(Info)]
    A,
}

fn main() {}
