//! Tests of `config.rs`.
#[test]
fn daily_check_accepts_off_and_hh_mm_and_rejects_the_rest() {
    let time = |v: Option<&str>| DaemonConfig { daily_check: v.map(str::to_string), ..DaemonConfig::default() }.daily_time();
    assert_eq!(time(None), Ok(chrono::NaiveTime::from_hms_opt(3, 30, 0)), "the default is 03:30");
    assert_eq!(time(Some("off")), Ok(None));
    assert_eq!(time(Some("05:15")), Ok(chrono::NaiveTime::from_hms_opt(5, 15, 0)));
    for bad in ["", "25:00", "3pm", "03:30:00", "on"] {
        assert!(time(Some(bad)).is_err(), "{bad:?}");
    }
}

#[test]
fn the_old_smart_check_key_is_still_read_and_daily_check_wins_when_both_are_there() {
    let read = |text: &str| toml::from_str::<Config>(text).unwrap().daemon.daily_time();
    assert_eq!(read("[daemon]\nsmart_check = \"05:15\"\n"), Ok(chrono::NaiveTime::from_hms_opt(5, 15, 0)));
    assert_eq!(read("[daemon]\nsmart_check = \"05:15\"\ndaily_check = \"off\"\n"), Ok(None));
    assert_eq!(read("[daemon]\ndaily_check = \"04:00\"\n"), Ok(chrono::NaiveTime::from_hms_opt(4, 0, 0)));
    let saved = toml::to_string(&toml::from_str::<Config>("[daemon]\nsmart_check = \"05:15\"\n").unwrap()).unwrap();
    assert!(!saved.contains("smart_check"), "the old key is read, never written: {saved}");
}

#[test]
fn documented_keys_match_the_serialised_fields() {
    let host = HostConfig { name: "n".into(), ssh_target: Some("u@h".into()), tag: Some("t".into()), site: Some("s".into()) };
    let ui = UiConfig { theme: Some("mono".into()), auto_refresh_secs: Some(5) };
    let daemon = DaemonConfig { notify_command: Some("true".into()), daily_check: Some("03:30".into()), smart_check: None };
    let cloud = CloudConfig { caps: [("b2".to_string(), "10G".to_string())].into() };
    let ssh = SshConfig { multiplex: Some(true) };
    let dashboard = DashboardConfig {
        memory_warn: Some(90),
        memory_crit: Some(97),
        swap_warn: Some(50),
        swap_crit: Some(90),
        quota_warn: Some(90),
        quota_crit: Some(98),
        dedup_min_ratio: Some(1.10),
        cpu_warn: Some(95),
        scrub_warn_days: Some(35),
        scrub_crit_days: Some(90),
        scrub_ignore: vec!["scratch".into()],
    };
    let protection = ProtectionConfig { level: Some("custom".into()), copies: Some(2.into()), online: Some(1.into()), domains: Some(2.into()), recent: Some("normal".into()), email: Some("report".into()), not_a_table: false };
    let notify = NotifyConfig {
        mail_to: Some("a@b".into()),
        mail_command: Some("c".into()),
        mail_from: Some("f@g".into()),
        check_interval_minutes: Some(5),
        report_time: Some("07:00".into()),
        remind_hours: Some(6),
    };
    let job = crate::jobs::Job {
        id: "i".into(), host: "local".into(), kind: "backup".into(), pool: Some("p".into()), dataset: Some("d".into()), recursive: true,
        prefix: Some("x".into()), keep: Some(1), dest_host: Some("h".into()), dest_dataset: Some("d".into()), dest_dir: Some("/d".into()), dest_keep: Some(2), remote: Some("aws:b".into()), bwlimit: Some("5M".into()),
        force: true, raw: false, repository: Some("r".into()), password_file: Some("/p".into()), retention: Some("7d".into()), source: Some("new".into()), snapshot: Some("s".into()), path: Some("p".into()), schedule: "* * * * *".into(),
        catch_up: true, enabled: true, created_at: Some(chrono::Utc::now()),
    };
    let mut found: Vec<String> = Vec::new();
    for value in [toml::Value::try_from(&host).unwrap(), toml::Value::try_from(&ui).unwrap(), toml::Value::try_from(&daemon).unwrap(), toml::Value::try_from(&notify).unwrap(), toml::Value::try_from(&cloud).unwrap(), toml::Value::try_from(&ssh).unwrap(), toml::Value::try_from(&dashboard).unwrap(), toml::Value::try_from(&protection).unwrap(), toml::Value::try_from(&job).unwrap()] {
        found.extend(value.as_table().unwrap().keys().cloned());
    }
    found.sort();
    found.dedup();
    let mut listed: Vec<String> = documented_config_keys().into_iter().map(String::from).collect();
    listed.sort();
    listed.dedup();
    assert_eq!(found, listed, "documented_config_keys() must list exactly the fields the structs serialise");
}

use super::*;

#[test]
fn connection_sharing_is_off_unless_the_ssh_section_turns_it_on() {
    assert_eq!(Config::default().ssh.mux_dir(), None);
    let on: Config = toml::from_str("[ssh]\nmultiplex = true\n").unwrap();
    assert_eq!(on.ssh.mux_dir(), Some(std::path::PathBuf::from("/var/db/zfsctl/ssh")));
    let off: Config = toml::from_str("[ssh]\nmultiplex = false\n").unwrap();
    assert_eq!(off.ssh.mux_dir(), None);
    assert!(!toml::to_string(&Config::default()).unwrap().contains("ssh"), "an unset [ssh] is not written");
}

#[test]
fn the_dashboard_defaults_are_the_specs_numbers() {
    let t = Thresholds::default();
    assert_eq!((t.memory, t.swap, t.cpu_warn, t.scrub_days), ((90, 97), (50, 90), 95, (35, 90)));
    assert!(t.scrub_ignore.is_empty());
    assert_eq!(Config::default().dashboard.resolve(), t, "no [dashboard] section means the defaults");
}

#[test]
fn quota_thresholds_default_to_90_and_98_and_fall_back_when_invalid() {
    assert_eq!(Thresholds::default().quota, (90, 98));
    let c: Config = toml::from_str("[dashboard]\nquota_warn = 80\nquota_crit = 95\n").unwrap();
    assert_eq!(c.dashboard.resolve().quota, (80, 95));
    let bad: Config = toml::from_str("[dashboard]\nquota_warn = 99\nquota_crit = 95\n").unwrap();
    assert_eq!(bad.dashboard.resolve().quota, (90, 98), "warn above crit falls back");
}

