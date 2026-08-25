//! A setting nobody documented. This must not build.
use dust_config::ConfigSection;

/// A section that is itself documented, so only the field is at fault.
#[derive(Default, ConfigSection)]
struct Section {
    /// This one is fine.
    pub documented: bool,

    pub undocumented: bool,
}

fn main() {}
