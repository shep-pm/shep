//! The shapes `dog_config`'s own documentation calls accepted. None of them
//! marks anything, so none needs `JsonSchema`: what is under test is that the
//! attribute emits a usable impl rather than refusing.

use shep_client::dogs::{DogConfig, dog_config};

#[dog_config]
struct NoFields {}

#[dog_config]
struct UnmarkedTuple(String, u64);

#[dog_config]
enum UnitVariants {
    Quiet,
    Loud,
}

/// Generics and a where clause, which the impl has to carry through.
#[dog_config]
struct Generic<T>
where
    T: Clone,
{
    inner: T,
}

fn takes_a_dog_config<T: DogConfig>() {}

fn main() {
    takes_a_dog_config::<NoFields>();
    takes_a_dog_config::<UnmarkedTuple>();
    takes_a_dog_config::<UnitVariants>();
    takes_a_dog_config::<Generic<String>>();
}
