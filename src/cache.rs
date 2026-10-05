use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::index::IndexData;
use crate::workspace::Derived;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    pub path: String,
    pub size: u64,
    pub modified_ns: u128,
}

impl Stamp {
    pub fn of(path: &Path) -> Option<Stamp> {
        let meta = std::fs::metadata(path).ok()?;
        let modified_ns = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_nanos();
        Some(Stamp { path: path.to_string_lossy().into_owned(), size: meta.len(), modified_ns })
    }
}

#[derive(Default, Serialize, Deserialize)]
pub struct CacheFile {
    pub format: String,
    pub index: Option<(Stamp, IndexData)>,
    pub entries: Vec<(u64, Arc<Derived>)>,
}

pub fn default_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("SCRATCHPILERD_CACHE_DIR") {
        return Some(PathBuf::from(dir));
    }
    let home = || std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from);
    if cfg!(windows) {
        return std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("scratchpilerd").join("cache"));
    }
    if cfg!(target_os = "macos") {
        return home().map(|h| h.join("Library/Caches/scratchpilerd"));
    }
    std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).or_else(|| home().map(|h| h.join(".cache"))).map(|d| d.join("scratchpilerd"))
}

pub fn file_for(dir: &Path, root: &Path) -> PathBuf {
    let hash = xxhash_rust::xxh3::xxh3_64(root.to_string_lossy().as_bytes());
    dir.join(format!("{hash:016x}.bin"))
}

pub fn load(path: &Path) -> Option<CacheFile> {
    let bytes = std::fs::read(path).ok()?;
    let file: CacheFile = bincode::deserialize(&bytes).ok()?;
    (file.format == crate::CACHE_FORMAT).then_some(file)
}

pub fn store(path: &Path, file: &CacheFile) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let bytes = bincode::serialize(file).map_err(std::io::Error::other)?;
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}
