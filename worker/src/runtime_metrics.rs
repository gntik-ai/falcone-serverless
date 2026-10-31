//! Runtime metrics v1 catalogue and bounded-cardinality validation.

const CATALOGUE: &str = include_str!("../../docs/contracts/runtime-metrics/v1.txt");

pub const RUNTIME_MODE_LABEL: &str = "runtime_mode";
pub const PHASE_LABEL: &str = "phase";
pub const WASM: &str = "wasm";
pub const OCI: &str = "oci";
pub const IMAGE_PULL: &str = "image_pull";
pub const POD_START: &str = "pod_start";
pub const INIT: &str = "init";
pub const CONTRACT_PROBE: &str = "contract_probe";
pub const COLD_START_PHASE_SECONDS: &str = "falcone_serverless_cold_start_phase_seconds";

#[derive(Debug, PartialEq, Eq)]
pub enum CatalogueError {
    Malformed,
    UnknownLabel,
    SeriesInventory,
    PhaseValues,
    RuntimeModeValues,
    MissingLabel,
    BudgetMismatch,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Label<'a> {
    pub name: &'a str,
    pub values: Vec<&'a str>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Series<'a> {
    pub name: &'a str,
    pub kind: &'a str,
    pub unit: &'a str,
    pub labels: Vec<&'a str>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Catalogue<'a> {
    pub labels: Vec<Label<'a>>,
    pub series: Vec<Series<'a>>,
    pub budget: usize,
}

impl<'a> Catalogue<'a> {
    pub fn label_values(&self, name: &str) -> Option<&[&'a str]> {
        self.labels
            .iter()
            .find(|label| label.name == name)
            .map(|label| label.values.as_slice())
    }

    pub fn label_combinations(&self) -> Result<usize, CatalogueError> {
        self.series.iter().try_fold(0usize, |total, series| {
            let product = series.labels.iter().try_fold(1usize, |product, name| {
                let values = self
                    .label_values(name)
                    .ok_or(CatalogueError::MissingLabel)?;
                product
                    .checked_mul(values.len())
                    .ok_or(CatalogueError::BudgetMismatch)
            })?;
            total
                .checked_add(product)
                .ok_or(CatalogueError::BudgetMismatch)
        })
    }
}

pub fn validate(text: &str) -> Result<Catalogue<'_>, CatalogueError> {
    let mut contract = false;
    let mut labels = Vec::new();
    let mut series = Vec::new();
    let mut budget = None;
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<_> = line.split_whitespace().collect();
        match fields.as_slice() {
            ["contract", "ctr-runtime-metrics", "v1"] if !contract => contract = true,
            ["label", name, values @ ..] if !values.is_empty() => {
                if labels.iter().any(|label: &Label<'_>| label.name == *name) {
                    return Err(CatalogueError::Malformed);
                }
                labels.push(Label {
                    name,
                    values: values.to_vec(),
                });
            }
            ["series", name, kind, unit, names @ ..] => series.push(Series {
                name,
                kind,
                unit,
                labels: names.to_vec(),
            }),
            ["budget", value] if budget.is_none() => {
                budget = Some(value.parse().map_err(|_| CatalogueError::Malformed)?);
            }
            _ => return Err(CatalogueError::Malformed),
        }
    }
    if !contract {
        return Err(CatalogueError::Malformed);
    }
    let catalogue = Catalogue {
        labels,
        series,
        budget: budget.ok_or(CatalogueError::Malformed)?,
    };
    if catalogue
        .labels
        .iter()
        .any(|label| !["runtime_mode", "phase"].contains(&label.name))
        || catalogue.series.iter().any(|series| {
            series
                .labels
                .iter()
                .any(|name| !["runtime_mode", "phase"].contains(name))
        })
    {
        return Err(CatalogueError::UnknownLabel);
    }
    if catalogue.series.len() != 1 {
        return Err(CatalogueError::SeriesInventory);
    }
    if ["runtime_mode", "phase"]
        .iter()
        .any(|name| catalogue.label_values(name).is_none())
        || catalogue.series.iter().any(|series| {
            ["runtime_mode", "phase"]
                .iter()
                .any(|name| !series.labels.contains(name))
        })
    {
        return Err(CatalogueError::MissingLabel);
    }
    let series = &catalogue.series[0];
    if series.name != "falcone_serverless_cold_start_phase_seconds"
        || series.kind != "histogram"
        || series.unit != "seconds"
        || series.labels != ["runtime_mode", "phase"]
    {
        return Err(CatalogueError::SeriesInventory);
    }
    if catalogue.label_values("phase")
        != Some(["image_pull", "pod_start", "init", "contract_probe"].as_slice())
    {
        return Err(CatalogueError::PhaseValues);
    }
    if catalogue.label_values("runtime_mode") != Some(["wasm", "oci"].as_slice()) {
        return Err(CatalogueError::RuntimeModeValues);
    }
    if catalogue.budget != 8 || catalogue.label_combinations()? != catalogue.budget {
        return Err(CatalogueError::BudgetMismatch);
    }
    Ok(catalogue)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn committed_catalogue_validates() {
        assert!(validate(CATALOGUE).is_ok());
    }

