//! The Cloud section's "set up a provider" guide: what to get from the provider and exactly what to answer in `rclone config`.
//!
//! zfsctl never creates a remote itself: the keys would have to travel in a command line, and the browser login of Google
//! Drive, `OneDrive` and Dropbox has no place on a hub without a browser. So the guide says what to do and zfsctl then checks
//! the result (F5 on the remote's row).

/// One provider the guide knows.
pub struct Provider {
    /// How people say it.
    pub name: &'static str,
    /// What to type at rclone's `Storage>` question.
    pub rclone_type: &'static str,
    /// What to get from the provider first.
    pub before: &'static [&'static str],
    /// The answers to rclone's questions that are not the default.
    pub answers: &'static [&'static str],
    /// The login is a browser login (OAuth): the hub has no browser, so the token comes from `rclone authorize`.
    pub browser_login: bool,
    /// A remote written the way a job's Remote field takes it.
    pub example: &'static str,
}

pub const PROVIDERS: &[Provider] = &[
    Provider {
        name: "Backblaze B2",
        rclone_type: "b2",
        before: &["In the Backblaze web console make an Application Key limited to the one bucket you will use (private), and note its keyID and applicationKey.", "Make the bucket first: zfsctl does not create buckets."],
        answers: &["account>  the keyID", "key>      the applicationKey"],
        browser_login: false,
        example: "b2:mybucket/photos",
    },
    Provider {
        name: "Amazon S3",
        rclone_type: "s3",
        before: &["In AWS make an IAM user whose policy allows only this bucket (list, get, put and delete), and note its access key id and secret access key.", "Make the bucket first: zfsctl does not create buckets."],
        answers: &["provider>           AWS", "env_auth>           false (the keys are typed in)", "access_key_id>      the access key id", "secret_access_key>  the secret access key", "region>             the bucket's region, such as us-east-1", "endpoint>           leave blank"],
        browser_login: false,
        example: "aws:mybucket/photos",
    },
    Provider {
        name: "Cloudflare R2, Wasabi, MinIO and other S3-compatible",
        rclone_type: "s3",
        before: &["From the provider note an access key id, a secret access key and the endpoint address (R2: https://<account id>.r2.cloudflarestorage.com, Wasabi: s3.<region>.wasabisys.com, MinIO: your server's address).", "Make the bucket first: zfsctl does not create buckets."],
        answers: &["provider>           Cloudflare, Wasabi, Minio or Other, as it fits", "access_key_id>      the access key id", "secret_access_key>  the secret access key", "endpoint>           the endpoint address"],
        browser_login: false,
        example: "r2:mybucket/photos",
    },
    Provider {
        name: "Google Drive",
        rclone_type: "drive",
        before: &["A Google account. rclone's built-in access works; for heavy use make your own client id in the Google console (optional, faster)."],
        answers: &["client_id> and client_secret>   leave blank", "scope>                          1 (full access)", "service_account_file>          leave blank", "team drive>                     n, unless it is a shared drive"],
        browser_login: true,
        example: "gdrive:zfsctl/photos",
    },
    Provider {
        name: "Microsoft OneDrive",
        rclone_type: "onedrive",
        before: &["A Microsoft account (personal or work or school)."],
        answers: &["client_id> and client_secret>   leave blank", "region>                         global", "type of connection>             OneDrive Personal or Business, as your account is, then pick the drive it lists"],
        browser_login: true,
        example: "onedrive:zfsctl/photos",
    },
    Provider {
        name: "Dropbox",
        rclone_type: "dropbox",
        before: &["A Dropbox account."],
        answers: &["client_id> and client_secret>   leave blank"],
        browser_login: true,
        example: "dropbox:zfsctl/photos",
    },
    Provider {
        name: "Azure Blob Storage",
        rclone_type: "azureblob",
        before: &["In Azure make a storage account and a container, and note the storage account name and one of its access keys (or make a SAS URL for the container).", "Make the container first: zfsctl does not create it."],
        answers: &["account>  the storage account name", "key>      the access key"],
        browser_login: false,
        example: "azure:mycontainer/photos",
    },
    Provider {
        name: "Google Cloud Storage",
        rclone_type: "gcs",
        before: &["In the Google Cloud console make a bucket and a service account with access to it, and download its JSON key file onto the host (readable by root only, for example /root/gcs-key.json).", "Make the bucket first: zfsctl does not create buckets."],
        answers: &["project_number>        your project number", "service_account_file>  the path of the JSON key file"],
        browser_login: false,
        example: "gcs:mybucket/photos",
    },
    Provider {
        name: "SFTP (a server you can reach with ssh)",
        rclone_type: "sftp",
        before: &["A login on the server, and a private key file for it on this host (readable by root only). Prefer a key to a password: a password would be stored by rclone, obscured but not encrypted."],
        answers: &["host>      the server's name or address", "user>      the login name", "key_file>  the path of the private key file", "pass>      leave blank"],
        browser_login: false,
        example: "nas:/backups/photos",
    },
];

