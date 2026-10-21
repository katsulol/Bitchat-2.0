use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Envelope error: {0}")]
    Envelope(String),

    #[error("Crypto error: {0}")]
    Crypto(String),

    #[error("Transport error: {0}")]
    Transport(String),

    #[error("KeyStore error: {0}")]
    KeyStore(String),

    #[error("Fragmentation error: {0}")]
    Fragmentation(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
