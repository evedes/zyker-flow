//! flow's one error type: a message, printed as `flow: <message>`.

use std::fmt;

#[derive(Debug)]
pub struct Error(pub String);

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

macro_rules! impl_from {
    ($($t:ty),*) => {$(
        impl From<$t> for Error {
            fn from(e: $t) -> Error {
                Error(e.to_string())
            }
        }
    )*};
}

impl_from!(String, &str, std::io::Error, serde_json::Error, fancy_regex::Error, regex::Error);

/// Returns early with an [`Error`] built like `format!`.
macro_rules! bail {
    ($($t:tt)*) => {
        return Err($crate::error::Error(format!($($t)*)))
    };
}
pub(crate) use bail;
