//! Product metadata and naming constants.
//!
//! Defined once here and mirrored in `macos/Config/Product.xcconfig`.
//! Do not hardcode these names anywhere else.

pub const DISPLAY_NAME: &str = "Shiki";
pub const CLI_BIN: &str = "shiki-term";
pub const CLI_CRATE: &str = "shiki-term";
pub const HOMEBREW_FORMULA: &str = "shiki-term";
pub const HOMEBREW_CASK: &str = "shiki-term-app";
pub const BUNDLE_ID: &str = "io.github.quiinthieu.shiki-term";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_constants_match_plan_spec() {
        assert_eq!(DISPLAY_NAME, "Shiki");
        assert_eq!(CLI_BIN, "shiki-term");
        assert_eq!(CLI_CRATE, "shiki-term");
        assert_eq!(HOMEBREW_FORMULA, "shiki-term");
        assert_eq!(HOMEBREW_CASK, "shiki-term-app");
        assert_eq!(BUNDLE_ID, "io.github.quiinthieu.shiki-term");
        assert_eq!(VERSION.split('.').count(), 3);
    }
}
