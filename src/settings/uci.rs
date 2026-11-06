// SPDX-FileCopyrightText: 2026-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::settings::uci_schema::{UciConfigSchema, UciRepeatedSectionMapping, UciSectionMapping};
use config::{ConfigError as SourceError, Map, Source, Value, ValueKind};

use ucifer::document::FromStrError;
use ucifer::{ConfigOption, Document};

#[derive(Clone, Debug)]
enum PackageSelection {
    Selected(Vec<String>),
    All,
}

#[derive(Clone, Debug)]
pub(crate) struct UciSource {
    packages: PackageSelection,
    required: bool,
    schema: &'static [UciSectionMapping],
    repeated_schema: &'static [UciRepeatedSectionMapping],
    config_dir: Option<PathBuf>,
}

impl UciSource {
    fn with_selection(packages: PackageSelection) -> Self {
        Self {
            packages,
            required: true,
            schema: &[],
            repeated_schema: &[],
            config_dir: None,
        }
    }

    pub(crate) fn new<I, S>(packages: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::with_selection(PackageSelection::Selected(
            packages.into_iter().map(Into::into).collect(),
        ))
    }

    #[allow(dead_code)]
    pub(crate) fn all() -> Self {
        Self::with_selection(PackageSelection::All)
    }

    pub(crate) fn required(mut self, required: bool) -> Self {
        self.required = required;
        self
    }

    pub(crate) fn from_schema<T>() -> Self
    where
        T: UciConfigSchema,
    {
        Self::new([T::UCI_PACKAGE]).with_schema(T::UCI_SECTIONS)
    }

    pub(crate) fn with_schema(mut self, schema: &'static [UciSectionMapping]) -> Self {
        self.schema = schema;
        self
    }

    /// Registers section types collected as arrays of tables at their config paths.
    /// Absent section types leave the destination unset for Serde/source defaults.
    #[allow(dead_code)]
    pub(crate) fn with_repeated_schema(
        mut self,
        schema: &'static [UciRepeatedSectionMapping],
    ) -> Self {
        self.repeated_schema = schema;
        self
    }

    #[allow(dead_code)]
    pub(crate) fn with_config_dir(mut self, config_dir: impl AsRef<Path>) -> Self {
        self.config_dir = Some(config_dir.as_ref().to_owned());
        self
    }

    fn package_names(&self, config_dir: &Path) -> Result<Vec<String>, SourceError> {
        if let PackageSelection::Selected(names) = &self.packages {
            return Ok(names.clone());
        }

        let enumerate_error = |error| {
            SourceError::Message(format!(
                "failed to enumerate UCI packages in {config_dir:?}: {error}"
            ))
        };
        let entries = match fs::read_dir(config_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(enumerate_error(error)),
        };
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(enumerate_error)?;
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if !name.contains('.') && entry.path().is_file() {
                names.push(name);
            }
        }
        names.sort();
        Ok(names)
    }

    fn mapped_config_key(&self, section_type: &str, uci_option: &str) -> Option<String> {
        self.schema
            .iter()
            .filter(|section| section.uci_section == section_type)
            .find_map(|section| {
                section
                    .options
                    .iter()
                    .find(|option| option.uci_option == uci_option)
                    .map(|option| format!("{}.{}", section.config_section, option.config_field,))
            })
    }

    fn collect_package(
        &self,
        package_name: &str,
        package: &Document<'_>,
        values: &mut Map<String, Value>,
        repeated_values: &mut Map<String, Vec<Value>>,
    ) -> Result<(), SourceError> {
        for (key, section) in package.sections_iter() {
            let section_type = package
                .type_name(key.type_key)
                .expect("document section types are interned");
            let section_name = package.section_name(*key);
            let repeated_mapping = self
                .repeated_schema
                .iter()
                .find(|mapping| mapping.uci_section == section_type);
            let mut section_values = Map::new();

            // config-rs paths omit the @ prefix used by UCI anonymous selectors.
            let config_selector = section_name
                .map(str::to_owned)
                .unwrap_or_else(|| format!("{section_type}[{}]", key.index));
            let uci_selector = if section_name.is_some() {
                config_selector.clone()
            } else {
                format!("@{config_selector}")
            };

            for (option_name, option) in section.iter() {
                let option_name = option_name.as_ref();
                let uci_key = format!("{package_name}.{uci_selector}.{option_name}");

                let repeated_option = repeated_mapping.and_then(|mapping| {
                    mapping
                        .options
                        .iter()
                        .find(|option| option.uci_option == option_name)
                });

                let destination_key = if let Some(mapping) = repeated_option {
                    mapping.config_field.to_owned()
                } else if repeated_mapping.is_some() {
                    option_name.to_owned()
                } else {
                    let mapped_key = if section_name.is_none() && key.index == 0 {
                        self.mapped_config_key(section_type, option_name)
                    } else {
                        None
                    };
                    mapped_key.unwrap_or_else(|| {
                        format!("{package_name}.{config_selector}.{option_name}")
                    })
                };

                let origin = format!("UCI option {uci_key}");
                let value = match option {
                    // libuci treats an empty scalar as an unset option.
                    ConfigOption::Scalar(value) if value.is_empty() => continue,
                    ConfigOption::Scalar(value) => {
                        Value::new(Some(&origin), ValueKind::String(value.as_ref().to_owned()))
                    }
                    ConfigOption::List(items) => {
                        let items = items
                            .iter()
                            .map(|item| {
                                Value::new(
                                    Some(&origin),
                                    ValueKind::String(item.as_ref().to_owned()),
                                )
                            })
                            .collect();

                        Value::new(Some(&origin), ValueKind::Array(items))
                    }
                };

                let destination = if repeated_mapping.is_some() {
                    &mut section_values
                } else {
                    &mut *values
                };

                if destination.insert(destination_key.clone(), value).is_some() {
                    return Err(SourceError::Message(format!(
                        "duplicate UCI destination key \
                         {destination_key:?} while mapping {uci_key:?}"
                    )));
                }
            }

            if let Some(mapping) = repeated_mapping {
                let origin = format!("UCI section {package_name}.{uci_selector}");
                repeated_values
                    .entry(mapping.config_path.to_owned())
                    .or_default()
                    .push(Value::new(Some(&origin), ValueKind::Table(section_values)));
            }
        }

        Ok(())
    }
}

