use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use crate::archive_dat::{parse_archive_dat_paths, parse_archive_dat_profile};
use crate::decrypt::decrypt_pal_dollar_file;
use crate::error::{AssetError, Result};
use crate::key::{make_pac_key, PacKey};
use crate::nls::Nls;
use crate::pac::PacArchive;
use crate::vfs;

#[derive(Clone, Debug)]
pub enum AssetSource {
    Loose { path: PathBuf },
    Pac { pac_path: PathBuf, key: PacKey },
    /// A translation patch's loose file, kept in the game's own (still
    /// encrypted) form as the patched executable reads it.
    Patch { path: PathBuf },
}

/// A translation patch that shadows the game's archives with loose files,
/// the way patch launchers redirect the executable's file reads.
#[derive(Clone, Debug, Default)]
pub struct GamePatch {
    /// Folders, relative to the game root, searched before every archive.
    pub overlay_dirs: Vec<String>,
    /// The patch's text code map: `TEXT.DAT` keeps its Chinese text in
    /// Shift-JIS-shaped double-byte codes that the patch's font hook maps to
    /// GBK (Longinus `CHS.TBL`, indexed by code). Text read from the patch is
    /// rewritten through it, so it reads as GBK.
    pub text_code_map: Option<Vec<u16>>,
}

#[derive(Clone, Debug)]
pub struct LoadedAsset {
    pub name: String,
    pub bytes: Vec<u8>,
    pub source: AssetSource,
}

#[derive(Debug)]
pub struct ResourceManager {
    root: PathBuf,
    nls: Nls,
    paths: Vec<String>,
    pacs: HashMap<PathBuf, PacArchive>,
    language_archive: Option<String>,
    last_path_wins: bool,
    patch: GamePatch,
    /// The patch folders' files by lower-case name, first folder first.
    patch_files: HashMap<String, PathBuf>,
}

impl ResourceManager {
    pub fn new(root: impl Into<PathBuf>, nls: Nls) -> Self {
        Self {
            root: root.into(),
            nls,
            paths: Vec::new(),
            pacs: HashMap::new(),
            language_archive: None,
            last_path_wins: false,
            patch: GamePatch::default(),
            patch_files: HashMap::new(),
        }
    }

    pub fn bootstrap(root: impl Into<PathBuf>, nls: Nls) -> Result<Self> {
        Self::bootstrap_with_language(root, nls, None)
    }

    /// [`Self::bootstrap`] plus a per-language archive list.
    ///
    /// The FHD releases ship `archive_cn.dat` / `archive_tc.dat` next to
    /// `archive.dat`. They name the localized archives that carry the
    /// translated screens (`system_cn`, `etc_cn`, `movie_cn`, `update_cn`), and
    /// those are meant to shadow the base archives. The language list is added
    /// before the base list for that reason, and a list that does not exist is
    /// not an error, so a single-language install behaves exactly like
    /// [`Self::bootstrap`].
    pub fn bootstrap_with_language(
        root: impl Into<PathBuf>,
        nls: Nls,
        language_archive: Option<&str>,
    ) -> Result<Self> {
        Self::bootstrap_with_path_order(root, nls, language_archive, false)
    }

    /// Newer PalSystemPathSet prepends each path to its native linked list.
    /// Keep that precedence explicit so older release policies stay intact.
    pub fn bootstrap_with_path_order(
        root: impl Into<PathBuf>,
        nls: Nls,
        language_archive: Option<&str>,
        last_path_wins: bool,
    ) -> Result<Self> {
        let mut manager = Self::new(root, nls);
        manager.language_archive = language_archive.map(str::to_owned);
        manager.last_path_wins = last_path_wins;
        manager.select_archive_profile(0)?;
        Ok(manager)
    }

    /// Game.exe set_system_path clears the previous paths before reloading
    /// ARCHIVE.DAT. Missing profiles fail with only the bootstrap path left.
    pub fn select_archive_profile(&mut self, profile: usize) -> Result<()> {
        self.paths.clear();
        self.pacs.clear();
        self.add_path("data");
        let archive = match self.open("archive.dat") {
            Ok(asset) => asset,
            Err(AssetError::AssetNotFound { .. }) => {
                let key = make_pac_key("archive.dat", self.nls)?;
                self.try_open_in_base("csv", "archive.dat", &key)?
                    .ok_or_else(|| AssetError::AssetNotFound { name: "archive.dat".into() })?
            }
            Err(err) => return Err(err),
        };
        let paths = parse_archive_dat_profile(&archive.bytes, self.nls, profile)?;
        if let Some(list) = self.language_archive.clone().filter(|_| !self.last_path_wins) {
            self.add_archive_paths(&list)?;
        }
        for path in paths {
            self.add_path(path);
        }
        if let Some(list) = self.language_archive.clone().filter(|_| self.last_path_wins) {
            self.add_archive_paths(&list)?;
        }
        Ok(())
    }

