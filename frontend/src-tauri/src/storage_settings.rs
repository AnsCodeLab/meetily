//! User-configurable storage location for downloaded AI models (Whisper, Parakeet,
//! and built-in summary models). Mirrors the pattern used by `audio::recording_preferences`
//! for the meeting recordings folder: a small persisted override on top of a sane
//! platform default, plus commands to pick a new location and move existing files there.

use anyhow::{anyhow, Result};
use log::{info, warn};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_store::StoreExt;

use crate::{parakeet_engine, summary, whisper_engine};

const STORE_FILE: &str = "storage_settings.json";
const MODELS_DIR_KEY: &str = "models_dir";

/// The unified models root Meetily uses when no custom location has been configured:
/// `<app_data_dir>/models`. Whisper models live directly in this directory, while
/// Parakeet and the summary engine each use a `parakeet/` and `summary/` subdirectory.
pub fn default_models_root<R: Runtime>(app: &AppHandle<R>) -> PathBuf {
    app.path()
        .app_data_dir()
        .expect("Failed to get app data dir")
        .join("models")
}

/// Resolve the models root currently in effect: the persisted custom location if one
/// was configured, otherwise the default.
pub fn resolve_models_root<R: Runtime>(app: &AppHandle<R>) -> PathBuf {
    let default = default_models_root(app);

    let store = match app.store(STORE_FILE) {
        Ok(store) => store,
        Err(e) => {
            warn!(
                "Failed to access storage settings store: {}, using default models dir",
                e
            );
            return default;
        }
    };

    match store.get(MODELS_DIR_KEY) {
        Some(value) => match serde_json::from_value::<String>(value.clone()) {
            Ok(path_str) if !path_str.trim().is_empty() => PathBuf::from(path_str),
            _ => default,
        },
        None => default,
    }
}

fn persist_models_root<R: Runtime>(app: &AppHandle<R>, dir: &Path) -> Result<()> {
    let store = app
        .store(STORE_FILE)
        .map_err(|e| anyhow!("Failed to access storage settings store: {}", e))?;

    store.set(
        MODELS_DIR_KEY,
        serde_json::Value::String(dir.to_string_lossy().to_string()),
    );
    store
        .save()
        .map_err(|e| anyhow!("Failed to persist storage settings: {}", e))?;

    Ok(())
}

/// Move every entry from `old_root` into `new_root`, merging directories and never
/// clobbering a file/directory that already exists at the destination. Falls back to
/// copy+delete when `rename` fails (e.g. moving across filesystems/drives).
fn migrate_directory_contents(old_root: &Path, new_root: &Path) -> Result<()> {
    if !old_root.exists() || old_root == new_root {
        return Ok(());
    }

    std::fs::create_dir_all(new_root)?;

    for entry in std::fs::read_dir(old_root)? {
        let entry = entry?;
        let src = entry.path();
        let dest = new_root.join(entry.file_name());

        if dest.exists() {
            warn!(
                "Skipping migration of {:?}: destination already exists at {:?}",
                src, dest
            );
            continue;
        }

        match std::fs::rename(&src, &dest) {
            Ok(()) => info!("Moved {:?} -> {:?}", src, dest),
            Err(_) => {
                // Cross-filesystem move (e.g. different drive): copy then remove the source.
                copy_recursive(&src, &dest)?;
                if src.is_dir() {
                    std::fs::remove_dir_all(&src)?;
                } else {
                    std::fs::remove_file(&src)?;
                }
                info!("Copied {:?} -> {:?} (cross-filesystem)", src, dest);
            }
        }
    }

    // Best-effort cleanup: remove the old root if migration emptied it. Ignore failure
    // (e.g. non-empty because a file was skipped above).
    let _ = std::fs::remove_dir(old_root);

    Ok(())
}

fn copy_recursive(src: &Path, dest: &Path) -> Result<()> {
    if src.is_dir() {
        std::fs::create_dir_all(dest)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &dest.join(entry.file_name()))?;
        }
    } else {
        std::fs::copy(src, dest)?;
    }
    Ok(())
}

// ============================================================================
// Tauri commands
// ============================================================================

#[tauri::command]
pub async fn get_models_root_directory<R: Runtime>(app: AppHandle<R>) -> Result<String, String> {
    Ok(resolve_models_root(&app).to_string_lossy().to_string())
}

#[tauri::command]
pub async fn get_default_models_root_directory<R: Runtime>(
    app: AppHandle<R>,
) -> Result<String, String> {
    Ok(default_models_root(&app).to_string_lossy().to_string())
}

