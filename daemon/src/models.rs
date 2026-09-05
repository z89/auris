//! Apple product-id -> human model name. Ids are the DID/advert product id in
//! uppercase hex, four digits.

/// Known AirPods-family product ids.
pub const MODELS: &[(&str, &str)] = &[
    ("2002", "AirPods"),
    ("200F", "AirPods 2"),
    ("2013", "AirPods 3"),
    ("2019", "AirPods 4"),
    ("201B", "AirPods 4 (ANC)"),
    ("200E", "AirPods Pro"),
    ("2014", "AirPods Pro 2"),
    ("2024", "AirPods Pro 2 (USB-C)"),
    ("200A", "AirPods Max"),
    ("201F", "AirPods Max (USB-C)"),
];

/// Apple's Bluetooth SIG vendor id, as it appears in a DID modalias.
pub const APPLE_VENDOR_ID: u32 = 0x004C;

/// Format a DID product id the way the contract wants it: uppercase hex, four
/// digits.
pub fn model_id(product: u32) -> String {
    format!("{product:04X}")
}

/// Look up a human model name. Returns `None` for anything not in the table,
/// which the contract renders as `"model": null`.
pub fn model_name(id: &str) -> Option<&'static str> {
    let id = id.trim().to_ascii_uppercase();
    MODELS.iter().find(|(k, _)| *k == id).map(|(_, v)| *v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_models_present() {
        for id in [
            "2002", "200F", "2013", "2019", "201B", "200E", "2014", "2024", "200A", "201F",
        ] {
            assert!(model_name(id).is_some(), "missing model {id}");
        }
        assert_eq!(model_name("201b"), Some("AirPods 4 (ANC)"));
        assert_eq!(model_name("FFFF"), None);
    }

    #[test]
    fn model_id_is_uppercase_hex() {
        assert_eq!(model_id(0x201B), "201B");
        assert_eq!(model_id(0x2002), "2002");
    }
}
