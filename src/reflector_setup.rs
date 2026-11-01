// SPDX-FileCopyrightText: 2022-Present Charles Corrigan mailto:chas-iot@runegate.org (github @chas-iot)
// SPDX-FileCopyrightText: 2022-Present Daniel Lakeland mailto:dlakelan@street-artists.org (github @dlakelan)
// SPDX-FileCopyrightText: 2022-Present Mark Baker mailto:mark@vpost.net (github @Fail-Safe)
// SPDX-FileCopyrightText: 2022-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use crate::settings::{ReflectorDefinition, ReflectorProtocol, Settings};
use log::warn;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};

pub(crate) struct ReflectorSetup {
    pub(crate) initial: Vec<SocketAddr>,
    pub(crate) pool: Vec<SocketAddr>,
    pub(crate) reflectors: HashMap<SocketAddr, ReflectorDefinition>,
    pub(crate) reselection_enabled: bool,
}

fn resolve_reflectors(
    definitions: &[ReflectorDefinition],
    protocol: ReflectorProtocol,
    countries: &[String],
    filter_country: bool,
) -> anyhow::Result<HashMap<SocketAddr, ReflectorDefinition>> {
    let mut resolved = HashMap::new();
    for definition in definitions {
        if !definition.protocols.contains(&protocol) {
            continue;
        }
        if protocol != ReflectorProtocol::Irtt
            && definition
                .host
                .parse::<IpAddr>()
                .is_ok_and(|ip| ip.is_ipv6())
        {
            continue;
        }
        if filter_country
            && !countries.is_empty()
            && definition.country.as_ref().is_some_and(|country| {
                !countries
                    .iter()
                    .any(|allowed| allowed.eq_ignore_ascii_case(country))
            })
        {
            continue;
        }
        let port = if protocol == ReflectorProtocol::Irtt {
            definition.irtt_port.unwrap_or(2112)
        } else {
            0
        };
        let endpoints = match (definition.host.as_str(), port).to_socket_addrs() {
            Ok(endpoints) => endpoints,
            Err(error) => {
                warn!("Failed to resolve reflector {}: {error}", definition.host);
                continue;
            }
        };
        for endpoint in endpoints {
            if protocol != ReflectorProtocol::Irtt && endpoint.is_ipv6() {
                continue;
            }
            insert_reflector(&mut resolved, endpoint, definition.clone())?;
        }
    }
    Ok(resolved)
}

fn insert_reflector(
    reflectors: &mut HashMap<SocketAddr, ReflectorDefinition>,
    endpoint: SocketAddr,
    definition: ReflectorDefinition,
) -> anyhow::Result<()> {
    if let Some(existing) = reflectors.get_mut(&endpoint) {
        let same_country = match (&existing.country, &definition.country) {
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
            _ => true,
        };
        let same_protocols = existing
            .protocols
            .iter()
            .all(|p| definition.protocols.contains(p))
            && definition
                .protocols
                .iter()
                .all(|p| existing.protocols.contains(p));
        if !same_country
            || !same_protocols
            || existing.irtt_port.unwrap_or(2112) != definition.irtt_port.unwrap_or(2112)
            || existing.irtt_hmac != definition.irtt_hmac
        {
            anyhow::bail!("conflicting reflector definitions for resolved endpoint {endpoint}");
        }
        if existing.country.is_none() {
            existing.country = definition.country;
        }
    } else {
        reflectors.insert(endpoint, definition);
    }
    Ok(())
}

pub(crate) fn setup_reflectors(settings: &Settings) -> anyhow::Result<ReflectorSetup> {
    let protocol = settings.advanced_settings.measurement_type;
    let configured = resolve_reflectors(
        &settings.reflectors.entries,
        protocol,
        &settings.reflectors.countries,
        true,
    )?;
    let bootstrap = resolve_reflectors(&settings.reflectors.bootstrap, protocol, &[], false)?;
    let mut reflectors = bootstrap.clone();
    for (endpoint, definition) in &configured {
        insert_reflector(&mut reflectors, *endpoint, definition.clone())?;
    }
    if reflectors.is_empty() {
        anyhow::bail!("no usable reflectors for measurement protocol {protocol}");
    }
    let mut pool = reflectors.keys().copied().collect::<Vec<_>>();
    pool.sort();
    let num_reflectors = settings.reflectors.num_reflectors as usize;
    let reselection_enabled = pool.len() > num_reflectors;
    let initial = if reselection_enabled {
        // Bootstrap priority applies only to initialization. All endpoints remain
        // in the sorted pool for subsequent selection.
        pool.iter()
            .filter(|endpoint| bootstrap.contains_key(endpoint))
            .chain(
                pool.iter()
                    .filter(|endpoint| !bootstrap.contains_key(endpoint)),
            )
            .take(num_reflectors)
            .copied()
            .collect()
    } else {
        pool.clone()
    };

    Ok(ReflectorSetup {
        initial,
        pool,
        reflectors,
        reselection_enabled,
    })
}