    #[test]
    fn runtime_mode_values_are_wasm_and_oci() {
        let catalogue = validate(CATALOGUE).expect("valid catalogue");
        assert_eq!(
            catalogue.label_values("runtime_mode"),
            Some(["wasm", "oci"].as_slice())
        );
    }

    #[test]
    fn oci_phases_are_the_four_cold_start_phases() {
        let catalogue = validate(CATALOGUE).expect("valid catalogue");
        assert_eq!(
            catalogue.label_values("phase"),
            Some(["image_pull", "pod_start", "init", "contract_probe"].as_slice())
        );
    }

    #[test]
    fn series_inventory_is_exact() {
        let catalogue = validate(CATALOGUE).expect("valid catalogue");
        assert_eq!(
            catalogue.series,
            [Series {
                name: "falcone_serverless_cold_start_phase_seconds",
                kind: "histogram",
                unit: "seconds",
                labels: vec!["runtime_mode", "phase"],
            }]
        );
    }

    #[test]
    fn series_budget_is_bounded_by_8() {
        let catalogue = validate(CATALOGUE).expect("valid catalogue");
        assert_eq!(catalogue.budget, 8);
        assert_eq!(catalogue.label_combinations(), Ok(8));
    }

    #[test]
    fn validator_rejects_single_invariant_mutations() {
        assert!(validate(CATALOGUE).is_ok());
        let mutated = format!("{CATALOGUE}label function_id arbitrary\n");
        assert_ne!(mutated, CATALOGUE);
        assert_eq!(validate(&mutated), Err(CatalogueError::UnknownLabel));
        let mutated = CATALOGUE.replace(
            "histogram seconds runtime_mode phase",
            "histogram seconds phase",
        );
        assert_ne!(mutated, CATALOGUE);
        assert_eq!(validate(&mutated), Err(CatalogueError::MissingLabel));
        let mutated = format!("{CATALOGUE}series extra histogram seconds phase\n");
        assert_ne!(mutated, CATALOGUE);
        assert_eq!(validate(&mutated), Err(CatalogueError::SeriesInventory));
        let mutated =
            CATALOGUE.replace("init contract_probe", "init contract_probe pod_scheduling");
        assert_ne!(mutated, CATALOGUE);
        assert_eq!(validate(&mutated), Err(CatalogueError::PhaseValues));
        let mutated = CATALOGUE.replace(" contract_probe", "");
        assert_ne!(mutated, CATALOGUE);
        assert_eq!(validate(&mutated), Err(CatalogueError::PhaseValues));
        let mutated = CATALOGUE.replace("wasm oci", "wasi oci");
        assert_ne!(mutated, CATALOGUE);
        assert_eq!(validate(&mutated), Err(CatalogueError::RuntimeModeValues));
        let mutated = CATALOGUE.replace("budget 8", "budget 9");
        assert_ne!(mutated, CATALOGUE);
        assert_eq!(validate(&mutated), Err(CatalogueError::BudgetMismatch));
        let mutated = format!("{CATALOGUE}unknown keyword\n");
        assert_ne!(mutated, CATALOGUE);
        assert_eq!(validate(&mutated), Err(CatalogueError::Malformed));
    }

    #[test]
    fn constants_match_catalogue() {
        let catalogue = validate(CATALOGUE).expect("valid catalogue");
        assert_eq!(RUNTIME_MODE_LABEL, "runtime_mode");
        assert_eq!(PHASE_LABEL, "phase");
        assert_eq!([WASM, OCI], ["wasm", "oci"]);
        assert_eq!(
            [IMAGE_PULL, POD_START, INIT, CONTRACT_PROBE],
            ["image_pull", "pod_start", "init", "contract_probe"]
        );
        assert_eq!(
            COLD_START_PHASE_SECONDS,
            "falcone_serverless_cold_start_phase_seconds"
        );
        assert_eq!(
            catalogue
                .labels
                .iter()
                .map(|label| label.name)
                .collect::<Vec<_>>(),
            [RUNTIME_MODE_LABEL, PHASE_LABEL]
        );
        assert_eq!(
            catalogue.label_values(RUNTIME_MODE_LABEL),
            Some([WASM, OCI].as_slice())
        );
        assert_eq!(
            catalogue.label_values(PHASE_LABEL),
            Some([IMAGE_PULL, POD_START, INIT, CONTRACT_PROBE].as_slice())
        );
        assert_eq!(catalogue.series[0].name, COLD_START_PHASE_SECONDS);
        assert_eq!(
            catalogue.series[0].labels,
            [RUNTIME_MODE_LABEL, PHASE_LABEL]
        );
    }
}
