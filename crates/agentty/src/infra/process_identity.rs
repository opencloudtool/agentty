//! Native creation timestamps without the precision loss of formatted `ps`
//! dates.

/// Platform-native process creation time: microseconds on macOS or clock ticks
/// since boot on Linux. Values are compared only within the current host run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProcessIdentity(pub(crate) u128);

impl ProcessIdentity {
    /// Reads creation identity from the kernel; missing or inaccessible
    /// processes have no usable identity.
    #[cfg(target_os = "macos")]
    pub(crate) fn read(pid: u32) -> Option<Self> {
        let info =
            libproc::proc_pid::pidinfo::<libproc::bsd_info::BSDInfo>(i32::try_from(pid).ok()?, 0)
                .ok()?;

        Some(Self(
            u128::from(info.pbi_start_tvsec) * 1_000_000 + u128::from(info.pbi_start_tvusec),
        ))
    }

    /// Reads the unrounded monotonic start tick from the process's stat file.
    #[cfg(target_os = "linux")]
    pub(crate) fn read(pid: u32) -> Option<Self> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;

        Self::from_linux_stat(&stat)
    }

    /// Unsupported hosts cannot supply a trustworthy accounting identity.
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub(crate) fn read(_pid: u32) -> Option<Self> {
        None
    }

    /// The command in parentheses can contain spaces and closing parentheses;
    /// field 22 follows nineteen fields after its final closing delimiter.
    #[cfg(any(target_os = "linux", test))]
    fn from_linux_stat(stat: &str) -> Option<Self> {
        let (_, fields) = stat.rsplit_once(')')?;

        fields.split_whitespace().nth(19)?.parse().ok().map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_creation_ticks_preserve_subsecond_identity_and_complex_names() {
        // Arrange
        let prefix = "42 (worker (tool) name) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18";

        // Act
        let first = ProcessIdentity::from_linux_stat(&format!("{prefix} 1001 1234"));
        let reused = ProcessIdentity::from_linux_stat(&format!("{prefix} 1002 1234"));

        // Assert
        assert_eq!(first, Some(ProcessIdentity(1001)));
        assert_eq!(reused, Some(ProcessIdentity(1002)));
        for malformed in ["", "42 (worker)", &format!("{prefix} invalid")] {
            assert!(ProcessIdentity::from_linux_stat(malformed).is_none());
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn native_identity_is_stable_for_owned_process_and_missing_for_invalid_pid() {
        // Arrange
        let pid = std::process::id();

        // Act
        let first = ProcessIdentity::read(pid).expect("current process identity");
        let second = ProcessIdentity::read(pid);

        // Assert
        assert_eq!(second, Some(first));
        assert!(first.0 > 0);
        assert!(ProcessIdentity::read(u32::MAX).is_none());
        assert!(ProcessIdentity::read(i32::MAX as u32).is_none());
    }
}