#[test]
fn the_dedup_ratio_threshold_defaults_to_1_10_and_falls_back_when_invalid() {
    assert_eq!(Thresholds::default().dedup_ratio, 110);
    let ok: Config = toml::from_str("[dashboard]\ndedup_min_ratio = 1.25\n").unwrap();
    assert_eq!(ok.dashboard.resolve().dedup_ratio, 125);
    for bad in ["0.5", "11.0", "\"abc\""] {
        let c: Config = toml::from_str(&format!("[dashboard]\ndedup_min_ratio = {bad}\n")).unwrap();
        assert_eq!(c.dashboard.resolve().dedup_ratio, 110, "{bad}");
    }
}

#[test]
fn dashboard_overrides_are_read_from_config_toml_and_resolved() {
    let text = "[dashboard]\nmemory_warn = 80\nmemory_crit = 95\nswap_warn = 20\nswap_crit = 60\ncpu_warn = 90\nscrub_warn_days = 14\nscrub_crit_days = 30\nscrub_ignore = [\"scratch\", \"tmp\"]\n";
    let c: Config = toml::from_str(text).unwrap();
    let t = c.dashboard.resolve();
    assert_eq!((t.memory, t.swap, t.cpu_warn, t.scrub_days), ((80, 95), (20, 60), 90, (14, 30)));
    assert_eq!(t.scrub_ignore, ["scratch", "tmp"]);
    let partial: Config = toml::from_str("[dashboard]\nmemory_warn = 85\n").unwrap();
    assert_eq!(partial.dashboard.resolve().memory, (85, 97), "one key of a pair keeps the other's default");
    assert!(!toml::to_string_pretty(&Config::default()).unwrap().contains("dashboard"), "an empty section writes nothing");
}

#[test]
fn a_pair_that_is_not_warn_below_crit_within_range_falls_back_to_its_defaults() {
    let bad = |memory_warn, memory_crit| DashboardConfig { memory_warn, memory_crit, ..DashboardConfig::default() }.resolve().memory;
    assert_eq!(bad(Some(97), Some(90)), (90, 97), "warn above crit");
    assert_eq!(bad(Some(90), Some(90)), (90, 97), "warn equal to crit");
    assert_eq!(bad(Some(0), Some(50)), (90, 97), "zero");
    assert_eq!(bad(Some(50), Some(101)), (90, 97), "over 100");
    assert_eq!(bad(Some(50), Some(100)), (50, 100), "100 is allowed");
    let days = |w, c| DashboardConfig { scrub_warn_days: w, scrub_crit_days: c, ..DashboardConfig::default() }.resolve().scrub_days;
    assert_eq!(days(Some(0), Some(10)), (35, 90));
    assert_eq!(days(Some(40), Some(30)), (35, 90));
    assert_eq!(days(Some(7), Some(8)), (7, 8));
    assert_eq!(DashboardConfig { cpu_warn: Some(0), ..DashboardConfig::default() }.resolve().cpu_warn, 95);
    assert_eq!(DashboardConfig { cpu_warn: Some(101), ..DashboardConfig::default() }.resolve().cpu_warn, 95);
    assert_eq!(DashboardConfig { cpu_warn: Some(100), ..DashboardConfig::default() }.resolve().cpu_warn, 100);
}

#[test]
fn a_typo_in_a_display_setting_never_stops_the_config_from_loading() {
    // The daemon and the TUI refuse to start on a config that does not parse: [dashboard] and [cloud] only decorate a
    // screen, so a wrong type there must cost that one setting, not the jobs and the alerts after a reboot.
    for text in [
        "[dashboard]\nmemory_warn = 300\n",
        "[dashboard]\nmemory_warn = -1\n",
        "[dashboard]\nmemory_warn = 92.5\n",
        "[dashboard]\nmemory_warn = \"90\"\n",
        "[dashboard]\nscrub_ignore = \"scratch\"\n",
        "[dashboard]\nscrub_ignore = [1, 2]\n",
        "dashboard = 5\n",
        "cloud = 3\n",
        "[cloud]\ncaps = 7\n",
        "[cloud.caps]\nb2 = 10\n",
    ] {
        let parsed: std::result::Result<Config, toml::de::Error> = toml::from_str(text);
        assert!(parsed.is_ok(), "{text:?}: {parsed:?}");
    }
    let bad: Config = toml::from_str("[dashboard]\nmemory_warn = 300\nswap_warn = 20\nswap_crit = 60\nscrub_ignore = \"scratch\"\n").unwrap();
    let t = bad.dashboard.resolve();
    assert_eq!((t.memory, t.swap), ((90, 97), (20, 60)), "the bad key falls back, the good ones are kept");
    assert!(t.scrub_ignore.is_empty());
    let caps: Config = toml::from_str("[cloud.caps]\nb2 = 10\nzfsctl-aws = \"5G\"\n").unwrap();
    assert_eq!(caps.cloud.caps.get("b2").map(String::as_str), Some("10"), "a non-text cap is kept as text, to be reported as not a size");
    assert_eq!(caps.cloud.caps.get("zfsctl-aws").map(String::as_str), Some("5G"));
    // a whole config with a bad section still loads from disk, and its other parts are intact.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n\n[dashboard]\nmemory_crit = 999\n").unwrap();
    let loaded = Config::load(&path).expect("a bad [dashboard] value must not stop the program from starting");
    assert_eq!(loaded.hosts.len(), 1);
}

#[test]
fn plan_sizes_are_read_the_way_cloud_plans_count_them() {
    for (text, bytes) in [("10G", 10_000_000_000), ("2.5G", 2_500_000_000), ("500M", 500_000_000), ("1T", 1_000_000_000_000), ("1000", 1000), (" 10G ", 10_000_000_000)] {
        assert_eq!(parse_cap(text), Some(bytes), "{text}");
    }
    for bad in ["", "G", "0", "0G", "-1G", "10GB", "1.2.3G", "ten", "10 G", "1e9", "99999999999T", "10g"] {
        assert_eq!(parse_cap(bad), None, "{bad:?}");
    }
}

#[test]
fn caps_are_read_from_the_cloud_section_and_an_empty_section_writes_nothing() {
    let c: Config = toml::from_str("[cloud.caps]\nb2 = \"10G\"\nzfsctl-aws = \"5G\"\n").unwrap();
    assert_eq!(c.cloud.caps.get("b2").map(String::as_str), Some("10G"));
    assert_eq!(c.cloud.caps.len(), 2);
    assert!(!toml::to_string_pretty(&Config::default()).unwrap().contains("cloud"));
}

#[test]
fn load_missing_file_returns_default() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("does-not-exist.toml");
    let config = Config::load(&path).unwrap();
    assert_eq!(config, Config::default());
}

