use shep_client::dogs::dog_config;

#[dog_config(secret)]
struct Config {
    token: String,
}

fn main() {}
