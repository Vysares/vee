use vee_core_api::config;

config::setting! {
    pub X: u16 = 0, 0..=10;
}

fn main() {
    let _ = X;
}
