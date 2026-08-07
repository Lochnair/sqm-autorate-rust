use std::path::{Path, PathBuf};

use config::{ConfigError as SourceError, Map, Source, Value, ValueKind};

use rust_uci::config::{
    Config as UciConfig, Package as UciPackage, SectionSelector, Value as UciValue,
};

#[derive(Clone, Debug)]
enum PackageSelection {
    Selected(Vec<String>),
    All,
}

#[derive(Clone, Debug)]
pub(crate) struct UciSource {
    packages: PackageSelection,
    required: bool,
    config_dir: Option<PathBuf>,
    save_dir: Option<PathBuf>,
}

impl UciSource {
    fn with_selection(packages: PackageSelection) -> Self {
        Self {
            packages,
            required: true,
            config_dir: None,
            save_dir: None,
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

    #[allow(dead_code)]
    pub(crate) fn with_directories(
        mut self,
        config_dir: impl AsRef<Path>,
        save_dir: impl AsRef<Path>,
    ) -> Self {
        self.config_dir = Some(config_dir.as_ref().to_owned());
        self.save_dir = Some(save_dir.as_ref().to_owned());
        self
    }

    fn open(&self) -> Result<UciConfig, SourceError> {
        let mut uci = rust_uci::Uci::new()
            .map_err(|e| SourceError::Message(format!("failed to open UCI: {e}")))?;

        if let Some(config_dir) = &self.config_dir {
            uci.set_config_dir(config_dir).map_err(|e| {
                SourceError::Message(format!(
                    "failed to set UCI config directory {config_dir:?}: {e}"
                ))
            })?;
        }

        if let Some(save_dir) = &self.save_dir {
            uci.set_save_dir(save_dir).map_err(|e| {
                SourceError::Message(format!(
                    "failed to set UCI save directory {save_dir:?}: {e}"
                ))
            })?;
        }

        Ok(uci.into())
    }

    fn config_selector(selector: &SectionSelector<'_>) -> String {
        match selector {
            SectionSelector::Named(name) => (*name).to_owned(),

            SectionSelector::Anonymous {
                section_type,
                index,
            } => format!("{section_type}[{index}]"),
        }
    }

    fn collect_package(
        &self,
        package_name: &str,
        package: &UciPackage,
        values: &mut Map<String, Value>,
    ) -> Result<(), SourceError> {
        let sections = package.sections().map_err(|e| {
            SourceError::Message(format!(
                "failed to retrieve sections from UCI package \
                 {package_name:?}: {e}"
            ))
        })?;

        for section in sections {
            let selector = section.selector().ok_or_else(|| {
                SourceError::Message(format!(
                    "loaded UCI section of type {:?} in package \
                     {package_name:?} has no selector",
                    section.type_()
                ))
            })?;

            let options = section.options().map_err(|e| {
                SourceError::Message(format!(
                    "failed to retrieve options from UCI section \
                     {package_name}.{selector}: {e}"
                ))
            })?;

            for option in options {
                let option_name = option.name();

                let config_relative_key =
                    format!("{}.{option_name}", Self::config_selector(&selector),);

                let destination_key = format!("{package_name}.{config_relative_key}");

                let uci_relative_key = format!("{selector}.{option_name}");
                let uci_key = format!("{package_name}.{uci_relative_key}");

                let Some(value) = option.get().map_err(|e| {
                    SourceError::Message(format!(
                        "failed to retrieve UCI option \
                         {uci_key}: {e}"
                    ))
                })?
                else {
                    continue;
                };

                let origin = format!("UCI option {uci_key}");

                let value = match value {
                    UciValue::String(value) => Value::new(Some(&origin), ValueKind::String(value)),

                    UciValue::List(items) => {
                        let items = items
                            .into_iter()
                            .map(|item| Value::new(Some(&origin), ValueKind::String(item)))
                            .collect();

                        Value::new(Some(&origin), ValueKind::Array(items))
                    }
                };

                if values.insert(destination_key.clone(), value).is_some() {
                    return Err(SourceError::Message(format!(
                        "duplicate UCI destination key \
                         {destination_key:?} while mapping {uci_key:?}"
                    )));
                }
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
        let resolved = (|| {
            let uci = self.open()?;

            let packages: Vec<UciPackage> = match &self.packages {
                PackageSelection::Selected(package_names) => package_names
                    .iter()
                    .map(|package_name| {
                        uci.package(package_name)
                            .map_err(|e| {
                                SourceError::Message(format!(
                                    "failed to open UCI package {package_name:?}: {e}"
                                ))
                            })?
                            .ok_or_else(|| {
                                SourceError::NotFound(format!("UCI package {package_name:?}"))
                            })
                    })
                    .collect::<Result<_, SourceError>>()?,

                PackageSelection::All => uci
                    .packages()
                    .map_err(|e| {
                        SourceError::Message(format!("failed to enumerate UCI packages: {e}"))
                    })?
                    .collect(),
            };

            Ok::<_, SourceError>((uci, packages))
        })();

        let (_uci, packages) = match resolved {
            Ok(resolved) => resolved,

            Err(_) if !self.required => {
                return Ok(Map::new());
            }

            Err(error) => {
                return Err(error);
            }
        };

        let mut values = Map::new();

        for package in packages {
            let package_name = package
                .name()
                .map_err(|e| SourceError::Message(format!("failed to read UCI package name: {e}")))?
                .to_owned();

            self.collect_package(&package_name, &package, &mut values)?;
        }

        Ok(values)
    }
}
