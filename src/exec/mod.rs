pub mod local;
pub mod ssh;

use crate::error::Result;

#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub status: i32,
}

impl CommandOutput {
    pub fn success(&self) -> bool {
        self.status == 0
    }
}

pub trait CommandRunner: Send + Sync {
    fn run(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput>;
}

#[cfg(test)]
pub mod test_support {
    use super::{CommandOutput, CommandRunner};
    use crate::error::Result;
    use std::sync::Mutex;

    /// A `CommandRunner` that records every call it receives and, by
    /// default, always returns the same scripted `CommandOutput`. Used
    /// by unit tests throughout the crate so ZFS/host logic can be
    /// tested without a real system.
    ///
    /// Some library functions (e.g.
    /// `zfs::disk::list_disks_with_membership`) compose several
    /// different subcommands in one call — a single scripted response
    /// can't stand in for `geom disk list`, `zpool list`, and `zpool
    /// status` all at once. `route` scripts a distinct response for
    /// calls matching a given `cmd` and first argument (e.g. `("zpool",
    /// "status")` vs `("zpool", "list")`); calls that don't match any
    /// route fall back to `response`.
    pub struct FakeRunner {
        pub calls: Mutex<Vec<(String, Vec<String>)>>,
        pub response: CommandOutput,
        routes: std::collections::HashMap<(String, String), CommandOutput>,
    }

    impl FakeRunner {
        pub fn new(stdout: &str) -> Self {
            FakeRunner {
                calls: Mutex::new(Vec::new()),
                response: CommandOutput {
                    stdout: stdout.to_string(),
                    stderr: String::new(),
                    status: 0,
                },
                routes: std::collections::HashMap::new(),
            }
        }

        pub fn failing(stderr: &str, status: i32) -> Self {
            FakeRunner {
                calls: Mutex::new(Vec::new()),
                response: CommandOutput {
                    stdout: String::new(),
                    stderr: stderr.to_string(),
                    status,
                },
                routes: std::collections::HashMap::new(),
            }
        }

        /// Scripts a distinct successful response for calls matching
        /// `cmd` whose first argument is `first_arg`. Builder-style —
        /// call before wrapping the runner in `Arc`.
        pub fn route(mut self, cmd: &str, first_arg: &str, stdout: &str) -> Self {
            self.routes.insert(
                (cmd.to_string(), first_arg.to_string()),
                CommandOutput { stdout: stdout.to_string(), stderr: String::new(), status: 0 },
            );
            self
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
            self.calls
                .lock()
                .unwrap()
                .push((cmd.to_string(), args.iter().map(|s| s.to_string()).collect()));
            let first_arg = args.first().copied().unwrap_or("");
            if let Some(routed) = self.routes.get(&(cmd.to_string(), first_arg.to_string())) {
                return Ok(routed.clone());
            }
            Ok(self.response.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::FakeRunner;
    use super::CommandRunner;

    #[test]
    fn fake_runner_route_overrides_default_response_for_matching_cmd_and_first_arg() {
        let runner = FakeRunner::new("default")
            .route("zpool", "status", "status-output")
            .route("zpool", "list", "list-output");
        assert_eq!(runner.run("zpool", &["status", "--", "tank"]).unwrap().stdout, "status-output");
        assert_eq!(runner.run("zpool", &["list", "-H"]).unwrap().stdout, "list-output");
        assert_eq!(
            runner.run("geom", &["disk", "list"]).unwrap().stdout,
            "default",
            "an unrouted call falls back to the default response"
        );
    }
}
