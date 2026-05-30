use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("onnx runtime error: {0}")]
    Ort(String),
    #[error("tokenizer error: {0}")]
    Tokenizer(String),
    #[error("model error: {0}")]
    Model(String),
}

impl<C> From<ort::Error<C>> for Error {
    fn from(value: ort::Error<C>) -> Self {
        Self::Ort(value.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
