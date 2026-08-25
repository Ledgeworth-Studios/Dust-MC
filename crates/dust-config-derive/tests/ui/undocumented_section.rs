//! A whole section nobody documented. This must not build either.
use dust_config::ConfigSection;

#[derive(Default, ConfigSection)]
struct Section {
    /// This one is fine.
    pub documented: bool,
}

fn main() {}
