use config::{ConfigError, Map, Source, Value, ValueKind};
use rust_uci::Uci;

#[derive(Clone, Copy, Debug)]
struct ConfigKey {
    canonical: &'static str,
    uci: &'static str,
}

const UCI_KEYS: &[ConfigKey] = &[
    ConfigKey {
        canonical: "network.download_base_kbits",
        uci: "sqm-autorate-rust.@network[0].download_base_kbits",
    },
    ConfigKey {
        canonical: "network.download_interface",
        uci: "sqm-autorate-rust.@network[0].download_interface",
    },
    ConfigKey {
        canonical: "observability.enabled",
        uci: "sqm-autorate-rust.@observability[0].enabled",
    },
    // ...
];

#[derive(Clone, Debug)]
struct UciSource {
    keys: &'static [ConfigKey],
}

impl UciSource {
    fn new() -> Self {
        Self { keys: UCI_KEYS }
    }
}

impl Source for UciSource {
    fn clone_into_box(&self) -> Box<dyn Source + Send + Sync> {
        Box::new(self.clone())
    }

    fn collect(&self) -> Result<Map<String, Value>, ConfigError> {
        let mut uci =
            Uci::new().map_err(|e| ConfigError::Message(format!("failed to open UCI: {e}")))?;

        let origin = "UCI package sqm-autorate-rust".to_owned();
        let mut values = Map::new();

        for key in self.keys {
            match uci.get(key.uci) {
                Ok(raw) => {
                    values.insert(
                        key.canonical.to_owned(),
                        Value::new(Some(&origin), ValueKind::String(raw)),
                    );
                }

                // This needs a real check for UCI's "not found" result.
                Err(e) if is_missing_uci_value(&e) => {}

                Err(e) => {
                    return Err(ConfigError::Message(format!(
                        "failed reading UCI key {}: {e}",
                        key.uci
                    )));
                }
            }
        }

        Ok(values)
    }
}
