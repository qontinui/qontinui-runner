//! Request-scoped `properties` / `attributes` projection for the element-state
//! routes.
//!
//! ui-bridge answers `GET /control/element/:id/state?properties=a,b&properties=c
//! &attributes=lang,dir` with a `projection` object beside the state. The runner
//! forwards the two keys verbatim (repeats preserved) and lets the SDK own the
//! grammar; it only enforces a defensive byte cap. Everything here is pure.

use serde_json::{json, Value};

/// Defensive cap on the total bytes of projection keys + values. Looser than the
/// SDK's own `MAX_PROJECTION_QUERY_LENGTH` (4,160), so it never rejects what the
/// SDK would accept.
pub const MAX_PROJECTION_BYTES: usize = 8 * 1024;

/// SDK code for a malformed projection request.
pub const REFUSAL_CODE: &str = "UB-VALIDATION-ERROR";

/// One key's values: a string when given once, an array when repeated
/// (mirrors ui-bridge's `routeQueryOf`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionList {
    One(String),
    Many(Vec<String>),
}

impl ProjectionList {
    fn from_values(mut values: Vec<String>) -> Option<Self> {
        match values.len() {
            0 => None,
            1 => Some(Self::One(values.remove(0))),
            _ => Some(Self::Many(values)),
        }
    }

    fn to_json(&self) -> Value {
        match self {
            Self::One(s) => json!(s),
            Self::Many(v) => json!(v),
        }
    }

    fn values(&self) -> Vec<&str> {
        match self {
            Self::One(s) => vec![s.as_str()],
            Self::Many(v) => v.iter().map(String::as_str).collect(),
        }
    }
}

/// A refused projection request (never truncated).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionRefusal {
    pub message: String,
}

