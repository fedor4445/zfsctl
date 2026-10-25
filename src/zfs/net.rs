//! Network interface traffic and link state for the Hosts panel.
//!
//! Two read-only, unprivileged commands feed it: `netstat -ibn` for the byte counters (read before and
//! after the one-second disk window, so the rate costs no extra waiting) and `ifconfig -a` for whether
//! each link is up and what speed it negotiated. Both are parsed leniently: a host whose output looks
//! different (not FreeBSD) yields no interfaces instead of an error.

/// One interface's running byte totals, from a `<Link#n>` row of `netstat -ibn`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Counters {
    pub name: String,
    pub in_bytes: u64,
    pub out_bytes: u64,
}

/// What `ifconfig -a` says about one interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkFacts {
    pub name: String,
    /// The `UP` flag is set and the link has a carrier.
    pub up: bool,
    /// The negotiated media, such as `10Gbase-T`; `None` when the interface reports none.
    pub speed: Option<String>,
    /// Loopback and housekeeping interfaces nobody wants to watch.
    pub hidden: bool,
}

/// One interface as the panel shows it: link state, speed and the current rates in bytes per second.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NetIface {
    pub name: String,
    pub up: bool,
    pub speed: Option<String>,
    pub in_bps: u64,
    pub out_bps: u64,
}

/// The `<Link#n>` rows of `netstat -ibn`: name (without the `*` that marks a downed interface), bytes in and
/// bytes out. The counters are read from the right-hand end of the row, because the Address column is empty
/// on some interfaces and shifts everything before it.
pub fn parse_netstat_ibn(output: &str) -> Vec<Counters> {
    output
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let last = fields.len().checked_sub(1)?;
            if !fields.get(2)?.starts_with("<Link#") {
                return None;
            }
            // ... Ipkts Ierrs Idrop Ibytes Opkts Oerrs Obytes Coll
            let in_bytes = fields.get(last.checked_sub(4)?)?.parse().ok()?;
            let out_bytes = fields.get(last.checked_sub(1)?)?.parse().ok()?;
            Some(Counters { name: fields.first()?.trim_end_matches('*').to_string(), in_bytes, out_bytes })
        })
        .collect()
}

/// `ifconfig -a`: one block per interface, the first line `name: flags=…<UP,…> …` and the rest indented.
pub fn parse_ifconfig(output: &str) -> Vec<LinkFacts> {
    let mut links: Vec<LinkFacts> = Vec::new();
    for line in output.lines() {
        if !line.starts_with([' ', '\t']) {
            let Some((name, rest)) = line.split_once(": flags=") else { continue };
            let flags = rest.split_once('<').and_then(|(_, f)| f.split_once('>')).map_or("", |(f, _)| f);
            let has = |flag: &str| flags.split(',').any(|f| f == flag);
            let hidden = has("LOOPBACK") || ["lo", "pflog", "pfsync", "enc"].iter().any(|p| name.starts_with(p));
            links.push(LinkFacts { name: name.to_string(), up: has("UP"), speed: None, hidden });
            continue;
        }
        let Some(link) = links.last_mut() else { continue };
        let detail = line.trim();
        if let Some(media) = detail.strip_prefix("media:") {
            link.speed = media
                .split_once('(')
                .map(|(_, rest)| rest.split([' ', '<', ')']).next().unwrap_or(""))
                .filter(|s| !s.is_empty() && *s != "autoselect" && *s != "none")
                .map(str::to_string);
        } else if detail.strip_prefix("status:").is_some_and(|s| s.trim() == "no carrier") {
            link.up = false;
        }
    }
    links
}

