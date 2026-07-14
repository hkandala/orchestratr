//! Small shared helpers for pulling typed values out of a request's `params` object.
//! One home for the extraction idioms the method handlers used to repeat by hand.

use serde_json::Value;

/// Extract a string param (`None` when absent or not a string).
pub(super) fn str_param(params: &Value, key: &str) -> Option<String> {
    params.get(key).and_then(|v| v.as_str()).map(String::from)
}

/// Extract a string-array param as `Vec<String>` (empty when absent or not an array);
/// non-string elements are dropped.
pub(super) fn str_array_param(params: &Value, key: &str) -> Vec<String> {
    params
        .get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}