    /// Applies a translation patch to every later lookup.
    /// The folders are listed once here, so names match whatever their case.
    pub fn set_patch(&mut self, patch: GamePatch) {
        self.patch_files.clear();
        for dir in &patch.overlay_dirs {
            let Ok(entries) = std::fs::read_dir(join_pal_path(&self.root, dir)) else {
                continue;
            };
            for entry in entries.flatten() {
                if entry.file_type().is_ok_and(|kind| kind.is_file()) {
                    let name = entry.file_name().to_string_lossy().to_lowercase();
                    self.patch_files.entry(name).or_insert_with(|| entry.path());
                }
            }
        }
        self.patch = patch;
    }

    pub fn patch(&self) -> &GamePatch {
        &self.patch
    }

    pub fn last_path_wins(&self) -> bool {
        self.last_path_wins
    }

    pub fn set_last_path_wins(&mut self, enabled: bool) {
        self.last_path_wins = enabled;
    }

    /// Append every search path named by an `archive*.dat` list, which is a
    /// `|`-separated list of base names just like `archive.dat`. Returns
    /// `false` when the list itself is absent.
    pub fn add_archive_paths(&mut self, list_name: &str) -> Result<bool> {
        let list = match self.open(list_name) {
            Ok(list) => list,
            Err(AssetError::AssetNotFound { .. }) => return Ok(false),
            Err(err) => return Err(err),
        };
        for path in parse_archive_dat_paths(&list.bytes, self.nls)? {
            self.add_path(path);
        }
        Ok(true)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn nls(&self) -> Nls {
        self.nls
    }

    pub fn paths(&self) -> &[String] {
        &self.paths
    }

    pub fn add_path(&mut self, path: impl AsRef<str>) {
        let normalized = normalize_relative_path(path.as_ref());
        if normalized.is_empty() {
            return;
        }
        if self.last_path_wins {
            // A repeated native registration becomes the newest list node.
            self.paths.retain(|existing| existing != &normalized);
            self.paths.push(normalized);
        } else if !self.paths.iter().any(|existing| existing == &normalized) {
            self.paths.push(normalized);
        }
    }

    pub fn preload_pacs(&mut self) -> Result<()> {
        let paths = self.paths.clone();
        for path in paths {
            let pac_path = self.pac_path_for_base(&path);
            if vfs::is_file(&pac_path) {
                self.ensure_pac_loaded(&pac_path)?;
            }
        }
        Ok(())
    }

    pub fn open(&mut self, name: &str) -> Result<LoadedAsset> {
        let key = make_pac_key(name, self.nls)?;

        if let Some(path) = self.patch_files.get(&name.to_lowercase()).cloned() {
            let bytes = vfs::read_all(&path)?;
            return Ok(LoadedAsset {
                name: name.to_owned(),
                bytes,
                source: AssetSource::Patch { path },
            });
        }

        let mut paths = self.paths.clone();
        if self.last_path_wins {
            paths.reverse();
        }
        for base in paths {
            if let Some(asset) = self.try_open_in_base(&base, name, &key)? {
                return Ok(asset);
            }
        }

        if let Some(asset) = self.try_open_in_root(name, &key)? {
            return Ok(asset);
        }

        Err(AssetError::AssetNotFound {
            name: name.to_owned(),
        })
    }

    pub fn open_decrypting(&mut self, name: &str) -> Result<LoadedAsset> {
        let mut asset = self.open(name)?;
        // Loose files are already decrypted by the PAC unpacker; only PAC-sourced
        // data carries the raw (encrypted) payload and needs the dollar-file transform.
        // A patch's files are in the game's own form, like the PAC payload.
        if matches!(asset.source, AssetSource::Pac { .. } | AssetSource::Patch { .. }) {
            decrypt_pal_dollar_file(name, &mut asset.bytes)?;
        }
        if let (AssetSource::Patch { .. }, Some(map)) = (&asset.source, &self.patch.text_code_map) {
            if name.eq_ignore_ascii_case("text.dat") {
                map_text_list_codes(&mut asset.bytes, map);
            }
        }
        Ok(asset)
    }

    pub fn list_loaded_pacs(&self) -> impl Iterator<Item = &PacArchive> {
        self.pacs.values()
    }

    fn try_open_in_base(
        &mut self,
        base: &str,
        name: &str,
        key: &PacKey,
    ) -> Result<Option<LoadedAsset>> {
        let loose = self.loose_path_for_base(base, name);
        if vfs::is_file(&loose) {
            let bytes = vfs::read_all(&loose)?;
            return Ok(Some(LoadedAsset {
                name: name.to_owned(),
                bytes,
                source: AssetSource::Loose { path: loose },
            }));
        }

        let pac_path = self.pac_path_for_base(base);
        if !vfs::is_file(&pac_path) {
            return Ok(None);
        }
        let pac = self.ensure_pac_loaded(&pac_path)?;
        if let Some(bytes) = pac.read_key(key)? {
            return Ok(Some(LoadedAsset {
                name: name.to_owned(),
                bytes,
                source: AssetSource::Pac {
                    pac_path,
                    key: *key,
                },
            }));
        }
        Ok(None)
    }

    fn try_open_in_root(&mut self, name: &str, key: &PacKey) -> Result<Option<LoadedAsset>> {
        let loose = join_pal_path(&self.root, name);
        if vfs::is_file(&loose) {
            let bytes = vfs::read_all(&loose)?;
            return Ok(Some(LoadedAsset {
                name: name.to_owned(),
                bytes,
                source: AssetSource::Loose { path: loose },
            }));
        }

        let pac_path = append_pac_extension(&self.root);
        if vfs::is_file(&pac_path) {
            let pac = self.ensure_pac_loaded(&pac_path)?;
            if let Some(bytes) = pac.read_key(key)? {
                return Ok(Some(LoadedAsset {
                    name: name.to_owned(),
                    bytes,
                    source: AssetSource::Pac {
                        pac_path,
                        key: *key,
                    },
                }));
            }
        }
        Ok(None)
    }

    fn loose_path_for_base(&self, base: &str, name: &str) -> PathBuf {
        let base_path = join_pal_path(&self.root, base);
        join_pal_path(&base_path, name)
    }

    fn pac_path_for_base(&self, base: &str) -> PathBuf {
        append_pac_extension(&join_pal_path(&self.root, base))
    }

    fn ensure_pac_loaded(&mut self, pac_path: &Path) -> Result<&PacArchive> {
        if !self.pacs.contains_key(pac_path) {
            let pac = PacArchive::from_file(pac_path)?;
            self.pacs.insert(pac_path.to_path_buf(), pac);
        }
        Ok(self
            .pacs
            .get(pac_path)
            .expect("pac cache entry was just inserted"))
    }
}

fn normalize_relative_path(path: &str) -> String {
    path.trim_matches(|c| c == '\\' || c == '/')
        .replace('/', "\\")
}

fn join_pal_path(base: &Path, pal_path: &str) -> PathBuf {
    let mut out = base.to_path_buf();
    for part in pal_path.split(|c| c == '\\' || c == '/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            out.pop();
            continue;
        }
        out.push(part);
    }
    out
}

