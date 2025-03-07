pub mod file;
pub mod http;
pub mod modio;

use crate::config::ConfigWrapper;
use crate::error::IntegrationError;

use anyhow::{anyhow, Result};

use serde::{Deserialize, Serialize};

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

pub struct ModStore {
    providers: HashMap<&'static str, Box<dyn ModProvider>>,
    cache: Arc<RwLock<ConfigWrapper<Cache>>>,
    blob_cache: BlobCache,
}
impl ModStore {
    pub fn new<P: AsRef<Path>>(
        cache_path: P,
        parameters: &HashMap<String, HashMap<String, String>>,
    ) -> Result<Self> {
        let factories = inventory::iter::<ProviderFactory>()
            .map(|f| (f.id, f))
            .collect::<HashMap<_, _>>();
        let providers = parameters
            .iter()
            .flat_map(|(id, params)| {
                factories
                    .get(id.as_str())
                    .ok_or_else(|| anyhow!("unknown provider: {id}"))
                    .map(|f| (f.new)(params).map(|p| (f.id, p)))
            })
            .collect::<Result<HashMap<_, _>>>()?;

        Ok(Self {
            providers,
            cache: Arc::new(RwLock::new(ConfigWrapper::new(
                cache_path.as_ref().join("cache.json"),
            ))),
            blob_cache: BlobCache::new(cache_path.as_ref().join("blobs")),
        })
    }
    pub fn add_provider(
        &mut self,
        provider_factory: &ProviderFactory,
        parameters: &HashMap<String, String>,
    ) -> Result<()> {
        let provider = (provider_factory.new)(parameters)?;
        self.providers.insert(provider_factory.id, provider);
        Ok(())
    }
    pub fn get_provider(&self, url: &str) -> Result<&dyn ModProvider> {
        let factory = inventory::iter::<ProviderFactory>()
            .find(|f| (f.can_provide)(url.to_owned()))
            .ok_or_else(|| anyhow!("Could not find mod provider for {}", url))?;
        let entry = self.providers.get(factory.id);
        Ok(match entry {
            Some(e) => e.as_ref(),
            None => {
                return Err(IntegrationError::NoProvider {
                    url: url.to_owned(),
                    factory,
                }
                .into())
            }
        })
    }
    pub async fn resolve_mods(
        &mut self,
        mods: &[String],
        update: bool,
    ) -> Result<HashMap<String, Mod>> {
        use futures::stream::{self, StreamExt, TryStreamExt};

        let mut to_resolve = mods.iter().cloned().collect::<HashSet<String>>();
        let mut mods_map = HashMap::new();

        while !to_resolve.is_empty() {
            for (u, m) in stream::iter(
                to_resolve
                    .iter()
                    .map(|u| self.get_mod(u.to_owned(), update)),
            )
            .buffered(5)
            .try_collect::<Vec<_>>()
            .await?
            {
                mods_map.insert(u, m);
                to_resolve.clear();
                for m in mods_map.values() {
                    for d in &m.suggested_dependencies {
                        if !mods_map.contains_key(d) {
                            to_resolve.insert(d.to_string());
                        }
                    }
                }
            }
        }

        Ok(mods_map)
    }
    pub async fn get_mod(&self, original_url: String, update: bool) -> Result<(String, Mod)> {
        let mut url = original_url.clone();
        loop {
            match self
                .get_provider(&url)?
                .get_mod(&url, update, self.cache.clone(), &self.blob_cache.clone())
                .await?
            {
                ModResponse::Resolve(m) => {
                    return Ok((original_url, m));
                }
                ModResponse::Redirect {
                    url: redirected_url,
                } => url = redirected_url,
            };
        }
    }
}

pub trait ReadSeek: Read + Seek + Send {}
impl<T: Seek + Read + Send> ReadSeek for T {}

/// Whether a mod can be resolved by clients or not
#[derive(Debug, Clone)]
pub enum ResolvableStatus {
    /// If a mod can not be resolved, specify just a name
    Unresolvable { name: String },
    /// Ifa mod can be resolved, specify the URL
    Resolvable { url: String },
}

/// Returned from ModStore
#[derive(Debug, Clone)]
pub struct Mod {
    pub status: ResolvableStatus,
    pub path: PathBuf,
    pub suggested_require: bool,
    pub suggested_dependencies: Vec<String>, // ModResponse
}

/// Returned from ModProvider
#[derive(Debug, Clone)]
pub enum ModResponse {
    Redirect { url: String },
    Resolve(Mod),
}

#[async_trait::async_trait]
pub trait ModProvider: Sync + std::fmt::Debug {
    async fn get_mod(
        &self,
        url: &str,
        update: bool,
        cache: Arc<RwLock<ConfigWrapper<Cache>>>,
        blob_cache: &BlobCache,
    ) -> Result<ModResponse>;
}

#[derive(Clone)]
pub struct ProviderFactory {
    pub id: &'static str,
    #[allow(clippy::type_complexity)]
    new: fn(&HashMap<String, String>) -> Result<Box<dyn ModProvider>>,
    can_provide: fn(String) -> bool,
    pub parameters: &'static [ProviderParameter<'static>],
}

impl std::fmt::Debug for ProviderFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderFactory")
            .field("id", &self.id)
            .field("parameters", &self.parameters)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct ProviderParameter<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub description: &'a str,
}

#[typetag::serde(tag = "type")]
pub trait ModProviderCache: Sync + Send + std::fmt::Debug {
    fn new() -> Self
    where
        Self: Sized;
    fn as_any(&self) -> &dyn std::any::Any;
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Cache(HashMap<String, Box<dyn ModProviderCache>>);
impl Cache {
    fn has<T: ModProviderCache + 'static>(&self, id: &str) -> bool {
        self.0
            .get(id)
            .and_then(|c| c.as_any().downcast_ref::<T>())
            .is_none()
    }
    fn get<T: ModProviderCache + 'static>(&self, id: &str) -> Option<&T> {
        self.0.get(id).and_then(|c| c.as_any().downcast_ref::<T>())
    }
    fn get_mut<T: ModProviderCache + 'static>(&mut self, id: &str) -> &mut T {
        if self.has::<T>(id) {
            self.0.insert(id.to_owned(), Box::new(T::new()));
        }
        self.0
            .get_mut(id)
            .and_then(|c| c.as_any_mut().downcast_mut::<T>())
            .unwrap()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BlobRef(String);

#[derive(Debug, Clone)]
pub struct BlobCache {
    path: PathBuf,
}
impl BlobCache {
    fn new<P: AsRef<Path>>(path: P) -> Self {
        std::fs::create_dir(&path).ok();
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }
    fn write(&self, blob: &[u8]) -> Result<BlobRef> {
        use sha2::{Digest, Sha256};

        let mut hasher = Sha256::new();
        hasher.update(blob);
        let hash = hex::encode(hasher.finalize());

        let tmp = self.path.join(format!(".{hash}"));
        std::fs::write(&tmp, blob)?;
        std::fs::rename(tmp, self.path.join(&hash))?;

        Ok(BlobRef(hash))
    }
    fn read(&self, blob: &BlobRef) -> Result<Box<dyn ReadSeek>> {
        // TODO verify hash, custom reader that hashes as it's read?
        Ok(Box::new(BufReader::new(File::open(
            self.path.join(&blob.0),
        )?)))
    }
    fn get_path(&self, blob: &BlobRef) -> Option<PathBuf> {
        let path = self.path.join(&blob.0);
        path.exists().then_some(path)
    }
}

inventory::collect!(ProviderFactory);
