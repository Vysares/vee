use vee_core_api::config;

config::setting! {
    pub X: u8 = 5, 1..=256;
}

fn main() {
    let _ = X;
}