/// The guide for one provider: what to get first, what to run, what to answer, how to use the remote and how to check it.
pub fn guide_text(p: &Provider) -> String {
    let short = p.example.split(':').next().unwrap_or("remote");
    let mut lines: Vec<String> = vec![format!("{}  (rclone type: {})", p.name, p.rclone_type), String::new(), "Get this first".to_string()];
    lines.extend(p.before.iter().map(|l| format!("  - {l}")));
    lines.push(String::new());
    lines.push("Then, on the host that runs the job (the hub, unless the job is on another host), as root:".to_string());
    lines.push("  sudo rclone config".to_string());
    lines.push("  n                New remote".to_string());
    lines.push(format!("  name>            a short name, such as {short} (letters, digits, - and _)"));
    lines.push(format!("  Storage>         {}", p.rclone_type));
    lines.extend(p.answers.iter().map(|l| format!("  {l}")));
    if p.browser_login {
        lines.push("  Use web browser to automatically authenticate rclone with remote?".to_string());
        lines.push("                   n (the hub has no browser)".to_string());
        lines.push(String::new());
        lines.push("The login is done in a browser, on another computer:".to_string());
        lines.push(format!("  1. On a computer with a browser and rclone installed run:  rclone authorize \"{}\"", p.rclone_type));
        lines.push("  2. Log in and allow access; rclone prints a token (a block of text between lines).".to_string());
        lines.push("  3. Paste that token into the question the hub's rclone config asks for it.".to_string());
    }
    lines.push("  (every other question: press Enter for the default; at the end: Keep this remote? y, then q)".to_string());
    lines.push(String::new());
    lines.push("Use it".to_string());
    lines.push(format!("  In a cloud mirror job write the remote as {}.", p.example));
    lines.push(format!("  In a restic backup write the repository as rclone:{}.", p.example));
    lines.push(String::new());
    lines.push("Check it".to_string());
    lines.push("  Back in Cloud, select the remote's row (r refreshes the list) and press F5: zfsctl lists the remote's top level, which changes nothing.".to_string());
    lines.push(String::new());
    lines.push("Your keys stay in rclone's own config file, readable by root only. zfsctl never reads or shows them.".to_string());
    lines.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_provider_has_a_name_a_type_and_a_remote_written_the_way_a_job_takes_it() {
        assert!(PROVIDERS.len() >= 9);
        for p in PROVIDERS {
            assert!(!p.name.is_empty() && !p.rclone_type.is_empty() && !p.before.is_empty() && !p.answers.is_empty(), "{}", p.name);
            assert!(crate::rclone::valid_remote(p.example), "{}: {}", p.name, p.example);
            assert!(crate::restic::valid_repository(&format!("rclone:{}", p.example)), "{}", p.name);
        }
        let names: Vec<&str> = PROVIDERS.iter().map(|p| p.name).collect();
        assert!(names.iter().enumerate().all(|(i, n)| !names[..i].contains(n)), "names are unique");
    }

    #[test]
    fn the_guide_says_what_to_get_run_answer_use_and_check() {
        let b2 = guide_text(&PROVIDERS[0]);
        for want in ["Backblaze B2", "Get this first", "sudo rclone config", "Storage>         b2", "keyID", "b2:mybucket/photos", "rclone:b2:mybucket/photos", "press F5", "never reads or shows"] {
            assert!(b2.contains(want), "{want:?} in:\n{b2}");
        }
        assert!(!b2.contains("rclone authorize"), "a key login needs no browser");
    }

    #[test]
    fn the_browser_login_providers_say_how_to_log_in_from_another_computer() {
        for name in ["Google Drive", "Microsoft OneDrive", "Dropbox"] {
            let p = PROVIDERS.iter().find(|p| p.name == name).unwrap();
            assert!(p.browser_login, "{name}");
            let text = guide_text(p);
            assert!(text.contains(&format!("rclone authorize \"{}\"", p.rclone_type)) && text.contains("the hub has no browser"), "{name}:\n{text}");
        }
        assert!(PROVIDERS.iter().filter(|p| !p.browser_login).all(|p| !guide_text(p).contains("rclone authorize")));
    }

    #[test]
    fn no_guide_asks_for_a_key_on_a_command_line() {
        for p in PROVIDERS {
            let text = guide_text(p);
            assert!(!text.contains("rclone config create") && !text.contains("--") , "{}: {text}", p.name);
        }
    }
}