#[test]
fn load_malformed_toml_returns_parse_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "this is not valid toml [[[").unwrap();
    let result = Config::load(&path);
    assert!(result.is_err());
}

#[test]
fn save_then_load_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("config.toml");
    let config = Config {
        hosts: vec![
            HostConfig { name: "local".to_string(), ssh_target: None, tag: None, site: None },
            HostConfig {
                name: "backup".to_string(),
                ssh_target: Some("backup.example.com".to_string()),
                tag: None,
                site: None,
            },
        ],
        ..Default::default()
    };
    config.save(&path).unwrap();
    let loaded = Config::load(&path).unwrap();
    assert_eq!(loaded, config);
}

#[test]
fn clean_tag_trims_drops_blank_and_limits_to_forty_characters() {
    assert_eq!(clean_tag("  backup "), Some("backup".to_string()));
    assert_eq!(clean_tag("   "), None);
    assert_eq!(clean_tag(&"x".repeat(60)).unwrap().chars().count(), 40);
}

#[test]
fn a_config_without_a_tag_loads_and_a_tag_round_trips_and_is_omitted_when_none() {
    let old = "[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n";
    let c: Config = toml::from_str(old).unwrap();
    assert_eq!(c.hosts[0].tag, None);
    assert!(!toml::to_string(&c).unwrap().contains("tag"));
    let with = "[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\ntag = \"backup\"\n";
    let c: Config = toml::from_str(with).unwrap();
    assert_eq!(c.hosts[0].tag.as_deref(), Some("backup"));
    assert!(toml::to_string(&c).unwrap().contains("tag = \"backup\""));
}

#[test]
fn a_config_without_notify_loads_saves_unchanged_and_resolves_to_the_defaults() {
    let config: Config = toml::from_str("[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n").unwrap();
    assert_eq!(config.notify, NotifyConfig::default());
    assert!(!toml::to_string_pretty(&config).unwrap().contains("notify"));
    let s = config.notify.resolve().unwrap();
    assert!(s.mail_to.is_empty() && !s.has_mail());
    assert_eq!(s.mail_command, "msmtp -t");
    assert_eq!(s.check_interval, std::time::Duration::from_secs(300));
    assert_eq!(s.report_time, Some(chrono::NaiveTime::from_hms_opt(7, 0, 0).unwrap()));
    assert_eq!(s.remind, Some(std::time::Duration::from_secs(6 * 3600)));
    assert_eq!(s.mail_from, None);
}

#[test]
fn every_notify_key_parses_and_round_trips() {
    let text = "[notify]\nmail_to = \"a@x.org, b@y.org\"\nmail_command = \"msmtp -a default -t\"\nmail_from = \"zfsctl@hub\"\ncheck_interval_minutes = 10\nreport_time = \"06:30\"\nremind_hours = 0\n";
    let config: Config = toml::from_str(text).unwrap();
    let s = config.notify.resolve().unwrap();
    assert_eq!(s.mail_to, ["a@x.org", "b@y.org"]);
    assert_eq!((s.mail_command.as_str(), s.mail_from.as_deref()), ("msmtp -a default -t", Some("zfsctl@hub")));
    assert_eq!(s.check_interval, std::time::Duration::from_secs(600));
    assert_eq!(s.report_time, Some(chrono::NaiveTime::from_hms_opt(6, 30, 0).unwrap()));
    assert_eq!(s.remind, None, "0 hours means never");
    assert_eq!(toml::from_str::<Config>(&toml::to_string_pretty(&config).unwrap()).unwrap(), config);
}

fn prot(level: Option<&str>, copies: Option<u8>, online: Option<u8>, recent: Option<&str>, email: Option<&str>) -> ProtectionConfig {
    ProtectionConfig { level: level.map(Into::into), copies: copies.map(Into::into), online: online.map(Into::into), domains: None, recent: recent.map(Into::into), email: email.map(Into::into), not_a_table: false }
}

#[test]
fn protection_defaults_are_basic_normal_immediate() {
    let s = ProtectionConfig::default().resolve().unwrap();
    assert_eq!(s, ProtectionSettings { copies: 1, online: 0, domains: 1, tolerance: 2, email: EmailTiming::Immediate, groups: GroupRules::default() });
    assert_eq!(s, ProtectionSettings::default());
    assert_eq!(Config::default().protection.resolve().unwrap(), s);
}

#[test]
fn careful_means_three_copies_one_online() {
    let s = prot(Some("careful"), None, None, None, None).resolve().unwrap();
    assert_eq!((s.copies, s.online), (3, 1));
}

#[test]
fn custom_reads_copies_and_online_and_ignores_them_for_other_levels() {
    let s = prot(Some("custom"), Some(2), Some(1), None, None).resolve().unwrap();
    assert_eq!((s.copies, s.online), (2, 1));
    let d = prot(Some("custom"), None, None, None, None).resolve().unwrap();
    assert_eq!((d.copies, d.online), (1, 0));
    let b = prot(Some("basic"), Some(3), Some(2), None, None).resolve().unwrap();
    assert_eq!((b.copies, b.online), (1, 0));
    let c = prot(Some("careful"), Some(9), Some(9), None, None).resolve().unwrap();
    assert_eq!((c.copies, c.online), (3, 1), "bad numbers are ignored, not errors, outside custom");
}

#[test]
fn recent_steps_are_one_two_and_four_intervals() {
    let t = |r| prot(None, None, None, Some(r), None).resolve().unwrap().tolerance;
    assert_eq!((t("strict"), t("normal"), t("relaxed")), (1, 2, 4));
    let e = |m| prot(None, None, None, None, Some(m)).resolve().unwrap().email;
    assert_eq!((e("immediate"), e("report"), e("off")), (EmailTiming::Immediate, EmailTiming::Report, EmailTiming::Off));
}

#[test]
fn a_wrong_value_names_the_key() {
    let err = |c: ProtectionConfig| c.resolve().unwrap_err();
    assert_eq!(err(prot(Some("paranoid"), None, None, None, None)), "[protection] level must be basic, careful or custom");
    assert!(err(prot(Some("custom"), Some(4), None, None, None)).contains("copies"));
    assert!(err(prot(Some("custom"), Some(0), None, None, None)).contains("copies"));
    assert!(err(prot(Some("custom"), Some(3), Some(3), None, None)).contains("online"));
    assert!(err(prot(Some("custom"), Some(1), Some(2), None, None)).contains("online"));
    assert!(err(prot(None, None, None, Some("fast"), None)).contains("recent must be strict, normal or relaxed"));
    assert!(err(prot(None, None, None, None, Some("daily"))).contains("email must be immediate, report or off"));
}

