use shep_client::dogs::dog_config;

#[dog_config]
enum Sink {
    #[shep(secret)]
    Discord {
        url: String,
    },
}

fn main() {}
