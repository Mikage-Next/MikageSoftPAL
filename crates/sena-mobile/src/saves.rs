//! The host's save folder. The original keeps its saves in `<game>/save`;
//! a host that keeps them elsewhere gets the ones already there copied over
//! once, so saves made with the Windows engine still load.
use std::fs;
use std::io;
use std::path::Path;

/// Creates `save_dir`. When it is new and the game folder has the
/// original's `save` folder, copies that folder's contents into it first.
pub fn adopt_original(game_dir: &Path, save_dir: &Path) -> io::Result<()> {
    let original = game_dir.join("save");
    let fresh = !save_dir.exists();
    fs::create_dir_all(save_dir)?;
    if fresh && original.is_dir() && original != save_dir {
        copy_tree(&original, save_dir)?;
    }
    Ok(())
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            fs::create_dir_all(&target)?;
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_original_saves_into_a_new_folder_only() {
        let root = std::env::temp_dir().join(format!("sena-mobile-saves-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("save/sena_rs")).unwrap();
        fs::write(root.join("save/save000.dat"), b"one").unwrap();
        fs::write(root.join("save/sena_rs/system.ini"), b"ini").unwrap();
        let saves = root.join("savedata");
        adopt_original(&root, &saves).unwrap();
        assert_eq!(fs::read(saves.join("save000.dat")).unwrap(), b"one");
        assert_eq!(fs::read(saves.join("sena_rs/system.ini")).unwrap(), b"ini");

        // Once the folder exists, its saves win.
        fs::write(saves.join("save000.dat"), b"mine").unwrap();
        fs::write(root.join("save/save000.dat"), b"two").unwrap();
        adopt_original(&root, &saves).unwrap();
        assert_eq!(fs::read(saves.join("save000.dat")).unwrap(), b"mine");
        let _ = fs::remove_dir_all(&root);
    }
}
