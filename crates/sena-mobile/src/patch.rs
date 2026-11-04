//! What the game folder holds besides the original release: translation
//! patches and the localized archives of the official Chinese releases.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use pal_asset::{GamePatch, MoeTables, Nls};

use crate::log;

#[derive(Debug, Default)]
pub struct Found {
    /// The text encoding the patch implies, when it implies one.
    pub nls: Option<Nls>,
    /// The patch's files and text code map.
    pub patch: Option<GamePatch>,
    /// An encrypted `.moe` container beside the game. Only registered here:
    /// the host must supply a `MoeCipher` before it can be read.
    pub moe_container: Option<PathBuf>,
    /// Algorithm tables exported by the user into `moe-tables.bin`. Absent
    /// when the file is missing or fails validation; the runtime never ships
    /// these bytes itself.
    pub moe_tables: Option<MoeTables>,
}

/// Reads the translation patch in the game folder, if there is one the
/// runtime understands.
pub fn detect(root: &Path) -> Found {
    if let Some(ini) = find_file(root, "Longinus.ini") {
        match read_longinus(root, &ini) {
            Some(found) => return found,
            None => log::emit(log::WARN, "Longinus.ini redirects no folder; the patch is ignored"),
        }
    }
    detect_xmoe(root)
}

/// A `.moe` container with the patch's compiler or launcher beside it is an
/// encrypted translation patch. Its SYSTEM.INI is GBK, and its loose files
/// (notably the subtitled opening movie) shadow the archives. The container
/// is only registered: this crate ships neither a cipher nor a key, and the
/// host decides whether it is entitled to decrypt it.
fn detect_xmoe(root: &Path) -> Found {
    let Some(container) = find_moe_container(root) else {
        return Found::default();
    };
    let has_runtime = find_file(root, "XmoeCompiler.dll").is_some()
        || find_file(root, "Koikake_chs.exe").is_some();
    if !has_runtime {
        return Found::default();
    }
    let mut overlay_dirs = Vec::new();
    if find_dir(root, "movie").is_some() {
        overlay_dirs.push("movie".to_owned());
    }
    let moe_tables = find_file(root, "moe-tables.bin").and_then(|path| {
        match MoeTables::read(&path) {
            Ok(tables) => Some(tables),
            Err(err) => {
                log::emit(
                    log::WARN,
                    &format!("moe tables at {} are unusable: {err:?}", path.display()),
                );
                None
            }
        }
    });
    log::emit(
        log::INFO,
        &format!(
            "moe patch: container {}, overlays {:?}, host tables {}",
            container.display(),
            overlay_dirs,
            if moe_tables.is_some() { "loaded" } else { "absent" }
        ),
    );
    Found {
        nls: Some(Nls::Gbk),
        patch: Some(GamePatch {
            overlay_dirs,
            text_code_map: None,
        }),
        moe_container: Some(container),
        moe_tables,
    }
}

fn find_moe_container(root: &Path) -> Option<PathBuf> {
    std::fs::read_dir(root).ok()?.flatten().find_map(|entry| {
        let path = entry.path();
        let is_moe = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("moe"));
        (path.is_file() && is_moe).then_some(path)
    })
}

/// Longinus (爱丽丝后宫汉化组 and others) hooks the executable's Win32 calls
/// as `Longinus.ini` says:
///
/// - `[CreateFileA]` redirects files, here `data\ARCHIVE.DAT` to the patch
///   folder's copy, whose archive list ends with that folder; with
///   `Use_Directory=1` its loose files (script, text, images) shadow the
///   archives.
/// - `[GetGlyphOutline]` `TBL_MAPPED=1` maps each double-byte code the game
///   draws through the `TBL` file to a GBK code.
/// - `[MultiByteToWideChar]` `CodePage=936`: the rest of the text is GBK.
fn read_longinus(root: &Path, ini: &Path) -> Option<Found> {
    let bytes = std::fs::read(ini).ok()?;
    let sections = parse_ini(&decode_ini(&bytes));
    let get = |section: &str, key: &str| {
        sections
            .get(&section.to_ascii_lowercase())
            .and_then(|keys| keys.get(&key.to_ascii_lowercase()))
            .map(String::as_str)
    };

    let mut overlay_dirs = Vec::new();
    let count = get("CreateFileA", "Count").and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
    for i in 1..=count {
        let Some(new_name) = get("CreateFileA", &format!("NewFileName{i}")) else {
            continue;
        };
        let Some((dir, _)) = new_name.rsplit_once(['\\', '/']) else {
            continue;
        };
        if let Some(found) = find_dir(root, dir) {
            let name = found.file_name()?.to_string_lossy().into_owned();
            if !overlay_dirs.contains(&name) {
                overlay_dirs.push(name);
            }
        }
    }
    if overlay_dirs.is_empty() {
        return None;
    }

    let text_code_map = (get("GetGlyphOutline", "TBL_MAPPED") == Some("1"))
        .then(|| get("GetGlyphOutline", "TBL"))
        .flatten()
        .and_then(|name| find_file(root, name))
        .and_then(|path| std::fs::read(path).ok())
        .map(|table| {
            table
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect::<Vec<_>>()
        });
    let nls = match get("MultiByteToWideChar", "CodePage") {
        Some("936") => Some(Nls::Gbk),
        _ if text_code_map.is_some() => Some(Nls::Gbk),
        _ => None,
    };
    log::emit(
        log::INFO,
        &format!(
            "Longinus patch: folders {overlay_dirs:?}, code map {}, encoding {:?}",
            text_code_map.as_ref().map_or(0, Vec::len),
            nls.map(|nls| nls.name())
        ),
    );
    Some(Found {
        nls,
        patch: Some(GamePatch {
            overlay_dirs,
            text_code_map,
        }),
        moe_container: None,
        moe_tables: None,
    })
}

