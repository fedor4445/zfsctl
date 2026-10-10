//! Getting a message to the operator: email through their own command, and the notify hook.

use super::notify::{run_command, NotifyError};
use crate::config::NotifySettings;
use chrono::{DateTime, Utc};
use std::time::Duration;

pub const TIMEOUT: Duration = Duration::from_secs(30);

/// `ZFSCTL_BODY` carries at most this much (environments have a size limit); stdin always carries all of it.
const ENV_BODY_MAX: usize = 32 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub kind: String,
    pub host: String,
    pub level: String,
    pub subject: String,
    pub body: String,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Channel {
    Mail,
    Hook,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Delivery {
    pub channel: Channel,
    pub result: Result<(), NotifyError>,
}

/// One printable ASCII line: control characters become spaces, anything else outside ASCII becomes `?`.
pub fn ascii_line(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else if c.is_ascii() { c } else { '?' }).collect()
}

pub fn default_from() -> String {
    let host = nix::unistd::gethostname().ok().and_then(|h| h.into_string().ok()).filter(|h| !h.is_empty()).unwrap_or_else(|| "localhost".to_string());
    format!("zfsctl@{}", ascii_line(&host))
}

pub fn compose_mail(from: &str, to: &[String], subject: &str, body: &str, now: DateTime<Utc>) -> String {
    let to: Vec<String> = to.iter().map(|a| ascii_line(a)).collect();
    format!(
        "From: {}\nTo: {}\nSubject: {}\nDate: {}\nMIME-Version: 1.0\nContent-Type: text/plain; charset=utf-8\nContent-Transfer-Encoding: 8bit\n\n{body}",
        ascii_line(from),
        to.join(", "),
        ascii_line(subject),
        now.to_rfc2822(),
    )
}

fn env_body(body: &str) -> &str {
    if body.len() <= ENV_BODY_MAX {
        return body;
    }
    let mut end = ENV_BODY_MAX;
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    &body[..end]
}

/// Sends `msg` on every configured channel (one `Delivery` each; none when nothing is configured). One channel
/// failing does not stop the other.
pub fn send(settings: &NotifySettings, hook: Option<&str>, msg: &Message, now: DateTime<Utc>, timeout: Duration) -> Vec<Delivery> {
    let mut out = Vec::new();
    if settings.has_mail() {
        let from = settings.mail_from.clone().unwrap_or_else(default_from);
        let text = compose_mail(&from, &settings.mail_to, &msg.subject, &msg.body, now);
        out.push(Delivery { channel: Channel::Mail, result: run_command(&settings.mail_command, &[], &text, timeout) });
    }
    if let Some(command) = hook {
        let env = [("ZFSCTL_KIND", msg.kind.as_str()), ("ZFSCTL_HOST", msg.host.as_str()), ("ZFSCTL_RESULT", msg.level.as_str()), ("ZFSCTL_MESSAGE", msg.subject.as_str()), ("ZFSCTL_BODY", env_body(&msg.body))];
        out.push(Delivery { channel: Channel::Hook, result: run_command(command, &env, &msg.body, timeout) });
    }
    out
}