fn append_pac_extension(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".pac");
    PathBuf::from(s)
}

#[allow(dead_code)]
fn has_parent_dir(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::ParentDir))
}

/// Rewrites the double-byte codes of a `$TEXT_LIST__` file's strings through
/// `map` (indexed by code; 0 keeps the code). Records are a 4-byte id and a
/// NUL-terminated string after the 16-byte header. A lead byte is any byte
/// from 0x80 to 0xFE, as the patched executable takes them; every code keeps
/// its two bytes, so record offsets do not move.
pub fn map_text_list_codes(bytes: &mut [u8], map: &[u16]) {
    if !bytes.starts_with(b"$TEXT_LIST__") {
        return;
    }
    let mut position = 0x10;
    while position + 4 < bytes.len() {
        position += 4;
        while position < bytes.len() && bytes[position] != 0 {
            let lead = bytes[position];
            if (0x80..=0xFE).contains(&lead) && position + 1 < bytes.len() && bytes[position + 1] != 0 {
                let code = u16::from_be_bytes([lead, bytes[position + 1]]);
                if let Some(&mapped) = map.get(usize::from(code)).filter(|&&mapped| mapped > 0xFF) {
                    bytes[position..position + 2].copy_from_slice(&mapped.to_be_bytes());
                }
                position += 2;
            } else {
                position += 1;
            }
        }
        position += 1;
    }
}

#[cfg(test)]
mod patch_tests {
    use super::map_text_list_codes;

    #[test]
    fn maps_double_byte_codes_in_place() {
        let mut map = vec![0u16; 0xF000];
        map[0x8140] = 0xA1A1;
        map[0x889F] = 0xB0A1;
        let mut bytes = b"$TEXT_LIST__\x02\0\0\0".to_vec();
        bytes.extend_from_slice(&[0, 0, 0, 0, b'A', 0x81, 0x40, 0x88, 0x9F, 0]);
        bytes.extend_from_slice(&[1, 0, 0, 0, 0x88, 0x9F, 0]);
        map_text_list_codes(&mut bytes, &map);
        assert_eq!(&bytes[0x14..0x1A], &[b'A', 0xA1, 0xA1, 0xB0, 0xA1, 0]);
        assert_eq!(&bytes[0x1E..0x21], &[0xB0, 0xA1, 0]);
    }
}