/// The interfaces to show, in `ifconfig` order: every visible one that also has counters, with its rate over
/// `elapsed_ms` (zero for one that appeared after `before` was read, or whose counter went backwards).
pub fn rates(before: &[Counters], after: &[Counters], links: &[LinkFacts], elapsed_ms: u64) -> Vec<NetIface> {
    let per_sec = |from: u64, to: u64| u64::try_from(u128::from(to.saturating_sub(from)) * 1000 / u128::from(elapsed_ms.max(1))).unwrap_or(u64::MAX);
    links
        .iter()
        .filter(|l| !l.hidden)
        .filter_map(|link| {
            let now = after.iter().find(|c| c.name == link.name)?;
            let (in_bps, out_bps) = match before.iter().find(|c| c.name == link.name) {
                Some(then) => (per_sec(then.in_bytes, now.in_bytes), per_sec(then.out_bytes, now.out_bytes)),
                None => (0, 0),
            };
            Some(NetIface { name: link.name.clone(), up: link.up, speed: link.speed.clone(), in_bps, out_bps })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Captured from the FreeBSD 15.1 hub.
    const NETSTAT: &str = "\
Name         Mtu Network              Address                             Ipkts Ierrs Idrop      Ibytes     Opkts Oerrs      Obytes  Coll
vtnet0      1500 <Link#1>             00:00:5e:00:53:01                11190393     0     0  2911160044   9533711     0  3063139595     0
vtnet0         - 172.16.10.0/24       172.16.10.90                     10207674     -     -  2698696579  10127315     -  2929574173     -
vtnet1*     1500 <Link#2>             00:00:5e:00:53:02                       0     0     0           0         0     0           0     0
lo0        16384 <Link#3>             lo0                                   967     0     0      128196       967     0      128196     0
bastille0* 16384 <Link#4>             bastille0                               0     0     0           0         0     0           0     0
tailscale0  1280 <Link#5>             tailscale0                          14197     0     0     1203893     13331     0     1581457     0
tailscale0     - 100.100.100.10/32    100.100.100.10                      11691     -     -      970789     10868     -     1337511     -
";

    const IFCONFIG: &str = "\
vtnet0: flags=1008843<UP,BROADCAST,RUNNING,SIMPLEX,MULTICAST,LOWER_UP> metric 0 mtu 1500
\toptions=ec07bb<RXCSUM,TXCSUM,VLAN_MTU,LINKSTATE>
\tether 00:00:5e:00:53:01
\tinet 172.16.10.90 netmask 0xffffff00 broadcast 172.16.10.255
\tmedia: Ethernet autoselect (10Gbase-T <full-duplex>)
\tstatus: active
vtnet1: flags=1008902<BROADCAST,PROMISC,SIMPLEX,MULTICAST,LOWER_UP> metric 0 mtu 1500
\tether 00:00:5e:00:53:02
\tmedia: Ethernet autoselect (10Gbase-T <full-duplex>)
\tstatus: active
lo0: flags=1008049<UP,LOOPBACK,RUNNING,MULTICAST,LOWER_UP> metric 0 mtu 16384
\tinet 127.0.0.1 netmask 0xff000000
bastille0: flags=8008<LOOPBACK,MULTICAST> metric 0 mtu 16384
\tgroups: lo
tailscale0: flags=1008043<UP,BROADCAST,RUNNING,MULTICAST,LOWER_UP> metric 0 mtu 1280
\tinet 100.100.100.10 netmask 0xffffffff broadcast 100.100.100.10
\tgroups: tun
";

    fn counters(name: &str, i: u64, o: u64) -> Counters {
        Counters { name: name.to_string(), in_bytes: i, out_bytes: o }
    }

    #[test]
    fn netstat_gives_one_row_per_interface_from_the_link_lines_only() {
        let c = parse_netstat_ibn(NETSTAT);
        let names: Vec<&str> = c.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["vtnet0", "vtnet1", "lo0", "bastille0", "tailscale0"], "the star on a downed interface is not part of its name");
        assert_eq!((c[0].in_bytes, c[0].out_bytes), (2_911_160_044, 3_063_139_595), "the address rows below are not summed in");
        assert_eq!((c[4].in_bytes, c[4].out_bytes), (1_203_893, 1_581_457));
    }

    #[test]
    fn netstat_reads_the_counters_from_the_right_when_the_address_column_is_empty() {
        let row = "wg0         1420 <Link#9>                                          5     0     0         700         6     0         800     0\n";
        assert_eq!(parse_netstat_ibn(row), [counters("wg0", 700, 800)]);
    }

    #[test]
    fn netstat_output_that_is_not_freebsd_is_no_interfaces() {
        assert!(parse_netstat_ibn("").is_empty());
        assert!(parse_netstat_ibn("netstat: invalid option -- 'b'\nUsage: netstat [options]\n").is_empty());
    }

    #[test]
    fn ifconfig_tells_up_from_down_speed_and_what_to_hide() {
        let l = parse_ifconfig(IFCONFIG);
        let row = |n: &str| l.iter().find(|l| l.name == n).unwrap().clone();
        assert_eq!((row("vtnet0").up, row("vtnet0").speed.as_deref(), row("vtnet0").hidden), (true, Some("10Gbase-T"), false));
        assert!(!row("vtnet1").up, "no UP flag: switched off, although its status says active");
        assert_eq!((row("tailscale0").up, row("tailscale0").speed), (true, None));
        assert!(row("lo0").hidden && row("bastille0").hidden, "loopbacks are hidden");
    }

    #[test]
    fn ifconfig_no_carrier_is_down_and_housekeeping_interfaces_are_hidden() {
        let out = "em0: flags=8843<UP,BROADCAST,RUNNING,SIMPLEX,MULTICAST> metric 0 mtu 1500\n\tmedia: Ethernet autoselect\n\tstatus: no carrier\npflog0: flags=0 metric 0 mtu 33152\nenc0: flags=0 metric 0 mtu 1536\n";
        let l = parse_ifconfig(out);
        assert_eq!((l[0].up, l[0].speed.clone()), (false, None));
        assert!(l[1].hidden && l[2].hidden);
        assert!(parse_ifconfig("garbage\n").is_empty());
    }

    #[test]
    fn rates_are_the_counter_difference_per_second_and_hide_hidden_interfaces() {
        let before = [counters("vtnet0", 1_000, 5_000), counters("lo0", 0, 0), counters("tailscale0", 10, 10)];
        let after = [counters("vtnet0", 3_000, 6_000), counters("lo0", 99, 99), counters("tailscale0", 10, 10), counters("vtnet1", 0, 0)];
        let r = rates(&before, &after, &parse_ifconfig(IFCONFIG), 500);
        let names: Vec<&str> = r.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["vtnet0", "vtnet1", "tailscale0"], "ifconfig order, no loopbacks");
        assert_eq!((r[0].in_bps, r[0].out_bps), (4_000, 2_000), "2000 bytes in 500 ms is 4000 a second");
        assert_eq!((r[1].in_bps, r[1].out_bps), (0, 0), "new since the first reading: no rate yet");
        assert!(r[0].up && !r[1].up);
    }

    #[test]
    fn a_counter_that_went_backwards_is_a_zero_rate_not_a_huge_one() {
        let links = parse_ifconfig(IFCONFIG);
        let r = rates(&[counters("vtnet0", 900, 900)], &[counters("vtnet0", 100, 100)], &links, 1000);
        assert_eq!((r[0].in_bps, r[0].out_bps), (0, 0));
        assert_eq!(rates(&[], &[], &links, 0), []);
    }
}
