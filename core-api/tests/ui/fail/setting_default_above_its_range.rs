use vee_core_api::config;

config::setting! {
    pub X: u32 = 11, 1..=10;
}

fn main() {
    let _ = X;
}
