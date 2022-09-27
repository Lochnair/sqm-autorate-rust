use std::error::Error;
use std::fmt;
use std::fmt::Display;

#[derive(Default, Debug)]
pub struct ConfigParseError {
    pub(crate) config_key: String,
}

impl Error for ConfigParseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        None
    }
}

// Implement std::fmt::Display for AppError
impl Display for ConfigParseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Couldn't parse value from string for key: {}",
            self.config_key
        ) // user-facing output
    }
}

#[derive(Default, Debug)]
pub struct InvalidMeasurementTypeError {
    pub(crate) type_: String,
}

impl Error for InvalidMeasurementTypeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        None
    }
}

impl Display for InvalidMeasurementTypeError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Couldn't find measurement type: {}", self.type_)
    }
}

#[derive(Default, Debug)]
pub struct MissingConfigError {
    pub(crate) config_key: String,
}

impl Error for MissingConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        None
    }
}

// Implement std::fmt::Display for AppError
impl Display for MissingConfigError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Couldn't find required configuration option: {}",
            self.config_key
        ) // user-facing output
    }
}

#[derive(Default, Debug)]
pub struct NoQdiscFoundError;

impl Error for NoQdiscFoundError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
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
pub struct PingParseError {
    pub(crate) msg: String,
}

impl Error for PingParseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        None
    }
}

// Implement std::fmt::Display for AppError
impl Display for PingParseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Couldn't parse received ping packet: {}", self.msg) // user-facing output
    }
}
