pub mod dataset;
pub mod pool;
pub mod snapshot;

/// Parses a `-H -p` numeric column that may show `-` when the value is
/// unavailable (e.g. `zpool list` prints `-` for SIZE/ALLOC/FREE on a
/// FAULTED/UNAVAIL pool). Treats `-` as `0` rather than a parse error,
/// so unhealthy pools/datasets/snapshots still show up in listings.
pub(crate) fn parse_numeric_or_dash(s: &str) -> Option<u64> {
    if s == "-" {
        Some(0)
    } else {
        s.parse().ok()
    }
}
