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
        let mut names = BTreeSet::new();
        for (i, c) in self.containers.iter().enumerate() {
            let at = format!("containers[{i}]");
            if !is_label(&c.name, 48) {
                return Err(format!("{at}.name: a DNS label of at most 48 characters"));
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
