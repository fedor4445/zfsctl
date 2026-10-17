//! Reading `smartctl -j` output: which disks of a host have a problem. Pure: the daemon runs the commands, the
//! Dashboard and the daily report show what this finds.

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum Severity {
    Watch,
    Issue,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Problem {
    pub severity: Severity,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Disk {
    pub dev: String,
    pub problems: Vec<Problem>,
}

/// Wear (percent used) from which an SSD is worth a look, and the temperature (Celsius) from which any disk is.
const WEAR_WATCH: u64 = 80;
const TEMPERATURE_WATCH: u64 = 60;

/// The device names `smartctl --scan -j` lists (`/dev/ada0` becomes `ada0`); empty for anything unreadable.
pub fn parse_scan(json: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else { return Vec::new() };
    let Some(devices) = value.get("devices").and_then(|d| d.as_array()) else { return Vec::new() };
    devices.iter().filter_map(|d| d.get("name")?.as_str()).map(|n| n.strip_prefix("/dev/").unwrap_or(n).to_string()).filter(|n| !n.is_empty()).collect()
}

/// What `smartctl -j -H -A -i /dev/<dev>` says about one disk. `None`: the disk does not report SMART (or the output
/// is not something to judge), so it stays silent. `Some` with no problems: healthy.
pub fn judge(dev: &str, json: &str) -> Option<Disk> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    value.as_object()?;
    // Only a disk that says it supports SMART can be "unreadable": a virtual disk or a USB bridge says nothing, and
    // a smartctl that does not know -j prints usage text, which is not JSON at all.
    if value.pointer("/smart_support/available").and_then(|a| a.as_bool()) != Some(true) {
        return None;
    }
    let table = value.pointer("/ata_smart_attributes/table").and_then(|t| t.as_array());
    let nvme = value.get("nvme_smart_health_information_log");
    let status = value.pointer("/smart_status/passed").and_then(|p| p.as_bool());
    let mut problems = Vec::new();
    let mut add = |severity, text: String| problems.push(Problem { severity, text });
    if table.is_none_or(|t| t.is_empty()) && nvme.is_none() && status.is_none() {
        add(Severity::Watch, "SMART unreadable".to_string());
        return Some(Disk { dev: dev.to_string(), problems });
    }
    // For NVMe smartctl says "not passed" whenever any critical-warning bit is set, a low spare or a hot moment
    // included; only "reliability degraded" (0x04) and "read-only" (0x08) are the drive giving up.
    let warning = nvme.and_then(|log| log.get("critical_warning")?.as_u64()).unwrap_or(0);
    if status == Some(false) && (nvme.is_none() || warning == 0 || warning & 0x0c != 0) {
        add(Severity::Issue, "SMART health FAILED".to_string());
    }
    let mut wear: Option<u64> = None;
    if let Some(table) = table {
        let attribute = |id: u64| table.iter().find(|a| a.get("id").and_then(|i| i.as_u64()) == Some(id));
        let raw = |id: u64| attribute(id).and_then(|a| a.pointer("/raw/value")?.as_u64()).filter(|n| *n > 0);
        for (id, what) in [(5, "reallocated sectors"), (197, "pending sectors"), (198, "uncorrectable sectors")] {
            if let Some(n) = raw(id) {
                add(Severity::Watch, format!("{what} {n}"));
            }
        }
        // These count down from 100 as the flash wears out. The ids are reused by other things (233 is often a write
        // counter, 231 a temperature), so only attributes with a wear name count.
        const REMAINING: [&str; 4] = ["wear_leveling_count", "media_wearout_indicator", "ssd_life_left", "percent_lifetime_remain"];
        wear = table
            .iter()
            .filter(|a| a.get("name").and_then(|n| n.as_str()).is_some_and(|n| REMAINING.contains(&n.to_ascii_lowercase().as_str())))
            .filter_map(|a| a.get("value")?.as_u64())
            .filter(|v| *v <= 100)
            .map(|v| 100 - v)
            .max();
    }
    if let Some(log) = nvme {
        let number = |key: &str| log.get(key).and_then(|n| n.as_u64());
        let mut spare_said = false;
        if let (Some(spare), Some(threshold)) = (number("available_spare"), number("available_spare_threshold")) {
            if spare < threshold {
                add(Severity::Watch, format!("spare capacity {spare}%"));
                spare_said = true;
            }
        }
        // Bit 0 is that spare warning (said above), 0x04 and 0x08 are the red verdict; any other bit is a note.
        let other = warning & !(if spare_said { 0x01 } else { 0 } | 0x0c);
        if other != 0 {
            add(Severity::Watch, format!("critical warning 0x{other:02x}"));
        }
        wear = wear.max(number("percentage_used"));
    }
    // smartctl 7.5 reports the endurance used for ATA, SCSI and NVMe alike; it beats guessing from attributes.
    if let Some(used) = value.pointer("/endurance_used/current_percent").and_then(|u| u.as_u64()) {
        wear = Some(used);
    }
    if let Some(w) = wear.filter(|w| *w >= WEAR_WATCH) {
        add(Severity::Watch, format!("SSD wear {w}%"));
    }
    if let Some(t) = value.pointer("/temperature/current").and_then(|t| t.as_u64()).filter(|t| *t >= TEMPERATURE_WATCH) {
        add(Severity::Watch, format!("temperature {t} C"));
    }
    Some(Disk { dev: dev.to_string(), problems })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk(json: &str) -> Disk {
        judge("d", json).unwrap_or_else(|| panic!("expected a verdict for {json}"))
    }

    fn texts(d: &Disk) -> Vec<(Severity, &str)> {
        d.problems.iter().map(|p| (p.severity, p.text.as_str())).collect()
    }

    #[test]
    fn a_healthy_sata_hdd_has_no_problems() {
        assert_eq!(texts(&disk(include_str!("smart_fixtures/hdd_ok.json"))), []);
    }

    #[test]
    fn reallocated_sectors_are_a_watch_with_the_count() {
        assert_eq!(texts(&disk(include_str!("smart_fixtures/hdd_reallocated.json"))), [(Severity::Watch, "reallocated sectors 8")]);
    }

    #[test]
    fn a_hot_disk_is_a_watch_from_60_degrees() {
        assert_eq!(texts(&disk(include_str!("smart_fixtures/hdd_hot.json"))), [(Severity::Watch, "temperature 63 C")]);
    }

    #[test]
    fn an_ssd_at_87_percent_wear_is_a_watch() {
        assert_eq!(texts(&disk(include_str!("smart_fixtures/ssd_worn.json"))), [(Severity::Watch, "SSD wear 87%")]);
    }

    #[test]
    fn nvme_wear_and_a_low_spare_are_watches_and_a_healthy_one_is_not() {
        assert_eq!(texts(&disk(include_str!("smart_fixtures/nvme_ok.json"))), []);
        assert_eq!(texts(&disk(include_str!("smart_fixtures/nvme_low_spare.json"))), [(Severity::Watch, "spare capacity 8%")]);
        assert_eq!(texts(&disk(include_str!("smart_fixtures/nvme_worn.json"))), [(Severity::Watch, "SSD wear 91%")]);
    }

    #[test]
    fn a_failed_overall_health_is_an_issue() {
        assert_eq!(texts(&disk(include_str!("smart_fixtures/hdd_failed.json"))), [(Severity::Issue, "SMART health FAILED")]);
    }

    #[test]
    fn real_smartctl_output_from_a_qemu_disk_is_healthy_and_says_nothing() {
        // Taken from the test hub (smartmontools 7.x, `smartctl -j -H -A -i /dev/ada0`; serial and wwn removed).
        assert_eq!(texts(&disk(include_str!("smart_fixtures/real_qemu_ata.json"))), []);
    }

    #[test]
    fn a_disk_without_smart_support_is_silent() {
        assert_eq!(judge("da0", include_str!("smart_fixtures/no_support.json")), None);
    }

    #[test]
    fn a_disk_that_claims_support_but_has_no_attribute_table_is_unreadable() {
        assert_eq!(texts(&disk(include_str!("smart_fixtures/no_table.json"))), [(Severity::Watch, "SMART unreadable")]);
    }

    #[test]
    fn garbage_truncated_empty_and_braces_only_never_panic_and_are_silent() {
        for json in [include_str!("smart_fixtures/truncated.json"), include_str!("smart_fixtures/empty.json"), include_str!("smart_fixtures/braces.json"), "smartctl: unrecognized option -j", "[1,2,3]", "null"] {
            assert_eq!(judge("d", json), None, "{json}");
        }
    }

    fn ata_with(extra: serde_json::Value) -> String {
        let mut base: serde_json::Value = serde_json::from_str(include_str!("smart_fixtures/hdd_ok.json")).unwrap();
        let table = base["ata_smart_attributes"]["table"].as_array_mut().unwrap();
        table.push(extra);
        base.to_string()
    }

    #[test]
    fn ssd_wear_is_read_by_attribute_name_so_counters_and_temperatures_are_not_wear() {
        let attr = |id: u64, name: &str, value: u64| serde_json::json!({"id": id, "name": name, "value": value, "worst": value, "thresh": 0, "raw": {"value": 0, "string": "0"}});
        // 233 as a write counter and 231 as a controller temperature say nothing about wear.
        assert_eq!(texts(&disk(&ata_with(attr(233, "Total_LBAs_Written", 1)))), []);
        assert_eq!(texts(&disk(&ata_with(attr(231, "Controller_Temperature", 15)))), []);
        assert_eq!(texts(&disk(&ata_with(attr(202, "Percent_Lifetime_Remain", 10)))), [(Severity::Watch, "SSD wear 90%")]);
        assert_eq!(texts(&disk(&ata_with(attr(233, "Media_Wearout_Indicator", 5)))), [(Severity::Watch, "SSD wear 95%")]);
    }

    #[test]
    fn smartctl_75_endurance_used_is_preferred_when_present() {
        let mut v: serde_json::Value = serde_json::from_str(include_str!("smart_fixtures/hdd_ok.json")).unwrap();
        v["endurance_used"] = serde_json::json!({"current_percent": 92});
        assert_eq!(texts(&disk(&v.to_string())), [(Severity::Watch, "SSD wear 92%")]);
    }

    #[test]
    fn an_nvme_critical_warning_is_red_only_for_reliability_or_read_only_and_never_twice() {
        let nvme = |warning: u64, spare: u64| {
            let mut v: serde_json::Value = serde_json::from_str(include_str!("smart_fixtures/nvme_ok.json")).unwrap();
            v["smart_status"]["passed"] = serde_json::json!(warning == 0);
            v["nvme_smart_health_information_log"]["critical_warning"] = serde_json::json!(warning);
            v["nvme_smart_health_information_log"]["available_spare"] = serde_json::json!(spare);
            v.to_string()
        };
        // smartctl reports "not passed" whenever any warning bit is set; a real low spare also sets bit 0.
        assert_eq!(texts(&disk(&nvme(0x01, 8))), [(Severity::Watch, "spare capacity 8%")]);
        assert_eq!(texts(&disk(&nvme(0x02, 100))), [(Severity::Watch, "critical warning 0x02")]);
        assert_eq!(texts(&disk(&nvme(0x04, 100))), [(Severity::Issue, "SMART health FAILED")]);
        assert_eq!(texts(&disk(&nvme(0x08, 100))), [(Severity::Issue, "SMART health FAILED")]);
    }

    #[test]
    fn parse_scan_lists_devices_without_dev_and_survives_garbage() {
        assert_eq!(parse_scan(include_str!("smart_fixtures/scan.json")), ["ada0", "nvme0"]);
        for json in ["", "{}", "garbage", "{\"devices\":[{\"type\":\"sat\"}]}"] {
            assert!(parse_scan(json).is_empty(), "{json}");
        }
    }
}