#[test]
fn a_wrong_type_in_protection_loads_and_resolve_names_the_key() {
    let resolved = |body: &str| {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, format!("{body}\n[[hosts]]\nname = \"t2\"\nssh = \"fb@t2\"\n")).unwrap();
        let config = Config::load(&path).unwrap_or_else(|e| panic!("{body}: the file must still load: {e}"));
        assert_eq!(config.hosts.len(), 1, "{body}: the rest of the file is read");
        config.protection.resolve()
    };
    let level = "[protection] level must be basic, careful or custom";
    assert_eq!(resolved("[protection]\nlevel = 5\n").unwrap_err(), level);
    assert_eq!(resolved("[protection]\nlevel = \"Careful\"\n").unwrap_err(), level, "the words are lower case");
    assert_eq!(resolved("[protection]\nlevel = \"custom\"\ncopies = \"2\"\n").unwrap_err(), "[protection] copies must be 1, 2 or 3");
    assert_eq!(resolved("[protection]\nlevel = \"custom\"\ncopies = 300\n").unwrap_err(), "[protection] copies must be 1, 2 or 3");
    assert_eq!(resolved("[protection]\nlevel = \"custom\"\ncopies = -1\n").unwrap_err(), "[protection] copies must be 1, 2 or 3");
    assert_eq!(resolved("[protection]\nlevel = \"custom\"\ncopies = 3\nonline = 1.5\n").unwrap_err(), "[protection] online must be 0, 1 or 2");
    assert_eq!(resolved("[protection]\nrecent = true\n").unwrap_err(), "[protection] recent must be strict, normal or relaxed");
    assert_eq!(resolved("[protection]\nemail = \"of\"\n").unwrap_err(), "[protection] email must be immediate, report or off");
    assert_eq!(resolved("[protection]\nemail = [\"off\"]\n").unwrap_err(), "[protection] email must be immediate, report or off");
    assert_eq!(resolved("protection = 5\n").unwrap_err(), "[protection] must be a table of keys (level, copies, online, domains, recent, email)");
    assert_eq!(resolved("[protection]\nlevel = \"custom\"\ncopies = 2\nonline = 1\n").unwrap(), ProtectionSettings { copies: 2, online: 1, domains: 1, tolerance: 2, email: EmailTiming::Immediate, groups: GroupRules::default() });
    assert_eq!(resolved("[protection]\nlevel = \"careful\"\ncopies = \"x\"\n").unwrap().copies, 3, "copies only counts under custom");
}

#[test]
fn resolve_or_basic_gives_the_defaults_and_the_problem_in_words() {
    let ok = prot(Some("careful"), None, None, None, Some("report")).resolve_or_basic();
    assert_eq!((ok.0.copies, ok.0.email, ok.1), (3, EmailTiming::Report, None));
    let (s, problem) = prot(Some("Careful"), None, None, None, None).resolve_or_basic();
    assert_eq!((s, problem.as_deref()), (ProtectionSettings::default(), Some("[protection] level must be basic, careful or custom")));
    let (s, problem) = prot(None, None, None, None, Some("of")).resolve_or_basic();
    assert_eq!((s, problem.as_deref()), (ProtectionSettings::default(), Some("[protection] email must be immediate, report or off")));
    assert_eq!(protection_problem_line("[protection] email must be immediate, report or off"), "[protection]: email must be immediate, report or off; judging at Basic");
}

#[test]
fn an_empty_section_is_not_written_back() {
    let c: Config = toml::from_str("[protection]\n").unwrap();
    assert_eq!(c.protection, ProtectionConfig::default());
    assert!(!toml::to_string(&c).unwrap().contains("protection"));
    let set: Config = toml::from_str("[protection]\nlevel = \"careful\"\nemail = \"off\"\n").unwrap();
    assert_eq!(toml::from_str::<Config>(&toml::to_string(&set).unwrap()).unwrap(), set);
}

#[test]
fn report_time_off_disables_the_report() {
    let c = NotifyConfig { report_time: Some("off".into()), ..NotifyConfig::default() };
    assert_eq!(c.resolve().unwrap().report_time, None);
}

#[test]
fn bad_notify_values_are_refused_with_the_reason() {
    let bad = |c: NotifyConfig| c.resolve().unwrap_err();
    let base = NotifyConfig::default();
    for to in ["nobody", "a@", "@x.org", "a b@x.org", "a@x.org\nBcc: z@z.org", "a@x@y", "<a@x.org>", "a@x.org,,b@y.org"] {
        assert!(bad(NotifyConfig { mail_to: Some(to.into()), ..base.clone() }).contains("mail_to"), "{to:?}");
    }
    assert!(bad(NotifyConfig { check_interval_minutes: Some(0), ..base.clone() }).contains("check_interval_minutes"));
    assert!(bad(NotifyConfig { check_interval_minutes: Some(1441), ..base.clone() }).contains("check_interval_minutes"));
    assert!(bad(NotifyConfig { check_interval_minutes: Some(u64::MAX), ..base.clone() }).contains("check_interval_minutes"), "a huge value must not overflow");
    assert!(bad(NotifyConfig { remind_hours: Some(u64::MAX), ..base.clone() }).contains("remind_hours"));
    assert!(bad(NotifyConfig { report_time: Some("7am".into()), ..base.clone() }).contains("report_time"));
    assert!(bad(NotifyConfig { report_time: Some("25:00".into()), ..base.clone() }).contains("report_time"));
    assert!(bad(NotifyConfig { mail_command: Some("  ".into()), ..base.clone() }).contains("mail_command"));
    assert!(bad(NotifyConfig { mail_from: Some("a\r\nb".into()), ..base }).contains("mail_from"));
}

fn host(name: &str, target: &str, tag: Option<&str>) -> HostConfig {
    HostConfig { name: name.into(), ssh_target: Some(target.into()), tag: tag.map(str::to_string), site: None }
}

#[test]
fn a_host_block_has_its_keys_and_escapes_strings() {
    assert_eq!(render_host_block(&host("a", "u@a", None)), "[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n");
    let with_tag = render_host_block(&host("a\"b", "u@a", Some("back up")));
    let parsed: Config = toml::from_str(&with_tag).unwrap();
    assert_eq!(parsed.hosts, vec![host("a\"b", "u@a", Some("back up"))], "{with_tag}");
}

