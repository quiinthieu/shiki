#[path = "../../../tests/support/mod.rs"]
mod support;

use sk_store::error::StoreError;
use sk_store::palette::{load_palette, load_palette_file};
use std::fs;
use support::TestEnv;

const VALID_CUSTOM_PALETTE: &str = r##"
name = "forest-study"
description = "Custom user palette inside TestEnv"

[[anchors]]
d = 0.0
bg = "#101816"
fg = "#dce8e3"
cursor = "#e4be78"
selection = "#233631"
ansi = [
  "#182421", "#e07274", "#78cc98", "#e4c27a",
  "#68b2ec", "#c985de", "#5cc2c8", "#c9d5d4",
  "#5a6b67", "#ef888b", "#95dfb2", "#f0d391",
  "#84c4f6", "#da9eed", "#7ad5da", "#eef6f5",
]

[[anchors]]
d = 1.0
bg = "#2d403b"
fg = "#f4faf7"
cursor = "#f0c87c"
selection_bg = "#1c2825"
selection_fg = "#f2faf6"
ansi = [
  "#23322e", "#f2928c", "#92e4b0", "#f4d88c",
  "#8ecbf9", "#e4aaf0", "#82dfe2", "#e2ece9",
  "#7e938e", "#f9a8a2", "#aceec4", "#f9e3a4",
  "#a6d7fc", "#eebef7", "#9ce8ea", "#f8fcfb",
]
"##;

#[test]
fn load_palette_resolves_bundled_shiki_user_palettes_dir_and_explicit_file_paths() {
    let env = TestEnv::new();

    // 1. Bundled `shiki` resolves by name when `palettes/` is empty.
    let bundled = load_palette(env.paths(), "shiki").expect("load bundled shiki");
    assert_eq!(bundled.name(), "shiki");
    assert_eq!(bundled.anchors().len(), 24);

    // 2. User palette `<name>.toml` in `$SHIKI_HOME/palettes/` resolves by name.
    let user_path = env.paths().palettes_dir().join("forest-study.toml");
    fs::write(&user_path, VALID_CUSTOM_PALETTE).expect("write user palette");
    let custom = load_palette(env.paths(), "forest-study").expect("load user palette by name");
    assert_eq!(custom.name(), "forest-study");
    assert_eq!(custom.anchors().len(), 2);

    // 3. Explicit `.toml` file path loads directly via both `load_palette` and `load_palette_file`.
    let external_path = env.root().join("standalone.toml");
    fs::write(&external_path, VALID_CUSTOM_PALETTE).expect("write standalone palette");
    let from_spec =
        load_palette(env.paths(), &external_path.to_string_lossy()).expect("load by path spec");
    let from_file = load_palette_file(&external_path).expect("load_palette_file");
    assert_eq!(from_spec, from_file);

    // 4. Missing palette name reports searched path clearly.
    let missing = load_palette(env.paths(), "does-not-exist").unwrap_err();
    match missing {
        StoreError::PaletteNotFound {
            name,
            searched_path,
        } => {
            assert_eq!(name, "does-not-exist");
            assert_eq!(
                searched_path,
                env.paths().palettes_dir().join("does-not-exist.toml")
            );
        }
        other => panic!("expected PaletteNotFound, got {other:?}"),
    }
}

#[test]
fn invalid_user_palette_never_falls_back_and_reports_exact_file_and_line() {
    let env = TestEnv::new();

    // Even if named `shiki.toml` in user's `palettes/` dir, an invalid user override must fail
    // loudly with exact file/line diagnostics rather than silently applying (`PLAN.md` §6).
    let broken_override = env.paths().palettes_dir().join("shiki.toml");
    fs::write(
        &broken_override,
        "name = \"shiki\"\n\n[[anchors]]\nd = 0.0\nbg = \"#nothex\"\nfg = \"#ffffff\"\ncursor = \"#ffffff\"\nselection = \"#222222\"\nansi = [\"#000\",\"#111\",\"#222\",\"#333\",\"#444\",\"#555\",\"#666\",\"#777\",\"#888\",\"#999\",\"#aaa\",\"#bbb\",\"#ccc\",\"#ddd\",\"#eee\",\"#fff\"]\n",
    )
    .expect("write broken user shiki.toml");

    let err = load_palette(env.paths(), "shiki").unwrap_err();
    match err {
        StoreError::PaletteParse(parse_err) => {
            assert_eq!(
                parse_err.source_name(),
                broken_override.display().to_string()
            );
            assert_eq!(parse_err.line(), Some(5));
            assert!(parse_err.message().contains("#nothex"));
        }
        other => panic!("expected StoreError::PaletteParse, got {other:?}"),
    }
}
