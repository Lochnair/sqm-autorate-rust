use std::error::Error;
use std::fmt;
use std::fmt::Display;

#[derive(Default, Debug)]
pub struct NoQdiscFoundError;

impl Error for NoQdiscFoundError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        // The compiler transparently casts `&sqlx::Error` into a `&dyn Error`
        None
    }
}

// Implement std::fmt::Display for AppError
impl Display for NoQdiscFoundError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Couldn't find a CAKE qdisc for interface") // user-facing output
    }
}

#[derive(Default, Debug)]
pub struct PingParseError;

impl Error for PingParseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        // The compiler transparently casts `&sqlx::Error` into a `&dyn Error`
        None
    }
}

// Implement std::fmt::Display for AppError
impl Display for PingParseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Couldn't parse received ping packet") // user-facing output
    }
}
