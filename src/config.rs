#[cfg(feature = "uci")]
use rust_uci::Uci;
use std::error::Error;
use std::fs::File;
use std::io::BufRead;
use std::net::IpAddr;
use std::path::Path;
use std::str::FromStr;
use std::{env, io};

fn read_lines<P>(filename: P) -> io::Result<io::Lines<io::BufReader<File>>>
where
    P: AsRef<Path>,
{
    let file = File::open(filename)?;
    Ok(io::BufReader::new(file).lines())
}

#[allow(dead_code)]
#[derive(Clone)]
pub(crate) struct Config {
    pub(crate) download_interface: String,
    pub(crate) upload_interface: String,

    pub(crate) reflector_list_file: String,

    pub(crate) min_change_interval: f64,
    pub(crate) num_reflectors: u8,
    pub(crate) tick_duration: f64,
}

impl Config {
    pub fn new() -> Self {
        // First try to load from UCI (if enabled), if nothing is returned, use the environment instead.
        match Self::load_from_uci() {
            Some(config) => config,
            None => Self {
                download_interface: env::var("DOWNLOAD_INTERFACE")
                    .expect("A download interface is required"),
                upload_interface: env::var("UPLOAD_INTERFACE")
                    .expect("A upload interface is required"),
                reflector_list_file: env::var("REFLECTOR_LIST_FILE")
                    .expect("A reflector list is required"),
                min_change_interval: 0.5,
                num_reflectors: 5,
                tick_duration: 0.5,
            },
        }
    }

    #[cfg(feature = "uci")]
    fn load_from_uci() -> Option<Self> {
        let uci: Uci = Uci::new().expect("Couldn't create UCI instance");

        Some(Self {
            download_interface: "wan".to_string(),
            upload_interface: "wan".to_string(),
            reflector_list_file: "".to_string(),
            min_change_interval: 0.5,
            num_reflectors: 5,
            tick_duration: 0.5,
        })
    }

    #[cfg(not(feature = "uci"))]
    fn load_from_uci() -> Option<Self> {
        None
    }

    pub fn load_reflectors(&self) -> Result<Vec<IpAddr>, Box<dyn Error>> {
        let lines = read_lines(self.reflector_list_file.clone())?;

        let mut reflectors: Vec<IpAddr> = Vec::with_capacity(50);

        let mut first = true;

        for line in lines {
            if first {
                first = false;
                continue;
            }

            let line = line?;
            let columns: Vec<&str> = line.split(",").collect();
            reflectors.push(IpAddr::from_str(columns[0])?);
        }

        Ok(reflectors)
    }
}
