use json::JsonValue;

/// We support a variety of true-ish values.
///
/// True if the value is a non-zero number, a string that starts with
/// "t/T", or a JsonValue::Bool(true).  False otherwise.
///
/// ```
/// assert!(!evergreen::util::json_bool(&json::from(vec!["true"])));
/// assert!(evergreen::util::json_bool(&json::from("trooo")));
/// assert!(!evergreen::util::json_bool(&json::from(0i8)));
/// assert!(!evergreen::util::json_bool(&json::from(false)));

/// ```

pub fn json_bool(value: &JsonValue) -> bool {
    if let Some(n) = value.as_i64() {
        n != 0
    } else if let Some(n) = value.as_f64() {
        n != 0.0
    } else if let Some(s) = value.as_str() {
        s.len() > 0 && (s[..1].eq("t") || s[..1].eq("T"))
    } else if let Some(b) = value.as_bool() {
        b
    } else {
        false
    }
}

/// Translate a number-ish thing into a float.
///
/// Returns an error if the value cannot be numerified.
///
/// ```
/// assert!(evergreen::util::json_float(&json::JsonValue::new_array()).is_err());
///
/// let res = evergreen::util::json_float(&json::from("1.2"));
/// assert_eq!(res.unwrap(), 1.2);
///
/// let res = evergreen::util::json_float(&json::from(0));
/// assert_eq!(res.unwrap(), 0.0);
/// ```
pub fn json_float(value: &JsonValue) -> Result<f64, String> {
    if let Some(n) = value.as_f64() {
        return Ok(n);
    } else if let Some(s) = value.as_str() {
        if let Ok(n) = s.parse::<f64>() {
            return Ok(n);
        }
    }
    Err(format!("Invalid float value: {}", value))
}

/// Translate a number-ish thing into a signed int.
///
/// Returns an error if the value cannot be numerified.
/// ```
/// let res = evergreen::util::json_int(&json::JsonValue::new_array());
/// assert!(res.is_err());
///
/// let res = evergreen::util::json_int(&json::from("-11"));
/// assert_eq!(res.unwrap(), -11);
///
/// let res = evergreen::util::json_int(&json::from(12));
/// assert_eq!(res.unwrap(), 12);

pub fn json_int(value: &JsonValue) -> Result<i64, String> {
    if let Some(n) = value.as_i64() {
        return Ok(n);
    } else if let Some(s) = value.as_str() {
        if let Ok(n) = s.parse::<i64>() {
            return Ok(n);
        }
    }
    Err(format!("Invalid int value: {}", value))
}

