// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ENVIRONMENT THE HARNESS READS: the process's own, taken when it is needed
//! ([`Env::process`]), or one a test builds ([`Env::from_pairs`]), so every rule that reads a
//! variable is tested without touching the real environment.
//!
//! It is a table, not a `&dyn Fn(&str) -> Option<OsString>` reader (the seam v0.1.0 to v0.3.0
//! used): a call through a trait object names no body, so the verified lane refuses every rule
//! that made one (an absent callee that may panic), while a lookup in this table is a body it
//! reads.

use std::ffi::OsString;

/// Environment variables, in the order they were given. A name given twice reads as its first
/// value, as `getenv(3)` reads a process environment that holds a name twice.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Env {
    vars: Vec<(OsString, OsString)>,
}

impl Env {
    /// This process's environment, now.
    #[must_use]
    pub fn process() -> Env {
        Env {
            vars: std::env::vars_os().collect(),
        }
    }

    /// An environment of exactly `pairs`.
    #[must_use]
    pub fn from_pairs<I, K, V>(pairs: I) -> Env
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<OsString>,
        V: Into<OsString>,
    {
        Env {
            vars: pairs
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        }
    }

    /// The value of `name`, or `None` when it is not set.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<OsString> {
        for (k, v) in &self.vars {
            if k == name {
                return Some(v.clone());
            }
        }
        None
    }

    /// This environment with `shim` set over it: what a child started with `shim` added reads.
    /// A name in `shim` wins over the same name here.
    #[must_use]
    pub fn over(&self, shim: &[(String, String)]) -> Env {
        let mut vars: Vec<(OsString, OsString)> = shim
            .iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v)))
            .collect();
        vars.extend(self.vars.iter().cloned());
        Env { vars }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_reads_its_first_value_and_an_unset_one_reads_none() {
        let env = Env::from_pairs([("A", "1"), ("B", ""), ("A", "2")]);
        assert_eq!(env.get("A"), Some(OsString::from("1")));
        assert_eq!(env.get("B"), Some(OsString::new()));
        assert_eq!(env.get("C"), None);
        assert_eq!(Env::default().get("A"), None);
    }

    #[test]
    fn the_shim_wins_over_the_environment_it_is_set_over() {
        let own = Env::from_pairs([("GROK_HOME", "/mine"), ("HOME", "/h")]);
        let child = own.over(&[("GROK_HOME".to_string(), "/from/shim".to_string())]);
        assert_eq!(child.get("GROK_HOME"), Some(OsString::from("/from/shim")));
        assert_eq!(child.get("HOME"), Some(OsString::from("/h")));
        assert_eq!(
            own.get("GROK_HOME"),
            Some(OsString::from("/mine")),
            "own is unchanged"
        );
    }

    #[test]
    fn the_process_environment_reads_as_std_reads_it() {
        let env = Env::process();
        for name in ["PATH", "HOME", "GROK_ALAB_SURELY_UNSET_VARIABLE"] {
            assert_eq!(env.get(name), std::env::var_os(name), "{name}");
        }
    }
}
