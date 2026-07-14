//! Small shared helpers for pulling typed values out of a request's `params` object.
//! One home for the extraction idioms the method handlers used to repeat by hand.

use serde_json::Value;

/// Extract a string param (`None` when absent or not a string).
pub(super) fn str_param(params: &Value, key: &str) -> Option<String> {
    params.get(key).and_then(|v| v.as_str()).map(String::from)
}

/// Collect a JSON array `Value` into `Vec<String>`, dropping non-string elements
/// (absent/non-array → empty). Shared by the server params and the CLI's argv/command plumbing.
pub(crate) fn str_array(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Extract a string-array param as `Vec<String>` (empty when absent or not an array);
/// non-string elements are dropped.
pub(super) fn str_array_param(params: &Value, key: &str) -> Vec<String> {
    params.get(key).map(str_array).unwrap_or_default()
}
