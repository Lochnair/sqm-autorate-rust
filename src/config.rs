use std::env;

#[allow(dead_code)]
pub(crate) struct Config {
    pub(crate) download_interface: String,
    pub(crate) upload_interface: String,
}

impl Config {
    pub fn new() -> Self {
        Self {
            download_interface: env::var("DOWNLOAD_INTERFACE")
                .expect("A download interface is required"),
            upload_interface: env::var("UPLOAD_INTERFACE").expect("A upload interface is required"),
        }
    }
}
