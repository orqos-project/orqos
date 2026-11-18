//! The pod specification accepted by `PUT /workload/pods/{id}`. Orqos-native: it does not depend on Rezn's IR.
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A pod of containers sharing one network namespace and hostname, run as owned Quadlet units.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PodSpec {
    /// Hostname inside every container of the pod (DNS label, at most 63 characters).
    pub hostname: String,
    pub containers: Vec<ContainerSpec>,
    /// Restart policy of every container. Restarts back off from 1 s to 5 minutes.
    #[serde(default)]
    pub restart: Restart,
    /// Seconds between SIGTERM and SIGKILL when a container stops.
    #[serde(default = "default_grace")]
    pub grace_seconds: u32,
    /// Labels set on the Podman pod and its containers, for clients' own bookkeeping (for example Rezn's
    /// ownership). Keys are lowercase `[a-z0-9./-]`; the `dev.orqos.` prefix is reserved.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    /// An Orqos-owned Podman network to join instead of Podman's default network, with DNS aliases for the pod.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<NetworkSpec>,
}

/// Pods naming the same network share it and resolve each other's aliases (aardvark-dns). Orqos creates the
/// network on first use and removes it when no pod names it any more.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NetworkSpec {
    /// DNS label of at most 40 characters; the Podman network is `orqos-net-<name>`.
    pub name: String,
    /// DNS names that resolve to this pod on the network, in the given order. Several pods may share an alias:
    /// it then resolves to all of them.
    #[serde(default)]
    pub aliases: Vec<String>,
}

