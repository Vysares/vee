use vee_core_api::event;

#[derive(event::Code)]
enum E {
    #[severity(Info)]
    A,
}

fn main() {}
