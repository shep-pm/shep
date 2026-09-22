use shep_client::dogs::dog_config;

#[dog_config]
struct Config {
    #[shep(scret)]
    token: String,
}

fn main() {}
