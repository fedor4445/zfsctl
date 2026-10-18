//! What a job may read from an encrypted dataset. A `zfs send` without the raw option delivers the data decrypted, and a
//! file-level copy of a locked dataset finds nothing to read, so each job kind says what it needs before it touches the
//! source. Asked of the source's own host at run time, so a dataset that was encrypted after the job was saved is caught.

use crate::exec::CommandRunner;

/// What a job kind needs from its source dataset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// `zfs send` (replication, stream files) and the rclone mirror: the source must not be encrypted at all.
    NotEncrypted,
    /// A file-level backup that re-encrypts by itself (restic): an encrypted source is fine while its key is loaded.
    KeyLoaded,
}

/// `Ok` when the job may go on, otherwise the reason it must not (shown as the run's failure message).
pub fn check_source(runner: &dyn CommandRunner, dataset: &str, need: Need) -> std::result::Result<(), String> {
    let enc = crate::zfs::encryption::query(runner, dataset).map_err(|e| format!("could not check whether {dataset} is encrypted: {e}"))?;
    match (enc, need) {
        (None, _) => Ok(()),
        (Some(_), Need::NotEncrypted) => Err(not_allowed(dataset)),
        (Some(e), Need::KeyLoaded) if e.locked => Err(format!("{dataset} is locked (key not loaded): load the key first")),
        (Some(_), Need::KeyLoaded) => Ok(()),
    }
}

/// The text a form or a run shows for a source this job kind does not take.
pub fn not_allowed(dataset: &str) -> String {
    format!("{dataset} is encrypted: sending it without a raw option would deliver it decrypted, so this job kind does not run on encrypted datasets")
}

/// What a test host answers to the encryption query: nothing for `None` (an unencrypted dataset), else an encrypted dataset
/// whose `keystatus` is the given text.
#[cfg(test)]
pub fn fake_answer(dataset: &str, keystatus: Option<&str>) -> String {
    keystatus.map_or_else(String::new, |k| format!("{dataset}\tencryption\taes-256-gcm\n{dataset}\tkeyformat\tpassphrase\n{dataset}\tkeystatus\t{k}\n{dataset}\tencryptionroot\t{dataset}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;

    fn get(status: &str) -> String {
        format!("tank/s\tencryption\taes-256-gcm\ntank/s\tkeyformat\tpassphrase\ntank/s\tkeystatus\t{status}\ntank/s\tencryptionroot\ttank/s\n")
    }

    #[test]
    fn an_encrypted_source_is_refused_by_the_kinds_that_would_send_it_in_the_clear() {
        for status in ["available", "unavailable"] {
            let err = check_source(&FakeRunner::new(&get(status)), "tank/s", Need::NotEncrypted).unwrap_err();
            assert!(err.starts_with("tank/s is encrypted") && err.contains("raw"), "{err}");
        }
    }

    #[test]
    fn restic_may_read_an_open_encrypted_source_but_not_a_locked_one() {
        assert_eq!(check_source(&FakeRunner::new(&get("available")), "tank/s", Need::KeyLoaded), Ok(()));
        let err = check_source(&FakeRunner::new(&get("unavailable")), "tank/s", Need::KeyLoaded).unwrap_err();
        assert_eq!(err, "tank/s is locked (key not loaded): load the key first");
    }

    #[test]
    fn a_plain_source_and_a_zfs_without_the_properties_pass() {
        let plain = "tank/s\tencryption\toff\ntank/s\tkeystatus\t-\n";
        assert_eq!(check_source(&FakeRunner::new(plain), "tank/s", Need::NotEncrypted), Ok(()));
        assert_eq!(check_source(&FakeRunner::failing("invalid property 'keystatus'", 1), "tank/s", Need::NotEncrypted), Ok(()));
    }

    #[test]
    fn a_dropped_connection_fails_the_run_instead_of_guessing() {
        let err = check_source(&FakeRunner::failing("Connection closed by 10.0.0.9", 255), "tank/s", Need::NotEncrypted).unwrap_err();
        assert_eq!(err, "could not check whether tank/s is encrypted: Connection closed by 10.0.0.9");
    }
}