#[test]
fn a_new_host_lands_after_the_last_host_block_and_everything_else_stays() {
    let before = "# my hosts\n[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"   # first\n\n[[hosts]]\nname = \"b\"\nssh_target = \"u@b\"\n\n# UI below\n[ui]\ntheme = \"mono\"\n";
    let after = add_host_block(before, &host("new", "u@new", Some("lab"))).unwrap();
    assert_eq!(after, "# my hosts\n[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"   # first\n\n[[hosts]]\nname = \"b\"\nssh_target = \"u@b\"\n\n[[hosts]]\nname = \"new\"\nssh_target = \"u@new\"\ntag = \"lab\"\n\n# UI below\n[ui]\ntheme = \"mono\"\n");
}

#[test]
fn with_no_hosts_yet_the_block_goes_at_the_end_after_a_blank_line() {
    let before = "# only settings\n[ui]\ntheme = \"mono\"\n";
    assert_eq!(add_host_block(before, &host("a", "u@a", None)).unwrap(), "# only settings\n[ui]\ntheme = \"mono\"\n\n[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n");
    assert_eq!(add_host_block("", &host("a", "u@a", None)).unwrap(), "[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n");
    assert_eq!(add_host_block("[ui]\ntheme = \"mono\"", &host("a", "u@a", None)).unwrap(), "[ui]\ntheme = \"mono\"\n\n[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n");
}

#[test]
fn the_last_host_block_at_the_end_of_the_file_with_or_without_a_final_newline() {
    let end = "[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n";
    assert_eq!(add_host_block(end, &host("b", "u@b", None)).unwrap(), "[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n\n[[hosts]]\nname = \"b\"\nssh_target = \"u@b\"\n");
    let bare = "[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"";
    assert_eq!(add_host_block(bare, &host("b", "u@b", None)).unwrap(), "[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n\n[[hosts]]\nname = \"b\"\nssh_target = \"u@b\"\n");
}

#[test]
fn comments_after_the_last_host_belong_to_whatever_follows_it() {
    let before = "[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n# the notify section:\n# mail_to is the address\n[notify]\nmail_to = \"x@y.org\"\n";
    let after = add_host_block(before, &host("b", "u@b", None)).unwrap();
    assert_eq!(after, "[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n\n[[hosts]]\nname = \"b\"\nssh_target = \"u@b\"\n# the notify section:\n# mail_to is the address\n[notify]\nmail_to = \"x@y.org\"\n");
}

#[test]
fn crlf_files_stay_crlf_when_a_host_is_added() {
    let before = "[[hosts]]\r\nname = \"a\"\r\nssh_target = \"u@a\"\r\n\r\n[ui]\r\ntheme = \"mono\"\r\n";
    let after = add_host_block(before, &host("b", "u@b", None)).unwrap();
    assert_eq!(after, "[[hosts]]\r\nname = \"a\"\r\nssh_target = \"u@a\"\r\n\r\n[[hosts]]\r\nname = \"b\"\r\nssh_target = \"u@b\"\r\n\r\n[ui]\r\ntheme = \"mono\"\r\n");
}

#[test]
fn hosts_written_as_an_inline_array_or_dotted_keys_are_refused() {
    for bad in ["hosts = [{ name = \"a\", ssh_target = \"u@a\" }]\n", "hosts.x = 1\n", "[hosts.sub]\nx = 1\n"] {
        assert!(add_host_block(bad, &host("b", "u@b", None)).unwrap_err().contains("by hand"), "{bad:?}");
    }
}

#[test]
fn a_host_header_inside_a_multi_line_string_is_never_mistaken_for_a_host() {
    let text = "[extra]\ntext = \'\'\'\n[[hosts]]\nname = \"inside\"\nssh_target = \"x\"\n\'\'\'\n[ui]\ntheme = \"mono\"\n";
    let after = add_host_block(text, &host("b", "u@b", None)).expect("the block can still go in safely");
    let parsed: toml::Table = after.parse().unwrap();
    let old: toml::Table = text.parse().unwrap();
    assert_eq!(parsed["extra"], old["extra"], "the string is untouched");
    assert_eq!(toml::from_str::<Config>(&after).unwrap().hosts, vec![host("b", "u@b", None)]);
}

#[test]
fn a_header_looking_line_inside_a_hosts_own_multi_line_value_is_refused_not_split() {
    // The line-based scan would insert the new block inside this tag's string.
    let text = "[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\ntag = \'\'\'\nline\n[x]\n\'\'\'\n";
    assert!(add_host_block(text, &host("b", "u@b", None)).is_err());
}

#[test]
fn the_result_always_holds_the_old_hosts_plus_the_new_one() {
    let before = "[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\ntag = \"x\"\n";
    let after = add_host_block(before, &host("b", "u@b", None)).unwrap();
    let hosts = toml::from_str::<Config>(&after).unwrap().hosts;
    assert_eq!(hosts, vec![host("a", "u@a", Some("x")), host("b", "u@b", None)]);
}

#[test]
fn adding_a_host_to_the_file_keeps_comments_a_backup_and_the_other_sections() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let old = "# keep me\n[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n\n[notify]   # alerts\nmail_to = \"x@y.org\"\n";
    std::fs::write(&path, old).unwrap();
    append_host_to_file(&path, &host("b", "u@b", None)).unwrap();
    assert_eq!(std::fs::read_to_string(dir.path().join("config.toml.bak")).unwrap(), old);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "# keep me\n[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n\n[[hosts]]\nname = \"b\"\nssh_target = \"u@b\"\n\n[notify]   # alerts\nmail_to = \"x@y.org\"\n");
    // A refused edit leaves the file untouched.
    std::fs::write(&path, "hosts = []\n").unwrap();
    assert!(append_host_to_file(&path, &host("c", "u@c", None)).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hosts = []\n");
}

fn cfg(to: &str) -> NotifyConfig {
    NotifyConfig { mail_to: Some(to.into()), ..NotifyConfig::default() }
}

#[test]
fn the_rendered_section_has_only_the_set_keys_and_escapes_strings() {
    assert_eq!(render_notify_section(&NotifyConfig::default()), "[notify]\n");
    let c = NotifyConfig { mail_to: Some("a@x.org".into()), mail_command: Some("msmtp -a \"x\" -t \\ y".into()), check_interval_minutes: Some(10), report_time: Some("off".into()), remind_hours: Some(0), mail_from: None };
    let text = render_notify_section(&c);
    assert!(text.starts_with("[notify]\n") && text.ends_with('\n'));
    assert_eq!(toml::from_str::<Config>(&text).unwrap().notify, c, "{text}");
    assert!(!text.contains("mail_from"));
}

