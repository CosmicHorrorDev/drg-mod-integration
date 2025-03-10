use std::path::{Path, PathBuf};

use anyhow::Result;

use serde::{de::DeserializeOwned, Serialize};

/// Wrapper around an object that is read from a file on init and written on drop
pub struct ConfigWrapper<C: Default + Serialize + DeserializeOwned> {
    path: PathBuf,
    config: C,
}
impl<C: Default + Serialize + DeserializeOwned> ConfigWrapper<C> {
    pub fn new<P: AsRef<Path>>(path: P) -> Self {
        Self {
            config: std::fs::read(&path)
                .ok()
                .and_then(|s| serde_json::from_slice(&s).ok())
                .unwrap_or_default(),
            path: path.as_ref().to_path_buf(),
        }
    }
    pub fn save(&self) -> Result<()> {
        std::fs::write(&self.path, serde_json::to_vec_pretty(&self.config)?)?;
        Ok(())
    }
}
impl<C: Default + Serialize + DeserializeOwned> std::ops::Deref for ConfigWrapper<C> {
    type Target = C;
    fn deref(&self) -> &Self::Target {
        &self.config
    }
}
impl<C: Default + Serialize + DeserializeOwned> std::ops::DerefMut for ConfigWrapper<C> {
    fn deref_mut(&mut self) -> &mut C {
        &mut self.config
    }
}
impl<C: Default + Serialize + DeserializeOwned> Drop for ConfigWrapper<C> {
    fn drop(&mut self) {
        self.save().unwrap();
    }
}
