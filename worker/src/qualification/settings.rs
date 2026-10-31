//! Pinned engine controls for WAS-01, not production worker policy.

pub const CONSUME_FUEL: bool = true;
pub const EPOCH_INTERRUPTION: bool = true;
pub const FUEL_LIMIT: u64 = 10_000_000;
pub const EPOCH_TICK_MS: u64 = 10;
pub const EPOCH_DEADLINE_TICKS: u64 = 100;
pub const EPOCH_RUN_FUEL: u64 = u64::MAX;
pub const FUEL_RUN_EPOCH_DEADLINE: u64 = u64::MAX;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_records_every_engine_constant() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("qualification/was-01/versions.toml");
        let manifest: toml::Value =
            toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let table = manifest["engine"].as_table().unwrap();
        assert_eq!(table.len(), 7);
        assert_eq!(table["consume_fuel"].as_bool(), Some(CONSUME_FUEL));
        assert_eq!(
            table["epoch_interruption"].as_bool(),
            Some(EPOCH_INTERRUPTION)
        );
        for (key, value) in [
            ("fuel_limit", FUEL_LIMIT),
            ("epoch_tick_ms", EPOCH_TICK_MS),
            ("epoch_deadline_ticks", EPOCH_DEADLINE_TICKS),
        ] {
            assert_eq!(table[key].as_integer().unwrap() as u64, value, "{key}");
        }
        for (key, value) in [
            ("epoch_run_fuel", EPOCH_RUN_FUEL),
            ("fuel_run_epoch_deadline", FUEL_RUN_EPOCH_DEADLINE),
        ] {
            assert_eq!(
                table[key].as_str().unwrap().parse::<u64>().unwrap(),
                value,
                "{key}"
            );
            assert_eq!(value, u64::MAX);
        }
    }
}