#[test]
fn an_existing_section_is_replaced_and_everything_else_is_kept_byte_for_byte() {
    let before = "# my hosts\n[[hosts]]\nname = \"a\"   # the first\nssh_target = \"u@a\"\n\n[notify]   # alerts\nmail_to = \"old@x.org\"\ncheck_interval_minutes = 9\n\n[ui]\ntheme = \"mono\"\n";
    let after = set_notify_section(before, &render_notify_section(&cfg("new@x.org"))).unwrap();
    assert_eq!(after, "# my hosts\n[[hosts]]\nname = \"a\"   # the first\nssh_target = \"u@a\"\n\n[notify]   # alerts\nmail_to = \"new@x.org\"\n\n[ui]\ntheme = \"mono\"\n");
}

#[test]
fn a_section_at_the_end_or_without_a_final_newline_is_replaced_too() {
    let end = "[ui]\ntheme = \"mono\"\n\n[notify]\nmail_to = \"old@x.org\"\n";
    assert_eq!(set_notify_section(end, &render_notify_section(&cfg("n@x.org"))).unwrap(), "[ui]\ntheme = \"mono\"\n\n[notify]\nmail_to = \"n@x.org\"\n");
    let bare = "[ui]\ntheme = \"mono\"\n\n[notify]\nmail_to = \"old@x.org\"";
    assert_eq!(set_notify_section(bare, &render_notify_section(&cfg("n@x.org"))).unwrap(), "[ui]\ntheme = \"mono\"\n\n[notify]\nmail_to = \"n@x.org\"\n");
}

#[test]
fn a_missing_section_is_appended_after_a_blank_line_and_an_empty_file_gets_just_the_section() {
    let before = "[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n";
    assert_eq!(set_notify_section(before, &render_notify_section(&cfg("n@x.org"))).unwrap(), "[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n\n[notify]\nmail_to = \"n@x.org\"\n");
    assert_eq!(set_notify_section("", &render_notify_section(&cfg("n@x.org"))).unwrap(), "[notify]\nmail_to = \"n@x.org\"\n");
    assert_eq!(set_notify_section("[ui]\ntheme = \"mono\"", &render_notify_section(&cfg("n@x.org"))).unwrap(), "[ui]\ntheme = \"mono\"\n\n[notify]\nmail_to = \"n@x.org\"\n");
}

#[test]
fn crlf_files_stay_crlf() {
    let before = "[ui]\r\ntheme = \"mono\"\r\n\r\n[notify]\r\nmail_to = \"old@x.org\"\r\n";
    let after = set_notify_section(before, &render_notify_section(&cfg("n@x.org"))).unwrap();
    assert_eq!(after, "[ui]\r\ntheme = \"mono\"\r\n\r\n[notify]\r\nmail_to = \"n@x.org\"\r\n");
}

#[test]
fn dotted_keys_inline_tables_subtables_and_duplicate_headers_are_refused() {
    let section = render_notify_section(&cfg("n@x.org"));
    for bad in ["notify.mail_to = \"a@x.org\"\n", "notify = { mail_to = \"a@x.org\" }\n", "[notify.extra]\nx = 1\n", "[notify]\nmail_to = \"a@x.org\"\n\n[ui]\n\n[notify]\n"] {
        assert!(set_notify_section(bad, &section).unwrap_err().contains("by hand"), "{bad:?}");
    }
    // A key that merely starts with the word is not a notify key.
    assert!(set_notify_section("[daemon]\nnotify_command = \"true\"\n", &section).is_ok());
}

#[test]
fn a_result_that_does_not_parse_or_does_not_say_what_was_asked_is_refused() {
    assert!(set_notify_section("[ui\ntheme = 1\n", &render_notify_section(&cfg("n@x.org"))).unwrap_err().contains("parse"));
    assert!(set_notify_section("", "[notify]\nmail_to = \n").is_err());
}

#[test]
fn saving_keeps_a_backup_the_permissions_and_every_other_byte() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let old = "# keep me\n[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n\n[notify]\nmail_to = \"old@x.org\"\n";
    std::fs::write(&path, old).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
    save_notify_section(&path, &render_notify_section(&cfg("new@x.org"))).unwrap();
    assert_eq!(std::fs::read_to_string(dir.path().join("config.toml.bak")).unwrap(), old);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "# keep me\n[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n\n[notify]\nmail_to = \"new@x.org\"\n");
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o640);
    let leftovers: Vec<String> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with("config.toml.tmp")).collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn a_refused_save_leaves_the_original_and_writes_no_backup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "notify.mail_to = \"x@y.org\"\n").unwrap();
    assert!(save_notify_section(&path, &render_notify_section(&cfg("n@x.org"))).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "notify.mail_to = \"x@y.org\"\n");
    assert!(!dir.path().join("config.toml.bak").exists());
}

#[test]
fn comments_blank_lines_and_other_keys_inside_and_around_the_section_survive_a_save() {
    let before = "[notify]  # alerts\n# mail_from = \"x@y\"\nmail_to = \"o@x.org\"\nmail_too = \"typo@x.org\"\ncheck_interval_minutes = 9\n\n# ---- UI settings below ----\n# theme options: mono, ...\n[ui]\ntheme = \"mono\"\n";
    let after = set_notify_section(before, &render_notify_section(&cfg("n@x.org"))).unwrap();
    assert_eq!(after, "[notify]  # alerts\nmail_to = \"n@x.org\"\n# mail_from = \"x@y\"\nmail_too = \"typo@x.org\"\n\n# ---- UI settings below ----\n# theme options: mono, ...\n[ui]\ntheme = \"mono\"\n");
    // Saving blank fields removes the keys this screen owns and leaves the rest.
    let off = set_notify_section(before, "[notify]\n").unwrap();
    assert!(!off.contains("mail_to =") && !off.contains("check_interval") && off.contains("mail_too") && off.contains("# mail_from") && off.contains("# ---- UI"), "{off}");
}

#[test]
fn text_inside_a_multi_line_string_or_an_unknown_table_is_never_changed() {
    let section = render_notify_section(&cfg("n@x.org"));
    // A header-looking line inside a string of an unknown table: the splice would eat lines, so it is refused.
    let tricky = "[extra]\ntext = \'\'\'\n[notify]\nkeep me\n[end]\'\'\'\n";
    assert!(set_notify_section(tricky, &section).is_err());
    assert_eq!(set_notify_section(tricky, "[notify]\n").unwrap(), tricky, "with nothing to write the text is returned untouched");
    let in_known = "[daemon]\nnotify_command = \"\"\"\n[notify]\n[x] \"\"\"\n";
    assert!(set_notify_section(in_known, &section).is_err());
}