/// Longinus.ini is UTF-16LE with a byte order mark; older ones are GBK.
fn decode_ini(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest.chunks_exact(2).map(|p| u16::from_le_bytes([p[0], p[1]])).collect();
        return String::from_utf16_lossy(&units);
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.trim_start_matches('\u{feff}').to_owned();
    }
    encoding_rs::GBK.decode(bytes).0.into_owned()
}

/// Sections and keys by lower-case name; `;` starts a comment line.
fn parse_ini(text: &str) -> HashMap<String, HashMap<String, String>> {
    let mut sections: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut current = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = name.trim().to_ascii_lowercase();
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            sections
                .entry(current.clone())
                .or_default()
                .insert(key.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    sections
}

fn find_entry(root: &Path, name: &str, dir: bool) -> Option<PathBuf> {
    std::fs::read_dir(root).ok()?.filter_map(Result::ok).find_map(|entry| {
        let kind = entry.file_type().ok()?;
        (kind.is_dir() == dir && entry.file_name().to_str()?.eq_ignore_ascii_case(name))
            .then(|| entry.path())
    })
}

fn find_file(root: &Path, name: &str) -> Option<PathBuf> {
    find_entry(root, name, false)
}

fn find_dir(root: &Path, name: &str) -> Option<PathBuf> {
    find_entry(root, name, true)
}

/// The official FHD Chinese releases keep their localized archives loose at
/// the root (`system_cn.pac`, `etc_cn.pac`, ...).
pub fn has_chinese_archives(root: &Path) -> bool {
    find_file(root, "system_cn.pac").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_longinus_redirect_and_code_map() {
        let root = std::env::temp_dir().join(format!("sena-mobile-longinus-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("GAME_CHS")).unwrap();
        let ini = "[CreateFileA]\r\nCount=1\r\nUse_Directory=1\r\nOldFileName1=data\\ARCHIVE.DAT\r\n\
                   NewFileName1=GAME_CHS\\ARCHIVE.DAT\r\n[MultiByteToWideChar]\r\nCodePage=936\r\n\
                   [GetGlyphOutline]\r\n;comment\r\nTBL_MAPPED=1\r\nTBL=chs.tbl\r\n";
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend(ini.encode_utf16().flat_map(u16::to_le_bytes));
        std::fs::write(root.join("Longinus.ini"), bytes).unwrap();
        std::fs::write(root.join("CHS.TBL"), [0x20, 0, 0xA1, 0xA1]).unwrap();
        let found = detect(&root);
        assert_eq!(found.nls, Some(Nls::Gbk));
        let patch = found.patch.unwrap();
        assert_eq!(patch.overlay_dirs, vec!["GAME_CHS".to_owned()]);
        assert_eq!(patch.text_code_map, Some(vec![0x20, 0xA1A1]));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn detects_an_encrypted_moe_patch_without_decrypting_it() {
        let root = std::env::temp_dir().join(format!("sena-mobile-moe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("movie")).unwrap();
        std::fs::write(root.join("Sena.moe"), [0u8; 32]).unwrap();
        std::fs::write(root.join("XmoeCompiler.dll"), b"").unwrap();
        std::fs::write(root.join("movie/koikake.wmv"), b"").unwrap();
        let found = detect(&root);
        assert_eq!(found.nls, Some(Nls::Gbk));
        let patch = found.patch.unwrap();
        assert_eq!(patch.overlay_dirs, vec!["movie".to_owned()]);
        assert!(patch.text_code_map.is_none());
        assert!(found.moe_tables.is_none());
        assert_eq!(
            found
                .moe_container
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())),
            Some("Sena.moe".to_owned())
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn loads_host_supplied_moe_tables() {
        let root = std::env::temp_dir()
            .join(format!("sena-mobile-moe-tables-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Sena.moe"), [0u8; 32]).unwrap();
        std::fs::write(root.join("XmoeCompiler.dll"), b"").unwrap();
        let tables = MoeTables {
            sbox: (0..=255u8).collect(),
            nibble: (0..16u8).collect(),
            keys: vec![0x1122_3344, 0x5566_7788],
        };
        std::fs::write(root.join("moe-tables.bin"), tables.encode().unwrap()).unwrap();
        let found = detect(&root);
        let loaded = found.moe_tables.expect("host table file should load");
        assert_eq!(loaded.sbox.len(), 256);
        assert_eq!(loaded.nibble.len(), 16);
        assert_eq!(loaded.keys, tables.keys);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn ignores_a_corrupt_moe_tables_file() {
        let root = std::env::temp_dir()
            .join(format!("sena-mobile-moe-badtables-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Sena.moe"), [0u8; 32]).unwrap();
        std::fs::write(root.join("XmoeCompiler.dll"), b"").unwrap();
        std::fs::write(root.join("moe-tables.bin"), b"not a table").unwrap();
        let found = detect(&root);
        assert!(found.moe_tables.is_none());
        assert!(found.moe_container.is_some());
        let _ = std::fs::remove_dir_all(&root);
    }
}
