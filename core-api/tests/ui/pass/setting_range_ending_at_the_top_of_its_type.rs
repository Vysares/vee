use vee_core_api::config;

config::setting! {
    pub X: u8 = 255, 1..=255;
}

fn main() {
    let _ = X;
}
