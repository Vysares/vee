use vee_core_api::bus;

#[derive(bus::Message)]
struct S<const N: usize> {
    a: u8,
}

fn main() {}
