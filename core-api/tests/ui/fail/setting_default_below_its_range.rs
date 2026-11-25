use vee_core_api::config;

config::setting! {
    pub X: u32 = 0, 1..=10;
}

fn main() {
    let _ = X;
}
