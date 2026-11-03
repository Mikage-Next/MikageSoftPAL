//! What the game folder holds besides the original release: translation
//! patches and the localized archives of the official Chinese releases.
use std::path::Path;

use pal_asset::Nls;

#[derive(Debug, Default)]
pub struct Found {
    /// The text encoding the patch implies, when it implies one.
    pub nls: Option<Nls>,
}

pub fn detect(_root: &Path) -> Found {
    Found::default()
}

/// The official FHD Chinese releases keep their localized archives loose at
/// the root (`system_cn.pac`, `etc_cn.pac`, ...).
pub fn has_chinese_archives(root: &Path) -> bool {
    std::fs::read_dir(root)
        .map(|entries| {
            entries.filter_map(Result::ok).any(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.eq_ignore_ascii_case("system_cn.pac"))
            })
        })
        .unwrap_or(false)
}
