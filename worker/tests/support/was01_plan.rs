//! The kubectl calls of the WAS-01 evidence run, and the ac1 check that they stay inside the
//! run's own namespace.
//!
//! The check is an allow-list of exact call shapes, not a search for forbidden words:
//! kubectl ignores `-n` for `--raw` and for cluster-scoped kinds, so "contains `-n <run>`"
//! would admit `-n <run> get nodes`. A call is allowed only if it is one of
//!
//! - `get --raw /version`
//! - `create namespace <run>`
//! - `delete namespace <run> --wait`
//! - `auth can-i <listed verb> <listed resource> -n <run>`
//!
//! The kubeconfig and request-timeout flags are added by the runner, never by the plan.

use super::cluster_evidence::{self, RESOURCES, VERBS};
use super::{Namespace, namespace_name};

pub const NAMESPACE_PREFIX: &str = "fs-test-was01-";

pub type Call = Vec<String>;

/// A fresh `fs-test-was01-<random>` namespace name: an fs-test DNS-1123 label of at most 63 characters.
pub fn run_namespace() -> Result<Namespace, String> {
    checked_namespace(&namespace_name("was01"))
}

pub fn checked_namespace(name: &str) -> Result<Namespace, String> {
    if !name.starts_with(NAMESPACE_PREFIX) {
        return Err("WAS-01 evidence namespace must start with fs-test-was01-".into());
    }
    Namespace::new(name)
}

fn call(parts: &[&str]) -> Call {
    parts.iter().map(|part| (*part).to_owned()).collect()
}

pub fn create_call(namespace: &Namespace) -> Call {
    call(&["create", "namespace", namespace.name()])
}

pub fn delete_call(namespace: &Namespace) -> Call {
    call(&["delete", "namespace", namespace.name(), "--wait"])
}

pub fn version_call() -> Call {
    call(&["get", "--raw", "/version"])
}

pub fn can_i_call(namespace: &Namespace, verb: &str, resource: &str) -> Call {
    call(&["auth", "can-i", verb, resource, "-n", namespace.name()])
}

/// Create, read the version, probe every listed verb and resource, delete.
pub fn build_plan(namespace: &Namespace) -> Vec<Call> {
    let mut plan = vec![create_call(namespace), version_call()];
    for resource in RESOURCES {
        for verb in VERBS {
            plan.push(can_i_call(namespace, verb, resource));
        }
    }
    plan.push(delete_call(namespace));
    plan
}

/// The item id a `can-i` call reports, if the call is one.
pub fn can_i_item_of(call: &[String]) -> Option<&'static str> {
    match call {
        [auth, can_i, verb, resource, flag, _]
            if auth == "auth" && can_i == "can-i" && flag == "-n" =>
        {
            cluster_evidence::can_i_item(verb, resource)
        }
        _ => None,
    }
}

/// Whether one call is an exact allowed shape for this run's namespace.
pub fn call_allowed(namespace: &Namespace, call: &[String]) -> bool {
    if !namespace.name().starts_with(NAMESPACE_PREFIX) {
        return false;
    }
    if call == version_call() || call == create_call(namespace) || call == delete_call(namespace) {
        return true;
    }
    can_i_item_of(call).is_some() && call.last().map(String::as_str) == Some(namespace.name())
}

/// ac1 `plan-namespaced-only`: every call is allowed, and the plan is not empty.
pub fn plan_namespaced_only(namespace: &Namespace, calls: &[Call]) -> Result<(), &'static str> {
    if calls.is_empty() || !calls.iter().all(|call| call_allowed(namespace, call)) {
        return Err("plan-namespaced-only failed: a call is outside this run's namespace");
    }
    Ok(())
}
