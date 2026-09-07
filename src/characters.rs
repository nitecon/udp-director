use k8s_openapi::api::core::v1::Pod;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::config::Config;

pub const RECONNECT_SECONDS: i64 = 120;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum CharacterStatus {
    Allocated,
    Used,
    Disconnected,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Character {
    pub character_id: String,
    pub status: CharacterStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disconnected_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CharacterServer {
    pub namespace: String,
    pub name: String,
    pub characters: Vec<Character>,
}

pub fn valid_character_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 63
        && id.as_bytes()[0].is_ascii_alphanumeric()
        && id.as_bytes()[id.len() - 1].is_ascii_alphanumeric()
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
}

/// Read Kubernetes character labels; the controller owns lifecycle mutations.
pub fn characters_on_pod(
    pod: &Pod,
    config: &Config,
    requested: &[String],
    now: OffsetDateTime,
) -> Vec<Character> {
    let prefix = format!("{}/", config.character_label_prefix);
    let disconnected: HashMap<String, String> = pod
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(&config.disconnect_annotation))
        .and_then(|value| serde_json::from_str(value).ok())
        .unwrap_or_default();
    let mut result = Vec::new();
    for (key, value) in pod.metadata.labels.iter().flatten() {
        let Some(id) = key.strip_prefix(&prefix) else {
            continue;
        };
        if !valid_character_id(id) || (!requested.is_empty() && !requested.iter().any(|r| r == id))
        {
            continue;
        }
        let status = match value.as_str() {
            "Allocated" => CharacterStatus::Allocated,
            "Used" => CharacterStatus::Used,
            "Disconnected" => CharacterStatus::Disconnected,
            _ => continue,
        };
        let disconnected_at = if status == CharacterStatus::Disconnected {
            let Some(raw) = disconnected.get(id) else {
                continue;
            };
            let Ok(at) = OffsetDateTime::parse(raw, &Rfc3339) else {
                continue;
            };
            if now < at || now >= at + time::Duration::seconds(RECONNECT_SECONDS) {
                continue;
            }
            Some(raw.clone())
        } else {
            None
        };
        result.push(Character {
            character_id: id.to_string(),
            status,
            disconnected_at,
        });
    }
    result.sort_by(|a, b| a.character_id.cmp(&b.character_id));
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn character_ids_fit_kubernetes_label_names() {
        assert!(valid_character_id("character-123"));
        assert!(valid_character_id("7a23bc10-1234-4321-9988-123456789abc"));
        for id in ["", "character:Used", "a/b", "a,b", "-id"] {
            assert!(!valid_character_id(id));
        }
    }

    #[test]
    fn labels_support_all_states_and_expire_disconnected_at_120_seconds() {
        let config: Config = serde_yaml::from_str(include_str!("../config.example.yaml")).unwrap();
        let now = OffsetDateTime::parse("2026-09-07T12:00:00Z", &Rfc3339).unwrap();
        let pod: Pod = serde_json::from_value(serde_json::json!({
            "metadata": {
                "labels": {
                    "characters.udp-director.io/a": "Allocated",
                    "characters.udp-director.io/b": "Used",
                    "characters.udp-director.io/c": "Disconnected",
                    "characters.udp-director.io/d": "Disconnected",
                    "characters.udp-director.io/e": "Disconnected"
                },
                "annotations": {"udp-director.io/disconnected-at":
                    "{\"c\":\"2026-09-07T11:58:01Z\",\"d\":\"2026-09-07T11:58:00Z\"}"}
            }
        }))
        .unwrap();
        let result = characters_on_pod(&pod, &config, &[], now);
        assert_eq!(
            result
                .iter()
                .map(|c| c.character_id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
        assert_eq!(result[0].status, CharacterStatus::Allocated);
        assert_eq!(result[1].status, CharacterStatus::Used);
        assert_eq!(result[2].status, CharacterStatus::Disconnected);
        assert!(result[2].disconnected_at.is_some());
        let matched = characters_on_pod(&pod, &config, &["b".into(), "c".into()], now);
        assert_eq!(matched.len(), 2);
        assert!(characters_on_pod(&pod, &config, &["absent".into()], now).is_empty());
        assert_eq!(
            characters_on_pod(&pod, &config, &[], now + time::Duration::SECOND).len(),
            2
        );
    }
}