#[test]
fn a_key_line_inside_a_string_is_not_removed_even_when_nothing_is_asked_for() {
    // Dropping our own key lines would eat this one: the whole-file comparison catches it where the notify
    // settings alone (which come out as the asked-for defaults) cannot.
    let text = "[extra]\ntext = \'\'\'\n[notify]\nmail_to = \"inside\"\n[end]\'\'\'\n";
    assert!(set_notify_section(text, "[notify]\n").is_err());
}

#[test]
fn a_notify_table_in_an_unusual_spelling_is_refused_with_the_by_hand_message() {
    let section = render_notify_section(&cfg("n@x.org"));
    for text in ["[ notify ]\nmail_to = \"a@x.org\"\n", "[\"notify\"]\nmail_to = \"a@x.org\"\n", "\"notify\" = { mail_to = \"a@x.org\" }\n", "\u{feff}[notify]\nmail_to = \"a@x.org\"\n"] {
        let err = set_notify_section(text, &section).unwrap_err();
        assert!(err.contains("by hand"), "{text:?}: {err}");
    }
}

#[test]
fn a_multi_line_value_for_one_of_our_keys_is_refused() {
    assert!(set_notify_section("[notify]\nmail_command = \"\"\"\nx\n\"\"\"\n", &render_notify_section(&cfg("n@x.org"))).is_err());
}

#[test]
fn a_symlinked_config_stays_a_symlink_and_the_target_is_updated() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real.toml");
    let link = dir.path().join("config.toml");
    std::fs::write(&real, "# mine\n[ui]\ntheme = \"mono\"\n").unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();
    save_notify_section(&link, &render_notify_section(&cfg("n@x.org"))).unwrap();
    assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "the link is still a link");
    assert!(std::fs::read_to_string(&real).unwrap().contains("mail_to = \"n@x.org\""));
    assert!(std::fs::read_to_string(&real).unwrap().starts_with("# mine"));
}

#[test]
fn a_file_that_reads_as_empty_but_is_not_is_refused() {
    // Simulated through the pure check used by the save: an empty read of a non-empty file.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n").unwrap();
    assert!(save_notify_section(&path, &render_notify_section(&cfg("n@x.org"))).is_ok());
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(saved.contains("[[hosts]]"), "the hosts survive: {saved}");
}

#[test]
fn a_missing_file_is_created_and_a_failing_write_leaves_the_original_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sub").join("config.toml");
    save_notify_section(&path, &render_notify_section(&cfg("n@x.org"))).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "[notify]\nmail_to = \"n@x.org\"\n");
    assert!(!dir.path().join("sub").join("config.toml.bak").exists(), "nothing to back up the first time");
    // A directory where the temporary file must go: the write fails, the original stays.
    std::fs::create_dir(dir.path().join("sub").join(format!("config.toml.tmp.{}", std::process::id()))).unwrap();
    let before = std::fs::read_to_string(&path).unwrap();
    let err = save_notify_section(&path, &render_notify_section(&cfg("other@x.org"))).unwrap_err().to_string();
    assert!(err.contains("config.toml.tmp"), "{err}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

#[test]
fn ui_theme_defaults_to_none_and_is_not_serialized_when_unset() {
    let config = Config::default();
    assert_eq!(config.ui.theme, None);
    let text = toml::to_string_pretty(&config).unwrap();
    assert!(!text.contains("[ui]"), "{text}");
    assert!(!text.contains("theme"), "{text}");
}

#[test]
fn ui_theme_parses_from_toml_alongside_hosts() {
    let text = "[ui]\ntheme = \"solarized-dark\"\n\n[[hosts]]\nname = \"backup\"\nssh_target = \"backup.example.com\"\n";
    let config: Config = toml::from_str(text).unwrap();
    assert_eq!(config.ui.theme.as_deref(), Some("solarized-dark"));
    assert_eq!(config.hosts.len(), 1);
}

#[test]
fn ui_theme_survives_a_save_load_round_trip_with_hosts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let config = Config {
        hosts: vec![HostConfig { name: "backup".to_string(), ssh_target: Some("backup.example.com".to_string()), tag: None, site: None }],
        ui: UiConfig { theme: Some("mono".to_string()), auto_refresh_secs: None },
        ..Default::default()
    };
    config.save(&path).unwrap();
    assert_eq!(Config::load(&path).unwrap(), config);
}

#[test]
fn auto_refresh_defaults_to_ten_seconds_and_zero_turns_it_off() {
    use std::time::Duration;
    let ui = |secs| UiConfig { theme: None, auto_refresh_secs: secs };
    assert_eq!(ui(None).auto_refresh_interval(), Some(Duration::from_secs(10)));
    assert_eq!(ui(Some(30)).auto_refresh_interval(), Some(Duration::from_secs(30)));
    assert_eq!(ui(Some(0)).auto_refresh_interval(), None);
}

#[test]
fn auto_refresh_secs_parses_from_toml_and_is_only_written_back_when_set() {
    let config: Config = toml::from_str("[ui]\nauto_refresh_secs = 0\n").unwrap();
    assert_eq!(config.ui.auto_refresh_interval(), None);
    let text = toml::to_string_pretty(&config).unwrap();
    assert!(text.contains("auto_refresh_secs = 0"), "{text}");
    assert!(!toml::to_string_pretty(&Config::default()).unwrap().contains("auto_refresh"));
}

#[test]
fn daemon_notify_command_parses_and_is_only_written_back_when_set() {
    let config: Config = toml::from_str("[daemon]\nnotify_command = \"curl -s ntfy.sh/zfs\"\n").unwrap();
    assert_eq!(config.daemon.notify_command.as_deref(), Some("curl -s ntfy.sh/zfs"));
    assert!(toml::to_string_pretty(&config).unwrap().contains("notify_command"));
    let text = toml::to_string_pretty(&Config::default()).unwrap();
    assert!(!text.contains("daemon") && !text.contains("notify"), "{text}");
}

fn with_groups(toml: &str) -> (ProtectionSettings, Vec<String>) {
    let text = format!("[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\ntag = \"db\"\n\n[[hosts]]\nname = \"b\"\nssh_target = \"u@b\"\ntag = \"web\"\n\n[[hosts]]\nname = \"c\"\nssh_target = \"u@c\"\n\n{toml}");
    toml::from_str::<Config>(&text).unwrap().protection_settings()
}

#[test]
fn without_groups_every_host_has_the_global_settings() {
    let (s, problems) = with_groups("[protection]\nlevel = \"careful\"\n");
    assert!(problems.is_empty(), "{problems:?}");
    for host in ["a", "b", "c", "local", "unknown"] {
        let e = s.for_host(host);
        assert_eq!((e.copies, e.online, e.tolerance, e.email), (3, 1, 2, EmailTiming::Immediate), "{host}");
    }
}