pub fn delivered(results: &[Delivery]) -> bool {
    results.iter().any(|d| d.result.is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::NotifyConfig;

    fn settings(command: &str, to: &str) -> NotifySettings {
        NotifyConfig { mail_to: Some(to.into()), mail_command: Some(command.into()), ..NotifyConfig::default() }.resolve().unwrap()
    }

    fn msg(subject: &str, body: &str) -> Message {
        Message { kind: "health-alert".into(), host: "fbsd16".into(), level: "critical".into(), subject: subject.into(), body: body.into() }
    }

    fn at() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-11T07:00:00Z").unwrap().with_timezone(&Utc)
    }

    #[test]
    fn a_composed_mail_has_every_header_then_a_blank_line_then_the_body_verbatim() {
        let text = compose_mail("zfsctl@hub", &["a@x.org".into(), "b@y.org".into()], "[zfsctl] fbsd16: tank DEGRADED", "line one\nline two\n", at());
        let (head, body) = text.split_once("\n\n").unwrap();
        assert!(!text.contains('\r'), "local submission expects bare LF line endings");
        let lines: Vec<&str> = head.split('\n').collect();
        assert_eq!(lines[0], "From: zfsctl@hub");
        assert_eq!(lines[1], "To: a@x.org, b@y.org");
        assert_eq!(lines[2], "Subject: [zfsctl] fbsd16: tank DEGRADED");
        assert!(lines[3].starts_with("Date: ") && lines[3].contains("2026"));
        assert!(lines.contains(&"MIME-Version: 1.0") && lines.contains(&"Content-Type: text/plain; charset=utf-8") && lines.contains(&"Content-Transfer-Encoding: 8bit"));
        assert_eq!(body, "line one\nline two\n");
    }

    #[test]
    fn a_subject_or_sender_cannot_add_a_header_or_carry_non_ascii() {
        assert_eq!(ascii_line("a\r\nBcc: z@z.org\tb"), "a  Bcc: z@z.org b");
        assert_eq!(ascii_line("héllo ✓"), "h?llo ?");
        let text = compose_mail("evil\r\nBcc: z@z.org", &["a@x.org".into()], "x\r\nBcc: z@z.org", "body", at());
        let head = text.split_once("\n\n").unwrap().0;
        assert!(!head.lines().any(|l| l.starts_with("Bcc:")), "{head}");
        assert_eq!(head.lines().filter(|l| l.starts_with("Subject:")).count(), 1);
    }

    #[test]
    fn the_mail_command_receives_the_whole_message_on_stdin() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("mail");
        let s = settings(&format!("cat > {}", out.display()), "me@x.org");
        let r = send(&s, None, &msg("[zfsctl] fbsd16: down", "body text"), at(), Duration::from_secs(10));
        assert_eq!(r.len(), 1);
        assert!(delivered(&r));
        let got = std::fs::read_to_string(out).unwrap();
        assert!(got.contains("To: me@x.org") && got.contains("Subject: [zfsctl] fbsd16: down") && got.ends_with("body text"), "{got}");
    }

    #[test]
    fn the_hook_gets_the_text_in_the_environment_and_on_stdin_and_runs_even_if_mail_fails() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("hook");
        let hook = format!("printf '%s|%s|%s|%s|%s|' \"$ZFSCTL_KIND\" \"$ZFSCTL_HOST\" \"$ZFSCTL_RESULT\" \"$ZFSCTL_MESSAGE\" \"$ZFSCTL_BODY\" > {0}; cat >> {0}", out.display());
        let s = settings("exit 3", "me@x.org");
        let r = send(&s, Some(&hook), &msg("[zfsctl] fbsd16: down", "the body"), at(), Duration::from_secs(10));
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].result, Err(NotifyError::Exit(3)));
        assert!(r[1].result.is_ok() && delivered(&r), "the hook is independent of the mail command");
        assert_eq!(std::fs::read_to_string(out).unwrap(), "health-alert|fbsd16|critical|[zfsctl] fbsd16: down|the body|the body");
    }

    #[test]
    fn no_channel_means_nothing_is_sent_and_nothing_counts_as_delivered() {
        let s = NotifySettings::default();
        let r = send(&s, None, &msg("s", "b"), at(), Duration::from_secs(1));
        assert!(r.is_empty() && !delivered(&r));
    }

    #[test]
    fn a_hanging_mail_command_is_killed_at_the_timeout() {
        let s = settings("exec sleep 30", "me@x.org");
        let t = std::time::Instant::now();
        let r = send(&s, None, &msg("s", "b"), at(), Duration::from_millis(300));
        assert_eq!(r[0].result, Err(NotifyError::TimedOut));
        assert!(t.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_command_that_ignores_stdin_does_not_block_a_large_body() {
        let s = settings("true", "me@x.org");
        let big = "x".repeat(2_000_000);
        assert!(delivered(&send(&s, None, &msg("s", &big), at(), Duration::from_secs(10))));
    }

    #[test]
    fn a_huge_body_is_cut_for_the_environment_at_a_character_boundary_but_not_on_stdin() {
        let body = "é".repeat(40_000);
        let cut = env_body(&body);
        assert!(cut.len() <= ENV_BODY_MAX && cut.chars().all(|c| c == 'é'));
        assert_eq!(env_body("short"), "short");
    }

    #[test]
    fn the_default_sender_names_the_hub() {
        assert!(default_from().starts_with("zfsctl@") && default_from().len() > "zfsctl@".len());
    }
}
