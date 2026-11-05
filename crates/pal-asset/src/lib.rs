//! Asset loading support for the reversed PAL engine.
//!
//! The base search path comes from `archive.dat`. [`ResourceManager::bootstrap_with_language`]
//! additionally applies the `archive_cn.dat` / `archive_tc.dat` path lists that the localized
//! FHD releases ship, so their `system_cn` / `etc_cn` / `movie_cn` / `update_cn` archives are
//! searched ahead of the base ones.
//! External asset names are accepted as UTF-8 strings. The selected [`Nls`] controls how
//! those names are encoded into the original byte-level PAL lookup keys.

mod archive_dat;
mod decrypt;
mod error;
mod key;
mod moe;
mod nls;
mod pac;
mod resource;
pub mod vfs;

pub use archive_dat::{parse_archive_dat_paths, parse_archive_dat_profile};
pub use decrypt::{decrypt_dollar_payload, decrypt_pal_dollar_file};
pub use error::{AssetError, Result};
pub use key::{make_pac_key, normalize_pal_name_bytes, PacKey};
pub use moe::{
    decrypt_index, decrypt_resource_body, lz_decode, name_hash, parse_index_records, probe,
    read_entry, MoeArchive,
    MoeCipher, MoeEntry, MoeError, MoeHeader, MoeIndex, MoeLayout, MoeProbe, MoeRecord,
    MoeResource, MoeSchema, MoeTables, MoeTextTable, XmoeCipher, MOE_BLOCK_SIZE,
    MOE_HEADER_SIZE, MOE_MAGIC, MOE_TABLE_MAGIC, MOE_TABLE_VERSION, MOE_TEXT_CODEPAGE,
};
pub use nls::Nls;
pub use pac::{PacArchive, PacBucket, PacEntry};
pub use resource::{map_text_list_codes, AssetSource, GamePatch, LoadedAsset, ResourceManager};
