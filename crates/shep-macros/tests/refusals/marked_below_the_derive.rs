use shep_client::dogs::dog_config;

#[derive(schemars::JsonSchema)]
#[dog_config]
struct Config {
    #[shep(secret)]
    token: String,
}

fn main() {}
