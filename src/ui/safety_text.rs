//! The data-safety graph in words: the one line the Dashboard shows and the trees of its pop-up.

use crate::daemon::safety::{Body, PlaceKind};
use chrono::{DateTime, Duration, Utc};

/// How old something is: `10 min`, `3 h`, `2 d`; `unknown` when a hop on the way never succeeded.
pub fn span(age: Option<Duration>) -> String {
    let Some(age) = age else { return "unknown".to_string() };
    let minutes = age.num_minutes().max(0);
    match minutes {
        0 => "now".to_string(),
        m if m < 60 => format!("{m} min"),
        m if m < 2 * 24 * 60 => format!("{} h", m / 60),
        m => format!("{} d", m / (24 * 60)),
    }
}

fn kind_word(kind: PlaceKind) -> &'static str {
    match kind {
        PlaceKind::Original => "original",
        PlaceKind::Replica => "replica",
        PlaceKind::Backup => "backup",
        PlaceKind::Mirror => "mirror",
        PlaceKind::File => "stream file (weak)",
    }
}

fn failures(n: usize) -> String {
    match n {
        0 => "tolerates no failure".to_string(),
        1 => "tolerates 1 failure".to_string(),
        n => format!("tolerates {n} failures"),
    }
}

/// How the weakest body reads on the Dashboard, and whether it is worth a yellow: `None` when no dataset has a copy yet.
/// The weakest is the first of `bodies` (they come weakest first).
pub fn summary(bodies: &[Body], now: DateTime<Utc>) -> Option<(bool, String)> {
    let weakest = bodies.first()?;
    let readings: Vec<_> = (1..weakest.places.len()).map(|i| weakest.reading(i, now)).collect();
    let oldest = readings.iter().map(|r| r.age).try_fold(Duration::zero(), |acc, age| age.map(|a| acc.max(a)));
    let attention = readings.iter().any(|r| !r.current || r.age.is_none()) || weakest.tolerates(now) == 0;
    let id = weakest.places.first().map_or("?", |p| p.id.as_str());
    let many = if bodies.len() == 1 { "1 data set has copies".to_string() } else { format!("{} data sets have copies", bodies.len()) };
    Some((attention, format!("weakest: {id}  {}  · oldest copy {} · {many}", failures(weakest.tolerates(now)), span(oldest))))
}

/// One body as a tree: the original with what it tolerates, then each place with its kind, the age of the data there and
/// whether the way to it is current; and what stops being fed if a place fails.
pub fn tree(body: &Body, now: DateTime<Utc>) -> Vec<String> {
    fn walk(body: &Body, at: usize, prefix: &str, now: DateTime<Utc>, lines: &mut Vec<String>) {
        let children: Vec<usize> = body.edges.iter().filter(|e| e.from == at).map(|e| e.to).collect();
        for (n, child) in children.iter().enumerate() {
            let last = n + 1 == children.len();
            let Some(place) = body.places.get(*child) else { continue };
            let reading = body.reading(*child, now);
            let edge = body.edges.iter().find(|e| e.to == *child);
            let state = match (edge.and_then(|e| e.last_ok), reading.current) {
                (None, _) => "never ran",
                (Some(_), true) => "current",
                (Some(_), false) => "not current",
            };
            lines.push(format!("{prefix}{} {}  {}  {}  {state}", if last { "└─" } else { "├─" }, place.id, kind_word(place.kind), span(reading.age)));
            walk(body, *child, &format!("{prefix}{}", if last { "   " } else { "│  " }), now, lines);
        }
    }
    let mut lines = vec![format!("{}  {}", body.places.first().map_or("?", |p| p.id.as_str()), failures(body.tolerates(now)))];
    walk(body, 0, "", now, &mut lines);
    for (i, place) in body.places.iter().enumerate().skip(1) {
        let below = body.dependents(i);
        if !below.is_empty() {
            let names: Vec<&str> = below.iter().filter_map(|j| body.places.get(*j)).map(|p| p.id.as_str()).collect();
            lines.push(format!("  if {} fails, {} {} no longer fed", place.id, names.join(", "), if names.len() == 1 { "is" } else { "are" }));
        }
    }
    lines
}

