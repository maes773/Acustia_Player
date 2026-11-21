use std::fmt;

#[derive(Debug)]
pub enum AudioError {
    /// No hay dispositivo de salida o no se pudo configurar.
    Device(String),
    /// Error del stream de salida.
    Stream(String),
    Io(std::io::Error),
    /// Formato no soportado o archivo corrupto.
    Decode(String),
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AudioError::Device(m) => write!(f, "dispositivo de audio: {m}"),
            AudioError::Stream(m) => write!(f, "stream de audio: {m}"),
            AudioError::Io(e) => write!(f, "E/S: {e}"),
            AudioError::Decode(m) => write!(f, "decodificación: {m}"),
        }
    }
}

impl std::error::Error for AudioError {}

impl From<std::io::Error> for AudioError {
    fn from(e: std::io::Error) -> Self {
        AudioError::Io(e)
    }
}

impl From<symphonia::core::errors::Error> for AudioError {
    fn from(e: symphonia::core::errors::Error) -> Self {
        AudioError::Decode(e.to_string())
    }
}
