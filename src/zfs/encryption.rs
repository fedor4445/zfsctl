//! Native encryption as `zfs get` reports it: which datasets are encrypted, with what, and whether their key is loaded.
//! Reads only. Only encrypted datasets are kept (`encryption` is `off` on the rest, `-` on a zfs that has no such property).

use crate::exec::CommandRunner;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Enc {
    /// `aes-256-gcm`...
    pub cipher: String,
    /// `passphrase`, `raw` or `hex`.
    pub key_format: String,
    /// The dataset whose key this one uses (itself, or an ancestor).
    pub root: String,
    /// `keystatus` is `unavailable`: the data cannot be read until the key is loaded.
    pub locked: bool,
}

const PROPS: &str = "encryption,keyformat,keystatus,encryptionroot";

/// `zfs get` for every filesystem and volume.
pub const GET_ARGS: [&str; 7] = ["get", "-H", "-t", "filesystem,volume", "-o", "name,property,value", PROPS];

/// Parses `zfs get -H -o name,property,value <PROPS>` into the encrypted datasets.
pub fn parse(output: &str) -> BTreeMap<String, Enc> {
    let mut seen: BTreeMap<String, Enc> = BTreeMap::new();
    let mut encrypted: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for line in output.lines() {
        // A dataset may be named like a property, so the value is the last field and the property the one before it.
        let fields: Vec<&str> = line.split('\t').collect();
        let [name, property, value] = fields.as_slice() else { continue };
        let entry = seen.entry((*name).to_string()).or_default();
        match *property {
            "encryption" if !matches!(*value, "off" | "-") => {
                encrypted.insert((*name).to_string());
                entry.cipher = (*value).to_string();
            }
            "keyformat" => entry.key_format = (*value).to_string(),
            "keystatus" => entry.locked = *value == "unavailable",
            "encryptionroot" if *value != "-" => entry.root = (*value).to_string(),
            _ => {}
        }
    }
    seen.into_iter().filter(|(name, _)| encrypted.contains(name)).collect()
}

/// Every encrypted dataset of the host; empty when the host cannot say (an old `zfs`, a refusal, a dropped connection).
pub fn list(runner: &dyn CommandRunner) -> BTreeMap<String, Enc> {
    runner.run("zfs", &GET_ARGS).ok().filter(crate::exec::CommandOutput::success).map(|o| parse(&o.stdout)).unwrap_or_default()
}

/// One dataset's encryption. `Err` only when the host could not be asked at all (the runner failed or ssh said 255); an
/// error answer (an old `zfs` without these properties, a refusal) is `Ok(None)`, as is a dataset that is not encrypted.
pub fn query(runner: &dyn CommandRunner, dataset: &str) -> std::result::Result<Option<Enc>, String> {
    if dataset.starts_with('-') {
        return Err(format!("not a dataset name: {dataset}"));
    }
    let args = ["get", "-H", "-o", "name,property,value", PROPS, dataset];
    match runner.run("zfs", &args) {
        Err(e) => Err(e.to_string()),
        Ok(o) if o.status == 255 => Err(o.stderr.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("could not reach the host").to_string()),
        Ok(o) if !o.success() => Ok(None),
        Ok(o) => Ok(parse(&o.stdout).remove(dataset)),
    }
}

/// The Lock column: blank, `open` or `LOCKED`.
pub fn lock_text(enc: Option<&Enc>) -> &'static str {
    match enc {
        None => "",
        Some(e) if e.locked => "LOCKED",
        Some(_) => "open",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;
    use crate::exec::{CommandOutput, CommandRunner};

    const CAPTURE: &str = "tank/secure\tencryption\taes-256-gcm\ntank/secure\tkeyformat\tpassphrase\ntank/secure\tkeystatus\tavailable\ntank/secure\tencryptionroot\ttank/secure\n\
tank/secure/kid\tencryption\taes-256-gcm\ntank/secure/kid\tkeyformat\tpassphrase\ntank/secure/kid\tkeystatus\tunavailable\ntank/secure/kid\tencryptionroot\ttank/secure\n\
tank/plain\tencryption\toff\ntank/plain\tkeyformat\tnone\ntank/plain\tkeystatus\t-\ntank/plain\tencryptionroot\t-\n\
keystatus\tencryption\toff\nkeystatus\tkeystatus\t-\nold\tencryption\t-\n";

    #[test]
    fn only_encrypted_datasets_are_kept() {
        let got = parse(CAPTURE);
        assert_eq!(got.keys().map(String::as_str).collect::<Vec<_>>(), ["tank/secure", "tank/secure/kid"]);
        assert_eq!(got["tank/secure"], Enc { cipher: "aes-256-gcm".into(), key_format: "passphrase".into(), root: "tank/secure".into(), locked: false });
        assert!(got["tank/secure/kid"].locked && got["tank/secure/kid"].root == "tank/secure");
    }

    #[test]
    fn short_or_blank_lines_and_a_dataset_named_like_a_property_are_harmless() {
        assert!(parse("").is_empty());
        assert!(parse("tank\tencryption\n\nnonsense\n\t\t\n").is_empty());
        let got = parse("keystatus\tencryption\taes-128-ccm\nkeystatus\tkeystatus\tunavailable\n");
        assert!(got["keystatus"].locked && got["keystatus"].cipher == "aes-128-ccm");
    }

    #[test]
    fn query_tells_a_dropped_connection_from_an_old_zfs() {
        struct Broken;
        impl CommandRunner for Broken {
            fn run(&self, _c: &str, _a: &[&str]) -> crate::error::Result<CommandOutput> {
                Err(crate::error::AppError::Config("ssh to the host failed".to_string()))
            }
            fn run_with_stdin(&self, c: &str, a: &[&str], _s: &str) -> crate::error::Result<CommandOutput> {
                self.run(c, a)
            }
            fn run_unprivileged(&self, c: &str, a: &[&str]) -> crate::error::Result<CommandOutput> {
                self.run(c, a)
            }
        }
        assert!(query(&Broken, "tank/a").unwrap_err().contains("ssh to the host failed"));
        assert_eq!(query(&FakeRunner::failing("Connection closed", 255), "tank/a").unwrap_err(), "Connection closed");
        assert_eq!(query(&FakeRunner::failing("bad property list: invalid property 'keystatus'", 1), "tank/a"), Ok(None));
        assert_eq!(query(&FakeRunner::new(CAPTURE), "tank/plain"), Ok(None));
        assert!(query(&FakeRunner::new(CAPTURE), "tank/secure").unwrap().is_some());
        assert!(query(&FakeRunner::new(""), "-evil").is_err());
    }

    #[test]
    fn list_is_empty_when_the_host_cannot_say() {
        assert!(list(&FakeRunner::failing("no", 1)).is_empty());
        assert_eq!(list(&FakeRunner::new(CAPTURE)).len(), 2);
    }

    #[test]
    fn lock_text_has_three_states() {
        let open = Enc::default();
        let locked = Enc { locked: true, ..Enc::default() };
        assert_eq!((lock_text(None), lock_text(Some(&open)), lock_text(Some(&locked))), ("", "open", "LOCKED"));
    }
}
