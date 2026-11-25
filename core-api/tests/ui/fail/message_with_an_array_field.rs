use vee_core_api::bus;

#[derive(bus::Message)]
struct S {
    bytes: [u8; 4],
}

fn main() {}
