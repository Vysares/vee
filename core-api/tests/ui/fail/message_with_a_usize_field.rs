use vee_core_api::bus;

#[derive(bus::Message)]
struct S {
    n: usize,
}

fn main() {}