#[test]
fn a_group_overrides_only_the_keys_it_sets_and_only_for_its_hosts() {
    let (s, problems) = with_groups("[protection]\nrecent = \"strict\"\n\n[groups.db.protection]\nlevel = \"careful\"\nemail = \"off\"\n");
    assert!(problems.is_empty(), "{problems:?}");
    let db = s.for_host("a");
    assert_eq!((db.copies, db.online, db.tolerance, db.email), (3, 1, 1, EmailTiming::Off), "the level and the email are the group's, how recent is inherited");
    let web = s.for_host("b");
    assert_eq!((web.copies, web.online, web.tolerance, web.email), (1, 0, 1, EmailTiming::Immediate), "another group keeps the global settings");
    assert_eq!(s.for_host("c"), s.for_host("local"), "a host without a tag is global");
    assert_eq!((s.copies, s.online), (1, 0), "the global settings themselves are untouched");
}

#[test]
fn a_group_may_make_the_rule_looser_or_custom() {
    let (s, problems) = with_groups("[protection]\nlevel = \"careful\"\n\n[groups.web.protection]\nlevel = \"custom\"\ncopies = 2\nonline = 0\n\n[groups.db.protection]\nlevel = \"basic\"\n");
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!((s.for_host("b").copies, s.for_host("b").online), (2, 0));
    assert_eq!((s.for_host("a").copies, s.for_host("a").online), (1, 0));
    assert_eq!((s.for_host("c").copies, s.for_host("c").online), (3, 1));
}

#[test]
fn a_wrong_group_value_is_named_and_that_group_falls_back_to_the_global_settings() {
    let (s, problems) = with_groups("[protection]\nlevel = \"careful\"\n\n[groups.db.protection]\nlevel = \"paranoid\"\n\n[groups.web.protection]\nemail = \"report\"\n");
    assert_eq!(problems, ["[groups.db.protection] level must be basic, careful or custom"]);
    assert_eq!((s.for_host("a").copies, s.for_host("a").email), (3, EmailTiming::Immediate), "the broken group is judged as the global settings, never silently looser");
    assert_eq!(s.for_host("b").email, EmailTiming::Report, "a good group still applies");
}

#[test]
fn a_group_that_is_not_a_table_and_a_groups_that_is_not_a_table_are_problems_not_a_crash() {
    let (s, problems) = with_groups("[groups]\ndb = 5\n");
    assert_eq!(problems, ["[groups.db] must be a table"]);
    assert_eq!(s.for_host("a"), s.for_host("c"));
    let config: Config = toml::from_str("groups = 5\n").unwrap();
    assert_eq!(config.protection_settings().1, ["[groups] must be a table of groups, one per tag"]);
}

#[test]
fn a_global_protection_problem_still_comes_first_and_judges_at_basic() {
    let (s, problems) = with_groups("[protection]\nlevel = \"paranoid\"\n\n[groups.db.protection]\nlevel = \"careful\"\n");
    assert_eq!(problems[0], "[protection] level must be basic, careful or custom");
    assert_eq!(s.for_host("c").copies, 1);
    assert_eq!(s.for_host("a").copies, 3, "the group is judged on its own keys over the Basic defaults");
}

fn mail_of(toml: &str) -> (std::collections::BTreeMap<String, Vec<String>>, Vec<String>) {
    let text = format!("[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\ntag = \"db\"\n\n[[hosts]]\nname = \"b\"\nssh_target = \"u@b\"\ntag = \"web\"\n\n[[hosts]]\nname = \"c\"\nssh_target = \"u@c\"\n\n{toml}");
    toml::from_str::<Config>(&text).unwrap().group_mail()
}

#[test]
fn a_group_may_have_its_own_recipients_and_its_hosts_alerts_go_there() {
    let (mail, problems) = mail_of("[groups.db.notify]\nmail_to = \"dba@example.com, oncall@example.com\"\n");
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(mail.get("a").map(Vec::as_slice), Some(&["dba@example.com".to_string(), "oncall@example.com".to_string()][..]));
    assert!(!mail.contains_key("b") && !mail.contains_key("c"), "hosts of other groups keep the global recipients: {mail:?}");
}

#[test]
fn a_bad_group_address_is_a_named_problem_and_that_group_keeps_the_global_recipients() {
    let (mail, problems) = mail_of("[groups.db.notify]\nmail_to = \"not an address\"\n\n[groups.web.notify]\nmail_to = 5\n");
    assert!(mail.is_empty(), "{mail:?}");
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(problems.iter().any(|p| p.starts_with("[groups.db.notify] mail_to: 'not an address' is not a plain address")), "{problems:?}");
    assert!(problems.iter().any(|p| p == "[groups.web.notify] mail_to must be an address, or several separated by commas"), "{problems:?}");
}

#[test]
fn hosts_with_the_same_site_are_one_failure_domain_and_a_host_without_one_is_its_own() {
    let host = |name: &str, site: Option<&str>| HostConfig { name: name.into(), ssh_target: Some("u@h".into()), tag: None, site: site.map(str::to_string) };
    let config = Config { hosts: vec![host("a", Some("rack1")), host("b", Some(" rack1 ")), host("c", None), host("d", Some(" "))], ..Config::default() };
    let (settings, _) = config.protection_settings();
    assert_eq!((settings.domain_of("a"), settings.domain_of("b")), ("rack1".to_string(), "rack1".to_string()));
    assert_eq!((settings.domain_of("c"), settings.domain_of("d"), settings.domain_of("local")), ("c".to_string(), "d".to_string(), "local".to_string()));
    let text = render_host_block(&host("a", Some("rack1")));
    assert!(text.contains("site = \"rack1\""), "{text}");
}

#[test]
fn domains_is_a_number_from_one_to_three_laid_over_per_group_and_a_wrong_one_is_named() {
    let ok = |t: &str| toml::from_str::<Config>(t).unwrap().protection_settings();
    assert_eq!(ok("").0.domains, 1);
    assert_eq!(ok("[protection]\ndomains = 2\n").0.domains, 2);
    let (s, problems) = ok("[protection]\ndomains = 4\n");
    assert_eq!((s.domains, problems), (1, vec!["[protection] domains must be 1, 2 or 3".to_string()]));
    let (s, problems) = ok("[protection]\ndomains = 2\n\n[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\ntag = \"db\"\n\n[groups.db.protection]\ndomains = 3\n");
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!((s.for_host("a").domains, s.for_host("local").domains), (3, 2));
}
