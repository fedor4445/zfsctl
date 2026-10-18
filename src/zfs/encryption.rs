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

/// What the host says about a dataset right before a key is loaded or changed.
struct KeyFacts {
    key_location: String,
    key_format: String,
    root: String,
    can_mount: bool,
}

/// Asks the host afresh (the screen's list may be old). Fails closed: no answer, no key operation.
fn key_facts(runner: &dyn CommandRunner, dataset: &str) -> crate::error::Result<KeyFacts> {
    let args = ["get", "-H", "-o", "property,value", "keylocation,keyformat,encryptionroot,canmount", dataset];
    let out = runner.run("zfs", &args)?;
    if !out.success() {
        return Err(crate::error::AppError::CommandFailed { cmd: format!("zfs get keylocation {dataset}"), status: out.status, stderr: out.stderr });
    }
    let value = |name: &str| out.stdout.lines().find_map(|l| l.split_once('\t').filter(|(k, _)| *k == name).map(|(_, v)| v.trim().to_string()));
    let (Some(key_location), Some(key_format), Some(root)) = (value("keylocation"), value("keyformat"), value("encryptionroot")) else {
        return Err(crate::error::AppError::Parse(format!("{dataset}: the host did not report its key properties")));
    };
    Ok(KeyFacts { key_location, key_format, root, can_mount: value("canmount").as_deref() == Some("on") })
}

