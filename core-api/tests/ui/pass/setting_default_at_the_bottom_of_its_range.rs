use vee_core_api::config;

config::setting! {
    pub X: u32 = 1, 1..=10;
}

fn main() {
    let _ = X;
}
