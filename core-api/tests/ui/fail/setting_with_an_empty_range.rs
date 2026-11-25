use vee_core_api::config;

config::setting! {
    pub X: u32 = 5, 10..=1;
}

fn main() {
    let _ = X;
}