/// A typed passphrase only means something when zfs reads the key from the prompt (stdin): a key kept in a file would be
/// read from there and the typed text silently ignored, for `change-key` too.
fn check_prompt_key(facts: &KeyFacts, dataset: &str) -> crate::error::Result<()> {
    let refuse = |why: String| Err(crate::error::AppError::Parse(why));
    if facts.key_location != "prompt" {
        return refuse(format!("{dataset} reads its key from {}, not from a typed passphrase: use zfs at the shell", facts.key_location));
    }
    if facts.key_format != "passphrase" {
        return refuse(format!("{dataset} has a {} key, not a passphrase", facts.key_format));
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

fn load_key_checked(runner: &dyn CommandRunner, dataset: &str, passphrase: &str) -> crate::error::Result<bool> {
    if dataset.starts_with('-') {
        return Err(crate::error::AppError::Parse(format!("not a dataset name: {dataset}")));
    }
    let facts = key_facts(runner, dataset)?;
    check_prompt_key(&facts, dataset)?;
    key_command(runner, "load-key", dataset, Some(passphrase))?;
    Ok(facts.can_mount)
}

/// `load_key`, then `zfs mount` of the same dataset when asked and when it is a mountable filesystem (a volume or a
/// `canmount=off` container is only unlocked). A mount that fails leaves the key loaded and says so.
pub fn load_key_and_mount(runner: &dyn CommandRunner, dataset: &str, passphrase: &str, mount: bool) -> crate::error::Result<()> {
    let can_mount = load_key_checked(runner, dataset, passphrase)?;
    if !mount || !can_mount {
        return Ok(());
    }
    let out = runner.run("zfs", &["mount", "--", dataset])?;
    if out.success() {
        return Ok(());
    }
    Err(crate::error::AppError::CommandFailed {
        cmd: format!("zfs mount {dataset}"),
        status: out.status,
        stderr: format!("{}\nThe key IS loaded; only the mount failed.", out.stderr.trim_end()),
    })
}

/// The ciphers `zfs create -o encryption=` takes (`OpenZFS`), `aes-256-gcm` first because it is the default. A host's `zfs`
/// refuses one it does not know with its own message.
pub const CIPHERS: [&str; 6] = ["aes-256-gcm", "aes-192-gcm", "aes-128-gcm", "aes-256-ccm", "aes-192-ccm", "aes-128-ccm"];

/// `zfs create` of an encrypted dataset with a passphrase key read from the prompt (stdin, once). The key is stored nowhere
/// by zfsctl; `keylocation=prompt` is what `L` and `C` need later.
pub fn create_encrypted(runner: &dyn CommandRunner, name: &str, cipher: &str, passphrase: &str) -> crate::error::Result<()> {
    use crate::error::AppError;
    if name.starts_with('-') || name.is_empty() {
        return Err(AppError::Parse(format!("not a dataset name: {name}")));
    }
    if !CIPHERS.contains(&cipher) {
        return Err(AppError::Parse(format!("unknown cipher {cipher}")));
    }
    check_passphrase(passphrase, true).map_err(AppError::Parse)?;
    // Asked again now (the screen's list may be old or not read yet): under an encrypted parent this would start a second key.
    if let Some((parent, _)) = name.rsplit_once('/') {
        match query(runner, parent) {
            Ok(None) => {}
            Ok(Some(_)) => return Err(AppError::Parse(format!("{parent} is encrypted: what is created under it uses its key already. Use n for a plain new dataset there"))),
            Err(e) => return Err(AppError::Parse(format!("could not check whether {parent} is encrypted: {e}"))),
        }
    }
    let encryption = format!("encryption={cipher}");
    let args = ["create", "-o", encryption.as_str(), "-o", "keyformat=passphrase", "-o", "keylocation=prompt", "--", name];
    let out = runner.run_with_stdin("zfs", &args, zeroize::Zeroizing::new(format!("{passphrase}\n")).as_str())?;
    if out.success() {
        return Ok(());
    }
    let mut stderr = out.stderr;
    if unknown_property(&stderr) {
        stderr = format!("{}\nThis host's zfs does not know native encryption.", stderr.trim_end());
    }
    Err(AppError::CommandFailed { cmd: format!("zfs create {name} (encrypted)"), status: out.status, stderr })
}

/// `zfs unload-key`: zfs refuses while the dataset is mounted, and its message is passed on.
pub fn unload_key(runner: &dyn CommandRunner, dataset: &str) -> crate::error::Result<()> {
    key_command(runner, "unload-key", dataset, None)
}

/// `zfs change-key` with a new passphrase on stdin. The data is not re-encrypted: the old passphrase simply stops working.
pub fn change_key(runner: &dyn CommandRunner, dataset: &str, passphrase: &str) -> crate::error::Result<()> {
    if dataset.starts_with('-') {
        return Err(crate::error::AppError::Parse(format!("not a dataset name: {dataset}")));
    }
    // Asked again now: on a dataset that has meanwhile started to use its parent's key, change-key would give it a key of its own.
    let facts = key_facts(runner, dataset)?;
    check_prompt_key(&facts, dataset)?;
    if facts.root != dataset {
        return Err(crate::error::AppError::Parse(format!("{dataset} uses the key of {}: change the key there", facts.root)));
    }
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
        assert!(check_passphrase("short", true).unwrap_err().contains('8'), "a new key needs 8 characters");
        assert!(check_passphrase("long enough", true).is_ok());
        assert!(check_passphrase(&"x".repeat(513), true).is_err(), "zfs takes at most 512 bytes");
    }

    const FACTS: &str = "keylocation\tprompt\nkeyformat\tpassphrase\nencryptionroot\ttank/s\ncanmount\ton\n";

    #[test]
    fn the_passphrase_goes_through_stdin_and_never_through_the_arguments() {
        let secret = "correct horse battery";
        let runner = FakeRunner::new("").route("zfs", "get", "keylocation\tprompt\nkeyformat\tpassphrase\nencryptionroot\ttank/secure\ncanmount\ton\n");
        load_key_and_mount(&runner, "tank/secure", secret, false).unwrap();
        change_key(&runner, "tank/secure", secret).unwrap();
        let calls = runner.calls.lock().unwrap();
        let verbs: Vec<&str> = calls.iter().map(|c| c.1[0].as_str()).collect();
        assert_eq!(verbs, ["get", "load-key", "get", "change-key"]);
        assert_eq!(calls[1], ("zfs".to_string(), vec!["load-key".to_string(), "--".to_string(), "tank/secure".to_string()]));
        assert_eq!(calls[3], ("zfs".to_string(), vec!["change-key".to_string(), "--".to_string(), "tank/secure".to_string()]));
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
        assert!(load_key_and_mount(&runner, "-r", "pass phrase", false).is_err());
        assert!(unload_key(&runner, "-r").is_err());
        assert!(runner.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn zfs_failures_come_back_verbatim_and_busy_gets_a_hint() {
        let wrong = FakeRunner::failing("Key load error: Incorrect key provided for 'tank/secure'.", 255);
        let msg = load_key_and_mount(&wrong, "tank/secure", "wrong guess", false).unwrap_err().to_string();
        assert!(msg.contains("Incorrect key provided"), "{msg}");
        assert!(!msg.contains("wrong guess"));
        let busy = FakeRunner::failing("Key unload error: 'tank/secure' is busy.", 255);
        let msg = unload_key(&busy, "tank/secure").unwrap_err().to_string();
        assert!(msg.contains("is busy") && msg.contains("unmount"), "{msg}");
    }

    #[test]
    fn a_failed_mount_says_the_key_is_loaded() {
        let runner = FakeRunner::new("").route("zfs", "get", FACTS).route_failure("zfs", "mount", "cannot mount 'tank/s': filesystem already mounted", 1);
        let msg = load_key_and_mount(&runner, "tank/s", "pass phrase", true).unwrap_err().to_string();
        assert!(msg.contains("already mounted") && msg.contains("key IS loaded"), "{msg}");
        let calls = runner.calls.lock().unwrap();
        assert_eq!((calls[1].1[0].as_str(), calls[2].1.as_slice()), ("load-key", ["mount", "--", "tank/s"].map(String::from).as_slice()));
        drop(calls);
        let plain = FakeRunner::new("").route("zfs", "get", FACTS);
        load_key_and_mount(&plain, "tank/s", "pass phrase", false).unwrap();
        assert_eq!(plain.calls.lock().unwrap().len(), 2, "no mount unless asked");
        let off = FakeRunner::new("").route("zfs", "get", "keylocation\tprompt\nkeyformat\tpassphrase\nencryptionroot\ttank/s\ncanmount\toff\n");
        load_key_and_mount(&off, "tank/s", "pass phrase", true).unwrap();
        assert!(off.calls.lock().unwrap().iter().all(|c| c.1[0] != "mount"), "a canmount=off container (or a volume, which prints -) is only unlocked");
    }

    #[test]
    fn a_key_kept_in_a_file_or_inherited_is_refused_before_anything_is_sent() {
        let file = FakeRunner::new("").route("zfs", "get", "keylocation\tfile:///root/s.key\nkeyformat\tpassphrase\nencryptionroot\ttank/s\ncanmount\ton\n");
        for result in [load_key_and_mount(&file, "tank/s", "pass phrase", false), change_key(&file, "tank/s", "pass phrase")] {
            assert!(result.unwrap_err().to_string().contains("file:///root/s.key"));
        }
        let child = FakeRunner::new("").route("zfs", "get", "keylocation\tprompt\nkeyformat\tpassphrase\nencryptionroot\ttank/s\ncanmount\ton\n");
        assert!(change_key(&child, "tank/s/kid", "pass phrase").unwrap_err().to_string().contains("uses the key of tank/s"));
        let raw = FakeRunner::new("").route("zfs", "get", "keylocation\tprompt\nkeyformat\traw\nencryptionroot\ttank/s\ncanmount\ton\n");
        assert!(load_key_and_mount(&raw, "tank/s", "pass phrase", false).is_err());
        for r in [&file, &child, &raw] {
            assert!(r.stdin_calls.lock().unwrap().is_empty(), "the passphrase must not be sent");
            assert!(r.calls.lock().unwrap().iter().all(|c| c.1[0] == "get"));
        }
    }

    #[test]
    fn a_host_that_cannot_say_how_the_key_is_kept_gets_no_key_operation() {
        let down = FakeRunner::failing("ssh: connection refused", 255);
        assert!(load_key_and_mount(&down, "tank/s", "pass phrase", false).is_err());
        assert!(change_key(&down, "tank/s", "pass phrase").is_err());
        let blank = FakeRunner::new("");
        assert!(load_key_and_mount(&blank, "tank/s", "pass phrase", false).is_err(), "an empty answer is not an answer");
        assert!(blank.stdin_calls.lock().unwrap().is_empty());
    }

    #[test]
    fn an_encrypted_dataset_is_created_with_the_passphrase_on_stdin_only() {
        let runner = FakeRunner::new("");
        create_encrypted(&runner, "tank/secure", "aes-256-gcm", "correct horse battery").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].1[0], "get", "the parent is asked first");
        assert_eq!(calls[1].1, ["create", "-o", "encryption=aes-256-gcm", "-o", "keyformat=passphrase", "-o", "keylocation=prompt", "--", "tank/secure"]);
        assert!(calls.iter().all(|c| c.1.iter().all(|a| !a.contains("horse"))));
        assert_eq!(runner.stdin_calls.lock().unwrap().as_slice(), [("zfs".to_string(), "correct horse battery\n".to_string())]);
    }

    #[test]
    fn a_create_under_an_encrypted_parent_or_an_unanswering_host_sends_nothing() {
        let under = FakeRunner::new("").route("zfs", "get", "tank/s\tencryption\taes-256-gcm\ntank/s\tkeyformat\tpassphrase\ntank/s\tkeystatus\tavailable\ntank/s\tencryptionroot\ttank/s\n");
        assert!(create_encrypted(&under, "tank/s/new", "aes-256-gcm", "long enough").unwrap_err().to_string().contains("tank/s is encrypted"));
        let down = FakeRunner::failing("ssh: connection refused", 255);
        assert!(create_encrypted(&down, "tank/s/new", "aes-256-gcm", "long enough").unwrap_err().to_string().contains("could not check"));
        for r in [&under, &down] {
            assert!(r.stdin_calls.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn a_bad_create_sends_nothing() {
        let runner = FakeRunner::new("");
        assert!(create_encrypted(&runner, "-x", "aes-256-gcm", "long enough").is_err());
        assert!(create_encrypted(&runner, "tank/a", "rot13", "long enough").is_err());
        assert!(create_encrypted(&runner, "tank/a", "aes-256-gcm", "short").is_err());
        assert!(runner.calls.lock().unwrap().is_empty() && runner.stdin_calls.lock().unwrap().is_empty());
    }

    #[test]
    fn a_zfs_without_encryption_says_so() {
        let runner = FakeRunner::failing("cannot create 'tank/a': invalid property 'encryption'", 2);
        let msg = create_encrypted(&runner, "tank/a", "aes-256-gcm", "long enough").unwrap_err().to_string();
        assert!(msg.contains("invalid property") && msg.contains("does not know native encryption") && !msg.contains("long enough"), "{msg}");
    }

    #[test]
    fn query_fails_closed_on_every_answer_that_is_not_a_clean_one() {
        let enc_out = "tank/s\tencryption\taes-256-gcm\ntank/s\tkeyformat\tpassphrase\ntank/s\tkeystatus\tavailable\ntank/s\tencryptionroot\ttank/s\n";
        // A dropped connection is an error even when its text happens to mention an unknown property.
        assert!(query(&FakeRunner::failing("invalid property 'keystatus'", 255), "tank/s").is_err());
        // Any other failure that is not "this zfs has no such property" is an error too.
        assert!(query(&FakeRunner::failing("cannot open 'tank/s': dataset does not exist", 1), "tank/s").is_err());
        assert!(query(&FakeRunner::failing("", 1), "tank/s").unwrap_err().contains("status 1"));
        // Only the unknown-property answer means "not encrypted", and a clean answer is parsed.
        assert_eq!(query(&FakeRunner::failing("bad property list: invalid property 'keystatus'", 1), "tank/s"), Ok(None));
        assert_eq!(query(&FakeRunner::new(enc_out), "tank/s").unwrap().unwrap().root, "tank/s");
        assert_eq!(query(&FakeRunner::new(""), "tank/s"), Ok(None));
    }

    #[test]
    fn the_encryption_root_dash_means_none_and_a_passphrases_limits_are_exact() {
        let parsed = parse("x\tencryption\taes-256-gcm\nx\tencryptionroot\t-\n");
        assert_eq!(parsed["x"].root, "");
        assert!(check_passphrase(&"x".repeat(512), false).is_ok());
        assert!(check_passphrase(&"x".repeat(513), false).is_err());
        assert!(check_passphrase(&"x".repeat(8), true).is_ok(), "8 characters is enough");
        assert!(check_passphrase(&"x".repeat(7), true).is_err());
        assert!(check_passphrase(&"é".repeat(8), true).is_ok(), "characters, not bytes");
    }
}