impl ProjectionRefusal {
    /// The runner's usual `{success:false,error,code}` JSON.
    pub fn to_json(&self) -> Value {
        json!({ "success": false, "error": self.message, "code": REFUSAL_CODE })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateProjection {
    pub properties: Option<ProjectionList>,
    pub attributes: Option<ProjectionList>,
}

impl StateProjection {
    /// Parse the raw query string. `Ok(None)` when neither key is present.
    pub fn from_raw_query(raw: Option<&str>) -> Result<Option<Self>, ProjectionRefusal> {
        let Some(raw) = raw else { return Ok(None) };
        let mut props: Vec<String> = Vec::new();
        let mut attrs: Vec<String> = Vec::new();
        let mut bytes = 0usize;
        for (k, v) in url::form_urlencoded::parse(raw.as_bytes()) {
            let bucket = match k.as_ref() {
                "properties" => &mut props,
                "attributes" => &mut attrs,
                _ => continue,
            };
            bytes += k.len() + v.len();
            if bytes > MAX_PROJECTION_BYTES {
                return Err(ProjectionRefusal {
                    message: format!(
                        "projection request exceeds {} bytes; refused rather than truncated",
                        MAX_PROJECTION_BYTES
                    ),
                });
            }
            bucket.push(v.into_owned());
        }
        let properties = ProjectionList::from_values(props);
        let attributes = ProjectionList::from_values(attrs);
        if properties.is_none() && attributes.is_none() {
            return Ok(None);
        }
        Ok(Some(Self {
            properties,
            attributes,
        }))
    }

    fn insert_keys(&self, obj: &mut serde_json::Map<String, Value>) {
        if let Some(p) = &self.properties {
            obj.insert("properties".into(), p.to_json());
        }
        if let Some(a) = &self.attributes {
            obj.insert("attributes".into(), a.to_json());
        }
    }

    /// WS payload for the projection-aware `getElementState` action.
    pub fn ws_payload(&self, id: &str) -> Value {
        let mut obj = serde_json::Map::new();
        obj.insert("id".into(), json!(id));
        self.insert_keys(&mut obj);
        Value::Object(obj)
    }

    /// HTTP path with the projection pairs re-encoded in original order.
    pub fn http_path(&self, id: &str) -> String {
        let mut pairs: Vec<String> = Vec::new();
        for (key, list) in [
            ("properties", &self.properties),
            ("attributes", &self.attributes),
        ] {
            if let Some(list) = list {
                for v in list.values() {
                    pairs.push(format!("{}={}", key, urlencoding::encode(v)));
                }
            }
        }
        format!("/control/element/{}/state?{}", id, pairs.join("&"))
    }

    /// IPC args for `get_element_state`.
    pub fn ipc_args(&self, id: &str) -> Value {
        let mut obj = serde_json::Map::new();
        obj.insert("elementId".into(), json!(id));
        self.insert_keys(&mut obj);
        Value::Object(obj)
    }
}

/// IPC args for `get_element_state`; with no projection, the legacy shape exactly.
pub fn ipc_args_for(projection: Option<&StateProjection>, id: &str) -> Value {
    match projection {
        Some(p) => p.ipc_args(id),
        None => json!({ "elementId": id }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(q: &str) -> Option<StateProjection> {
        StateProjection::from_raw_query(Some(q)).unwrap()
    }

    #[test]
    fn full_projection_payload_and_path() {
        let p = parse("properties=a,b&properties=c&attributes=lang,dir").unwrap();
        assert_eq!(
            p.ws_payload("save"),
            json!({ "id": "save", "properties": ["a,b", "c"], "attributes": "lang,dir" })
        );
        assert_eq!(
            p.http_path("save"),
            "/control/element/save/state?properties=a%2Cb&properties=c&attributes=lang%2Cdir"
        );
    }

    #[test]
    fn absent_projection_is_none() {
        assert_eq!(StateProjection::from_raw_query(None).unwrap(), None);
        assert_eq!(parse(""), None);
        assert_eq!(parse("tabId=x"), None);
        assert_eq!(parse("?"), None);
        assert_eq!(ipc_args_for(None, "save"), json!({ "elementId": "save" }));
    }

    #[test]
    fn once_is_string_repeat_is_array_in_order() {
        let p = parse("properties=a&attributes=x&attributes=y&attributes=z").unwrap();
        assert_eq!(p.properties, Some(ProjectionList::One("a".into())));
        assert_eq!(
            p.attributes,
            Some(ProjectionList::Many(vec![
                "x".into(),
                "y".into(),
                "z".into()
            ]))
        );
    }

    #[test]
    fn unrelated_keys_are_not_forwarded() {
        let p = parse("tabId=t&properties=a&other=1").unwrap();
        assert_eq!(p.http_path("e"), "/control/element/e/state?properties=a");
    }

    #[test]
    fn percent_and_plus_decode_and_reencode_losslessly() {
        let p = parse("properties=a%2Cb+c%26d").unwrap();
        assert_eq!(p.properties, Some(ProjectionList::One("a,b c&d".into())));
        assert_eq!(
            p.http_path("e"),
            "/control/element/e/state?properties=a%2Cb%20c%26d"
        );
        let again = parse(p.http_path("e").split_once('?').unwrap().1).unwrap();
        assert_eq!(again, p);
    }

    #[test]
    fn empty_value_is_forwarded_as_empty_string() {
        let p = parse("properties=").unwrap();
        assert_eq!(p.properties, Some(ProjectionList::One(String::new())));
    }

    #[test]
    fn cap_refuses_over_and_accepts_sdk_bound() {
        let ok = format!("properties={}", "a".repeat(4_160));
        assert!(StateProjection::from_raw_query(Some(&ok))
            .unwrap()
            .is_some());
        let big = format!("properties={}", "a".repeat(MAX_PROJECTION_BYTES + 1));
        let err = StateProjection::from_raw_query(Some(&big)).unwrap_err();
        assert_eq!(err.to_json()["code"], REFUSAL_CODE);
        assert_eq!(err.to_json()["success"], false);
        // Both keys count together toward the cap.
        let two = format!(
            "properties={}&attributes={}",
            "a".repeat(5_000),
            "b".repeat(5_000)
        );
        assert!(StateProjection::from_raw_query(Some(&two)).is_err());
    }

    #[test]
    fn ipc_args_carry_element_id_and_keys() {
        let p = parse("properties=a&attributes=lang").unwrap();
        assert_eq!(
            ipc_args_for(Some(&p), "x"),
            json!({ "elementId": "x", "properties": "a", "attributes": "lang" })
        );
    }
}
