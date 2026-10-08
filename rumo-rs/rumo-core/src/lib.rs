// SPDX-License-Identifier: Apache-2.0

pub mod canvas;
pub mod codec;
pub mod ease;
pub mod edl;
pub mod effect;
pub mod model;
pub mod project_json;

pub const RUMO_CORE_VERSION: &str = "0.0.1";

pub fn core_version() -> &'static str {
    RUMO_CORE_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_set() {
        assert_eq!(core_version(), "0.0.1");
    }
}