impl Source for UciSource {
    fn clone_into_box(&self) -> Box<dyn Source + Send + Sync> {
        Box::new(self.clone())
    }

    fn collect(&self) -> Result<Map<String, Value>, SourceError> {
        let config_dir = self
            .config_dir
            .as_deref()
            .unwrap_or(Path::new("/etc/config"));
        let mut values = Map::new();
        let mut repeated_values = Map::new();

        // ucifer reads package files directly. OpenWrt's runtime overlays and staged
        // deltas are managed by libuci and are outside this source's file input.
        for package_name in self.package_names(config_dir)? {
            let path = config_dir.join(&package_name);
            let contents = match fs::read_to_string(&path) {
                Ok(contents) => contents,
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    return if self.required {
                        Err(SourceError::NotFound(format!("UCI package {path:?}")))
                    } else {
                        Ok(Map::new())
                    };
                }
                Err(error) => {
                    return Err(SourceError::Message(format!(
                        "failed to read UCI package {path:?}: {error}"
                    )));
                }
            };
            let package = Document::try_from(contents.as_str()).map_err(|error| {
                let detail = match &error {
                    FromStrError::Syntax(error) => error.to_string(),
                    FromStrError::Import(error) => error.to_string(),
                };
                SourceError::Message(format!("failed to parse UCI package {path:?}: {detail}"))
            })?;

            self.collect_package(&package_name, &package, &mut values, &mut repeated_values)?;
        }

        for (config_path, sections) in repeated_values {
            let origin = "UCI repeated sections".to_owned();
            if values
                .insert(
                    config_path.clone(),
                    Value::new(Some(&origin), ValueKind::Array(sections)),
                )
                .is_some()
            {
                return Err(SourceError::Message(format!(
                    "duplicate UCI destination key {config_path:?} while mapping repeated sections"
                )));
            }
        }

        Ok(values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;
    use crate::settings::uci_schema::UciOptionMapping;

    #[test]
    fn optional_package_distinguishes_missing_and_malformed_files() {
        let root = std::env::temp_dir().join(format!("unravel-uci-{}", std::process::id()));
        let config_dir = root.join("config");
        fs::create_dir_all(&config_dir).unwrap();
        let config_file = config_dir.join("unravel");
        let source = UciSource::from_schema::<Settings>()
            .with_config_dir(&config_dir)
            .required(false);
        let missing = source.collect();
        fs::write(&config_file, "config network 'unterminated\n").unwrap();
        let result = source.collect();

        fs::remove_dir_all(root).unwrap();
        assert!(missing.unwrap().is_empty());
        assert!(result.is_err());
    }

    #[test]
    fn maps_anonymous_indices_and_preserves_repeated_list_order() {
        let config_dir =
            std::env::temp_dir().join(format!("unravel-uci-mapping-{}", std::process::id()));
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join("unravel"),
            "\
config network 'named'
    option device 'eth0'
config reflector 'first'
    list address 'one'
    list address 'two'
config network
    option device 'eth1'
    option unused ''
config reflector
    list address 'three'
",
        )
        .unwrap();

        let source = UciSource::new(["unravel"])
            .with_config_dir(&config_dir)
            .with_schema(&[UciSectionMapping {
                uci_section: "network",
                config_section: "network",
                options: &[UciOptionMapping {
                    uci_option: "device",
                    config_field: "device",
                }],
            }])
            .with_repeated_schema(&[UciRepeatedSectionMapping {
                uci_section: "reflector",
                config_path: "reflectors.entries",
                options: &[UciOptionMapping {
                    uci_option: "address",
                    config_field: "addresses",
                }],
            }]);
        let result = config::Config::builder().add_source(source).build();
        fs::remove_dir_all(config_dir).unwrap();

        let config = result.unwrap();
        assert_eq!(config.get_string("unravel.named.device").unwrap(), "eth0");
        assert_eq!(
            config.get_string("unravel.network[1].device").unwrap(),
            "eth1"
        );
        assert!(config.get_string("network.device").is_err());
        assert!(config.get_string("unravel.network[1].unused").is_err());
        assert_eq!(
            config
                .get::<Vec<String>>("reflectors.entries[0].addresses")
                .unwrap(),
            ["one", "two"]
        );
        assert_eq!(
            config
                .get::<Vec<String>>("reflectors.entries[1].addresses")
                .unwrap(),
            ["three"]
        );
    }
}