fn default_grace() -> u32 {
    30
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Restart {
    #[default]
    Always,
    OnFailure,
    Never,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ContainerSpec {
    /// DNS label, unique in the pod.
    pub name: String,
    /// Image reference; pulled when missing.
    pub image: String,
    /// Replaces the image's entrypoint (and its default command), like Kubernetes `command`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    /// Replaces the image's default command, like Kubernetes `args`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    /// Environment variables, passed literally.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

/// Pod IDs become unit, pod and container names, so they are shorter than DNS labels (as in host mode).
pub fn valid_id(id: &str) -> bool {
    is_label(id, 48)
}

fn is_label(value: &str, max: usize) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= max
        && bytes[0].is_ascii_alphanumeric()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// Network names become `orqos-net-<name>`; 40 characters keep that within Podman's limits with room to spare.
pub fn valid_network_name(name: &str) -> bool {
    is_label(name, 40)
}

/// A lowercase DNS name: dot-separated labels of at most 63 characters, at most 253 in total.
fn valid_dns_name(name: &str) -> bool {
    name.len() <= 253 && name.split('.').all(|label| is_label(label, 63))
}

fn valid_label_key(key: &str) -> bool {
    key.len() <= 128
        && key
            .as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"./-".contains(&b))
        && !key.starts_with("dev.orqos.")
}

/// Label values need no quoting in Quadlet `Label=` lines and no systemd escaping.
fn valid_label_value(value: &str) -> bool {
    value.len() <= 256
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:/@+-".contains(&b))
}

fn has_control(value: &str) -> bool {
    value.chars().any(|c| c.is_control())
}

fn valid_image(image: &str) -> bool {
    !image.is_empty()
        && image.len() <= 255
        && image
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-/:@".contains(&b))
        && image.as_bytes()[0].is_ascii_alphanumeric()
}

fn valid_env_key(key: &str) -> bool {
    let mut bytes = key.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

impl PodSpec {
    /// Checks the spec, returning the first problem with its field path.
    pub fn validate(&self) -> Result<(), String> {
        if !is_label(&self.hostname, 63) {
            return Err(
                "hostname: a DNS label of at most 63 lowercase letters, digits or '-'".into(),
            );
        }
        if self.containers.is_empty() {
            return Err("containers: at least one container".into());
        }
        if self.grace_seconds > 3600 {
            return Err("grace_seconds: at most 3600".into());
        }
        if self.labels.len() > 32 {
            return Err("labels: at most 32".into());
        }
        for (key, value) in &self.labels {
            if !valid_label_key(key) {
                return Err(format!(
                    "labels: invalid key {key:?} (lowercase letters, digits, '.', '/' and '-', at most 128; \"dev.orqos.\" is reserved)"
                ));
            }
            if !valid_label_value(value) {
                return Err(format!(
                    "labels.{key}: letters, digits and . _ : / @ + -, at most 256 characters"
                ));
            }
        }
        if let Some(network) = &self.network {
            if !valid_network_name(&network.name) {
                return Err("network.name: a DNS label of at most 40 characters".into());
            }
            if network.aliases.len() > 64 {
                return Err("network.aliases: at most 64".into());
            }
            let mut seen = BTreeSet::new();
            for (i, alias) in network.aliases.iter().enumerate() {
                if !valid_dns_name(alias) {
                    return Err(format!(
                        "network.aliases[{i}]: a lowercase DNS name (dot-separated labels, at most 253 characters)"
                    ));
                }
                if !seen.insert(alias.as_str()) {
                    return Err(format!("network.aliases[{i}]: duplicate alias {alias:?}"));
                }
            }
        }
        let mut names = BTreeSet::new();
        for (i, c) in self.containers.iter().enumerate() {
            let at = format!("containers[{i}]");
            if !is_label(&c.name, 63) {
                return Err(format!("{at}.name: a DNS label of at most 63 characters"));
            }
            if !names.insert(c.name.as_str()) {
                return Err(format!("{at}.name: duplicate container name {:?}", c.name));
            }
            if !valid_image(&c.image) {
                return Err(format!(
                    "{at}.image: letters, digits and . _ - / : @, at most 255 characters"
                ));
            }
            if let Some(command) = &c.command {
                if command.is_empty() {
                    return Err(format!(
                        "{at}.command: omit it or give at least one element"
                    ));
                }
                if command.iter().any(|a| has_control(a)) {
                    return Err(format!("{at}.command: control characters are not allowed"));
                }
            }
            if let Some(args) = &c.args {
                if args.iter().any(|a| has_control(a)) {
                    return Err(format!("{at}.args: control characters are not allowed"));
                }
                // Without `command`, arguments travel through systemd command-line parsing, where an empty word
                // ends the list and a lone ";" separates commands.
                if c.command.is_none() && args.iter().any(|a| a.is_empty() || a == ";") {
                    return Err(format!(
                        "{at}.args: empty or \";\" arguments need `command` (e.g. the image's entrypoint) to be set"
                    ));
                }
            }
            for (key, value) in &c.env {
                if !valid_env_key(key) {
                    return Err(format!("{at}.env: invalid variable name {key:?}"));
                }
                if has_control(value) {
                    return Err(format!(
                        "{at}.env.{key}: control characters are not allowed"
                    ));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(value: serde_json::Value) -> Result<PodSpec, String> {
        let spec: PodSpec = serde_json::from_value(value).map_err(|e| e.to_string())?;
        spec.validate().map(|()| spec)
    }

    fn minimal() -> serde_json::Value {
        json!({"hostname": "web-0", "containers": [{"name": "web", "image": "docker.io/library/nginx:1.27"}]})
    }

    #[test]
    fn defaults_and_minimal_spec() {
        let s = spec(minimal()).unwrap();
        assert_eq!(s.restart, Restart::Always);
        assert_eq!(s.grace_seconds, 30);
    }

    #[test]
    fn rejects_unknown_fields_and_bad_values() {
        let mut v = minimal();
        v["privileged"] = json!(true);
        assert!(spec(v).is_err());
        let mut v = minimal();
        v["hostname"] = json!("Web");
        assert!(spec(v).unwrap_err().starts_with("hostname"));
        let mut v = minimal();
        v["containers"][0]["image"] = json!("nginx latest");
        assert!(spec(v).unwrap_err().contains(".image"));
        let mut v = minimal();
        v["containers"][0]["env"] = json!({"A": "line\nbreak"});
        assert!(spec(v).unwrap_err().contains("control"));
        let mut v = minimal();
        v["containers"][0]["env"] = json!({"1A": "x"});
        assert!(spec(v).unwrap_err().contains("variable name"));
        let mut v = minimal();
        v["containers"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name": "web", "image": "alpine"}));
        assert!(spec(v).unwrap_err().contains("duplicate"));
    }

    #[test]
    fn labels_are_checked() {
        let mut v = minimal();
        v["labels"] = json!({"dev.rezn.owner": "ab12", "dev.rezn.ordinal": "0", "empty": ""});
        assert!(spec(v.clone()).is_ok());
        v["labels"] = json!({"dev.orqos.owner": "x"});
        assert!(spec(v.clone()).unwrap_err().contains("reserved"));
        v["labels"] = json!({"Upper": "x"});
        assert!(spec(v.clone()).unwrap_err().contains("invalid key"));
        v["labels"] = json!({"k": "two words"});
        assert!(spec(v.clone()).unwrap_err().starts_with("labels.k"));
        v["labels"] = json!({"k": "100%"});
        assert!(spec(v).unwrap_err().starts_with("labels.k"));
        let mut v = minimal();
        v["containers"][0]["name"] = json!("c".repeat(63));
        assert!(spec(v.clone()).is_ok());
        v["containers"][0]["name"] = json!("c".repeat(64));
        assert!(spec(v).is_err());
    }

    #[test]
    fn networks_are_checked() {
        let mut v = minimal();
        v["network"] = json!({"name": "rz-0123", "aliases": ["web", "web.shop.svc", "web-0.web"]});
        assert_eq!(spec(v.clone()).unwrap().network.unwrap().aliases.len(), 3);
        v["network"] = json!({"name": "rz-0123"});
        assert!(spec(v.clone()).unwrap().network.unwrap().aliases.is_empty());
        v["network"] = json!({"name": "n".repeat(41)});
        assert!(spec(v.clone()).unwrap_err().starts_with("network.name"));
        v["network"] = json!({"name": "Net"});
        assert!(spec(v.clone()).unwrap_err().starts_with("network.name"));
        for bad in [
            "Web", "web..svc", ".web", "web.", "-web", "web_1", "a b", "",
        ] {
            v["network"] = json!({"name": "n", "aliases": [bad]});
            assert!(
                spec(v.clone())
                    .unwrap_err()
                    .starts_with("network.aliases[0]"),
                "{bad:?}"
            );
        }
        v["network"] = json!({"name": "n", "aliases": ["web", "web"]});
        assert!(spec(v.clone()).unwrap_err().contains("duplicate"));
        v["network"] =
            json!({"name": "n", "aliases": (0..65).map(|i| format!("a{i}")).collect::<Vec<_>>()});
        assert!(spec(v.clone()).unwrap_err().contains("at most 64"));
        v["network"] = json!({"name": "n", "extra": 1});
        assert!(spec(v).is_err());
    }

    #[test]
    fn args_needing_command_are_explained() {
        let mut v = minimal();
        v["containers"][0]["args"] = json!(["a", ""]);
        assert!(spec(v.clone()).unwrap_err().contains("need `command`"));
        v["containers"][0]["args"] = json!([";"]);
        assert!(spec(v.clone()).unwrap_err().contains("need `command`"));
        v["containers"][0]["command"] = json!(["/bin/sh", "-c"]);
        assert!(spec(v).is_ok());
    }

    #[test]
    fn ids_are_short_labels() {
        assert!(valid_id("shop-api-0"));
        assert!(!valid_id(&"a".repeat(49)));
        assert!(!valid_id("Shop"));
        assert!(!valid_id("-x"));
    }
}