/// Open a native folder picker for the new models storage location.
#[tauri::command]
pub async fn select_models_directory<R: Runtime>(
    app: AppHandle<R>,
) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;

    info!("Opening dialog to select models storage location");

    let folder = app.dialog().file().blocking_pick_folder();
    Ok(folder.map(|p| p.to_string()))
}

/// Move all downloaded models (Whisper, Parakeet, summary) to `new_dir`, persist the
/// choice, and re-point every model engine at the new location. No app restart needed.
#[tauri::command]
pub async fn change_models_directory<R: Runtime>(
    app: AppHandle<R>,
    new_dir: String,
) -> Result<String, String> {
    let new_root = PathBuf::from(&new_dir);
    let old_root = resolve_models_root(&app);

    if new_root == old_root {
        return Ok(old_root.to_string_lossy().to_string());
    }

    if new_root.starts_with(&old_root) {
        return Err("The new location cannot be inside the current models folder".to_string());
    }

    // GGUF/ONNX model weights can be multiple gigabytes; run the move off the async
    // runtime's worker threads so it never stalls other Tauri commands.
    let old_for_move = old_root.clone();
    let new_for_move = new_root.clone();
    tokio::task::spawn_blocking(move || migrate_directory_contents(&old_for_move, &new_for_move))
        .await
        .map_err(|e| format!("Migration task panicked: {}", e))?
        .map_err(|e| format!("Failed to move existing models: {}", e))?;

    persist_models_root(&app, &new_root).map_err(|e| e.to_string())?;

    whisper_engine::commands::reinit_with_dir(new_root.clone())
        .map_err(|e| format!("Failed to reinitialize Whisper engine: {}", e))?;
    parakeet_engine::commands::reinit_with_dir(new_root.clone())
        .map_err(|e| format!("Failed to reinitialize Parakeet engine: {}", e))?;
    summary::summary_engine::commands::init_model_manager(&app)
        .await
        .map_err(|e| format!("Failed to reinitialize summary model manager: {}", e))?;

    info!("Models directory changed: {:?} -> {:?}", old_root, new_root);
    Ok(new_root.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_moves_files_and_nested_subdirectories() {
        let old = tempfile::tempdir().unwrap();
        let new = tempfile::tempdir().unwrap();

        std::fs::write(old.path().join("ggml-base.bin"), b"whisper model").unwrap();
        std::fs::create_dir_all(old.path().join("parakeet")).unwrap();
        std::fs::write(old.path().join("parakeet").join("encoder.onnx"), b"parakeet model").unwrap();
        std::fs::create_dir_all(old.path().join("summary")).unwrap();
        std::fs::write(old.path().join("summary").join("qwen.gguf"), b"summary model").unwrap();

        migrate_directory_contents(old.path(), new.path()).unwrap();

        assert_eq!(
            std::fs::read(new.path().join("ggml-base.bin")).unwrap(),
            b"whisper model"
        );
        assert_eq!(
            std::fs::read(new.path().join("parakeet").join("encoder.onnx")).unwrap(),
            b"parakeet model"
        );
        assert_eq!(
            std::fs::read(new.path().join("summary").join("qwen.gguf")).unwrap(),
            b"summary model"
        );
        // Old root should be cleaned up once emptied by the move.
        assert!(!old.path().exists());
    }

    #[test]
    fn migrate_never_clobbers_an_existing_destination_file() {
        let old = tempfile::tempdir().unwrap();
        let new = tempfile::tempdir().unwrap();

        std::fs::write(old.path().join("ggml-base.bin"), b"new download").unwrap();
        std::fs::write(new.path().join("ggml-base.bin"), b"already there").unwrap();

        migrate_directory_contents(old.path(), new.path()).unwrap();

        // Destination file must be untouched; source is left behind rather than lost.
        assert_eq!(
            std::fs::read(new.path().join("ggml-base.bin")).unwrap(),
            b"already there"
        );
        assert_eq!(
            std::fs::read(old.path().join("ggml-base.bin")).unwrap(),
            b"new download"
        );
    }

    #[test]
    fn migrate_is_a_noop_when_old_root_does_not_exist() {
        let old = tempfile::tempdir().unwrap();
        let missing = old.path().join("never-created");
        let new = tempfile::tempdir().unwrap();

        migrate_directory_contents(&missing, new.path()).unwrap();

        assert!(std::fs::read_dir(new.path()).unwrap().next().is_none());
    }
}
