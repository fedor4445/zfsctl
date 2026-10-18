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

/// Every encrypted dataset of the host. `None` when the host could not be asked (a failed runner, ssh's 255): the caller keeps
/// what it knew. An error answer (an old `zfs` without the properties, a refusal) is an empty list.
pub fn list(runner: &dyn CommandRunner) -> Option<BTreeMap<String, Enc>> {
    match runner.run("zfs", &GET_ARGS) {
        Err(_) => None,
        Ok(o) if o.status == 255 => None,
        Ok(o) if o.success() => Some(parse(&o.stdout)),
        Ok(_) => Some(BTreeMap::new()),
    }
}

/// Whether a failed `zfs get` says the host's `zfs` does not know these properties (so nothing is encrypted there).
fn unknown_property(stderr: &str) -> bool {
    let text = stderr.to_lowercase();
    text.contains("bad property list") || text.contains("invalid property") || text.contains("unknown property")
}

/// One dataset's encryption. `Ok(None)` for a dataset that is not encrypted and for a `zfs` that says it does not know the
/// properties. Anything else that went wrong (the runner failed, ssh's 255, a signal, a refusal, an unrecognised error) is `Err`:
/// the caller must not read "could not ask" as "not encrypted".
pub fn query(runner: &dyn CommandRunner, dataset: &str) -> std::result::Result<Option<Enc>, String> {
    if dataset.starts_with('-') {
        return Err(format!("not a dataset name: {dataset}"));
    }
    let args = ["get", "-H", "-o", "name,property,value", PROPS, dataset];
    match runner.run("zfs", &args) {
        Err(e) => Err(e.to_string()),
        Ok(o) if o.status == 255 => Err(o.stderr.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("could not reach the host").to_string()),
        Ok(o) if !o.success() && unknown_property(&o.stderr) => Ok(None),
        Ok(o) if !o.success() => Err(o.stderr.lines().map(str::trim).find(|l| !l.is_empty()).map_or_else(|| format!("zfs get failed (status {})", o.status), str::to_string)),
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

/// The longest passphrase `zfs` takes, in bytes.
const MAX_PASSPHRASE: usize = 512;

/// What the form checks before a passphrase is sent. It travels on one line of stdin, so it cannot hold a line break;
/// a new key needs 8 characters (zfs's own minimum, said here so the form can say it first).
pub fn check_passphrase(passphrase: &str, new_key: bool) -> std::result::Result<(), String> {
    if passphrase.is_empty() {
        return Err("Enter the passphrase".to_string());
    }
    if passphrase.contains(['\n', '\r']) {
        return Err("A passphrase cannot contain a line break".to_string());
    }
    if passphrase.len() > MAX_PASSPHRASE {
        return Err(format!("A passphrase can be at most {MAX_PASSPHRASE} bytes"));
    }
    if new_key && passphrase.chars().count() < 8 {
        return Err("A new passphrase needs at least 8 characters".to_string());
    }
    Ok(())
}

fn key_command(runner: &dyn CommandRunner, verb: &str, dataset: &str, passphrase: Option<&str>) -> crate::error::Result<()> {
    if dataset.starts_with('-') {
        return Err(crate::error::AppError::Parse(format!("not a dataset name: {dataset}")));
    }
    let args = [verb, "--", dataset];
    let out = match passphrase {
        // The input is a copy of the secret: it is wiped as soon as the call returns.
        Some(p) => runner.run_with_stdin("zfs", &args, zeroize::Zeroizing::new(format!("{p}\n")).as_str())?,
        None => runner.run("zfs", &args)?,
    };
    if out.success() {
        return Ok(());
    }
    let mut stderr = out.stderr;
    if stderr.contains("is busy") {
        stderr = format!("{}\nThe dataset is in use: unmount it, and everything under it, first.", stderr.trim_end());
    }
    Err(crate::error::AppError::CommandFailed { cmd: format!("zfs {verb} {dataset}"), status: out.status, stderr })
}

/// `zfs load-key`: the passphrase is the key's one line on stdin. Only the encryption root can be loaded.
pub fn load_key(runner: &dyn CommandRunner, dataset: &str, passphrase: &str) -> crate::error::Result<()> {
    key_command(runner, "load-key", dataset, Some(passphrase))
}

/// `zfs unload-key`: zfs refuses while the dataset is mounted, and its message is passed on.
pub fn unload_key(runner: &dyn CommandRunner, dataset: &str) -> crate::error::Result<()> {
    key_command(runner, "unload-key", dataset, None)
}

/// `zfs change-key` with a new passphrase on stdin. The data is not re-encrypted: the old passphrase simply stops working.
pub fn change_key(runner: &dyn CommandRunner, dataset: &str, passphrase: &str) -> crate::error::Result<()> {
    key_command(runner, "change-key", dataset, Some(passphrase))
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
        assert!(query(&FakeRunner::failing("", -1), "tank/a").unwrap_err().contains("status -1"), "killed by a signal: could not ask");
        assert!(query(&FakeRunner::failing("sudo: a password is required", 1), "tank/a").is_err(), "a refusal is not an answer either");
        assert_eq!(query(&FakeRunner::new(CAPTURE), "tank/plain"), Ok(None));
        assert!(query(&FakeRunner::new(CAPTURE), "tank/secure").unwrap().is_some());
        assert!(query(&FakeRunner::new(""), "-evil").is_err());
    }

    #[test]
    fn list_is_empty_when_the_host_cannot_say() {
        assert_eq!(list(&FakeRunner::failing("no", 1)), Some(BTreeMap::new()), "an error answer is an empty list");
        assert_eq!(list(&FakeRunner::failing("closed", 255)), None, "a lost connection is not an answer");
        assert_eq!(list(&FakeRunner::new(CAPTURE)).unwrap().len(), 2);
    }

    #[test]
    fn lock_text_has_three_states() {
        let open = Enc::default();
        let locked = Enc { locked: true, ..Enc::default() };
        assert_eq!((lock_text(None), lock_text(Some(&open)), lock_text(Some(&locked))), ("", "open", "LOCKED"));
    }

    #[test]
    fn a_passphrase_is_checked_before_it_is_sent() {
        assert!(check_passphrase("", false).is_err());
        assert!(check_passphrase("two\nlines", false).is_err());
        assert!(check_passphrase("with\rreturn", false).is_err());
        assert!(check_passphrase("short", false).is_ok(), "loading takes whatever the key is");
        assert!(check_passphrase("short", true).unwrap_err().contains("8"), "a new key needs 8 characters");
        assert!(check_passphrase("long enough", true).is_ok());
        assert!(check_passphrase(&"x".repeat(513), true).is_err(), "zfs takes at most 512 bytes");
    }

    #[test]
    fn the_passphrase_goes_through_stdin_and_never_through_the_arguments() {
        let secret = "correct horse battery";
        let runner = FakeRunner::new("");
        load_key(&runner, "tank/secure", secret).unwrap();
        change_key(&runner, "tank/secure", secret).unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zfs".to_string(), vec!["load-key".to_string(), "--".to_string(), "tank/secure".to_string()]));
        assert_eq!(calls[1], ("zfs".to_string(), vec!["change-key".to_string(), "--".to_string(), "tank/secure".to_string()]));
        assert!(calls.iter().all(|(_, args)| args.iter().all(|a| !a.contains(secret))));
        let stdin = runner.stdin_calls.lock().unwrap();
        assert_eq!(stdin.len(), 2);
        assert!(stdin.iter().all(|(cmd, input)| cmd == "zfs" && input == &format!("{secret}\n")));
    }

    #[test]
    fn unload_key_sends_no_stdin() {
        let runner = FakeRunner::new("");
        unload_key(&runner, "tank/secure").unwrap();
        assert_eq!(runner.calls.lock().unwrap()[0].1, vec!["unload-key", "--", "tank/secure"]);
        assert!(runner.stdin_calls.lock().unwrap().is_empty());
    }

    #[test]
    fn a_name_that_looks_like_an_option_is_refused_before_anything_runs() {
        let runner = FakeRunner::new("");
        assert!(load_key(&runner, "-r", "pass phrase").is_err());
        assert!(unload_key(&runner, "-r").is_err());
        assert!(runner.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn zfs_failures_come_back_verbatim_and_busy_gets_a_hint() {
        let wrong = FakeRunner::failing("Key load error: Incorrect key provided for 'tank/secure'.", 255);
        let msg = load_key(&wrong, "tank/secure", "wrong guess").unwrap_err().to_string();
        assert!(msg.contains("Incorrect key provided"), "{msg}");
        assert!(!msg.contains("wrong guess"));
        let busy = FakeRunner::failing("Key unload error: 'tank/secure' is busy.", 255);
        let msg = unload_key(&busy, "tank/secure").unwrap_err().to_string();
        assert!(msg.contains("is busy") && msg.contains("unmount"), "{msg}");
    }
}
