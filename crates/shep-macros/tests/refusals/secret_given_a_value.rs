use shep_client::dogs::dog_config;

#[dog_config]
struct Config {
    #[shep(secret = true)]
    token: String,
}

fn main() {}
