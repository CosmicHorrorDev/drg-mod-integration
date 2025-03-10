use thiserror::Error;

#[derive(Error, Debug)]
pub enum IntegrationError {
    #[error("No provider found for {url}")]
    NoProvider {
        url: String,
        factory: &'static crate::providers::ProviderFactory,
    },
}