/// The whole pop-up: every body's tree, the weakest first.
pub fn popup(bodies: &[Body], now: DateTime<Utc>) -> String {
    const SHOWN: usize = 200;
    if bodies.is_empty() {
        return "No dataset has a copy yet: the data lives only where it is.\n\nA replication, a backup or a cloud mirror job makes the first one (Jobs).".to_string();
    }
    let mut lines = vec!["Where each dataset's data lives, the weakest first. The age is the oldest hop on the way to a place; a place that is not current does not count as safe.".to_string(), String::new()];
    for body in bodies.iter().take(SHOWN) {
        lines.extend(tree(body, now));
        lines.push(String::new());
    }
    if bodies.len() > SHOWN {
        lines.push(format!("(+{} more)", bodies.len() - SHOWN));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProtectionSettings;
    use crate::daemon::history::{Run, RunResult};
    use crate::daemon::safety::bodies;
    use crate::jobs::Job;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-24T12:00:00Z").unwrap().with_timezone(&Utc)
    }

    fn ok_at(job: &str, ago: Duration) -> Run {
        let at = now() - ago;
        Run { job_id: job.into(), run_id: format!("{job}-{}", ago.num_minutes()), started_at: at, scheduled_for: None, finished_at: Some(at), result: Some(RunResult::Ok), message: String::new(), catch_up: false, summary: None }
    }

    fn on(host: &str, job: Job) -> Job {
        Job { host: host.into(), ..job }
    }

    fn chain(replica_ago: Duration, mirror_ago: Duration) -> Vec<Body> {
        let jobs = vec![on("a", Job::test_replicate("r", "tank/data", "rep", "b", "tank/copy")), on("b", Job::test_mirror("m", "tank/copy", "m", "b2:bkt/x"))];
        let runs = vec![ok_at("r", replica_ago), ok_at("m", mirror_ago)];
        bodies(&jobs, &runs, &ProtectionSettings::default(), &|h| h.to_string(), now())
    }

    #[test]
    fn spans_are_short_and_say_unknown_when_they_are() {
        assert_eq!([span(Some(Duration::seconds(20))), span(Some(Duration::minutes(10))), span(Some(Duration::hours(3))), span(Some(Duration::hours(47))), span(Some(Duration::days(3))), span(None)], ["now", "10 min", "3 h", "47 h", "3 d", "unknown"]);
    }

    #[test]
    fn a_chain_is_drawn_as_a_tree_with_what_depends_on_what() {
        let all = chain(Duration::minutes(10), Duration::hours(3));
        assert_eq!(
            tree(&all[0], now()),
            [
                "a:tank/data  tolerates 2 failures",
                "└─ b:tank/copy  replica  10 min  current",
                "   └─ b2:bkt/x  mirror  3 h  current",
                "  if b:tank/copy fails, b2:bkt/x is no longer fed",
            ]
        );
    }

    #[test]
    fn branches_are_drawn_apart_and_a_stopped_or_never_run_job_is_said() {
        let jobs = vec![on("a", Job::test_replicate("r", "tank/data", "rep", "b", "tank/copy")), on("a", Job::test_mirror("m", "tank/data", "m", "b2:bkt/x")), on("a", Job::test_backup("k", "tank/data", "bk", "sftp:nas:/r", "/pw"))];
        let runs = vec![ok_at("r", Duration::minutes(5)), ok_at("m", Duration::days(4))];
        let all = bodies(&jobs, &runs, &ProtectionSettings::default(), &|h| h.to_string(), now());
        assert_eq!(
            tree(&all[0], now()),
            ["a:tank/data  tolerates 1 failure", "├─ b:tank/copy  replica  5 min  current", "├─ b2:bkt/x  mirror  4 d  not current", "└─ sftp:nas:/r  backup  unknown  never ran"]
        );
    }

    #[test]
    fn the_dashboard_line_names_the_weakest_data_and_is_yellow_when_something_is_not_current() {
        let (attention, text) = summary(&chain(Duration::minutes(10), Duration::hours(3)), now()).unwrap();
        assert_eq!(text, "weakest: a:tank/data  tolerates 2 failures  · oldest copy 3 h · 1 data set has copies");
        assert!(!attention);
        let (attention, text) = summary(&chain(Duration::minutes(10), Duration::days(5)), now()).unwrap();
        assert!(attention && text.contains("tolerates 1 failure") && text.contains("oldest copy 5 d"), "{text}");
        assert!(summary(&[], now()).is_none(), "nothing to say when no dataset has a copy");
    }

    #[test]
    fn the_popup_explains_itself_lists_every_body_and_says_so_when_there_is_none() {
        let text = popup(&chain(Duration::minutes(10), Duration::hours(3)), now());
        assert!(text.starts_with("Where each dataset's data lives") && text.contains("a:tank/data  tolerates 2 failures"), "{text}");
        assert!(popup(&[], now()).starts_with("No dataset has a copy yet"));
    }
}
