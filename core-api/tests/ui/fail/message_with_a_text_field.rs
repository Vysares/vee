use vee_core_api::bus;

#[derive(bus::Message)]
struct S {
    name: String,
}

fn main() {}
