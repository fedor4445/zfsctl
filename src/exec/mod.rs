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

    /// A `CommandRunner` that records every call it receives and always
    /// returns the same scripted `CommandOutput`. Used by unit tests
    /// throughout the crate so ZFS/host logic can be tested without a
    /// real system.
    pub struct FakeRunner {
        pub calls: Mutex<Vec<(String, Vec<String>)>>,
        pub response: CommandOutput,
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
            }
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
            self.calls
                .lock()
                .unwrap()
                .push((cmd.to_string(), args.iter().map(|s| s.to_string()).collect()));
            Ok(self.response.clone())
        }
    }
}
