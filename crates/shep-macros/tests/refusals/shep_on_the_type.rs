use shep_client::dogs::dog_config;

#[dog_config]
#[shep(secret)]
struct Config {
    token: String,
}

fn main() {}
