//! Windowing mode abstraction.
//!
//! Native multi-window is supported on every desktop platform. Windows uses
//! the patched winit dependency that prevents multi-window redraw starvation.

/// Determines how the application handles multiple windows and popouts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowingMode {
    /// Each pane popout opens a separate OS-native window.
    /// Supported on Windows, macOS, and Linux.
    NativeMultiWindow,
    /// All UI is rendered inside a single native window.
    /// Panes that would pop out are instead docked/maximized internally.
    /// Retained as an internal fallback mode.
    SingleWindowEmbedded,
}

impl WindowingMode {
    /// Returns the default windowing mode.
    pub fn platform_default() -> Self {
        Self::NativeMultiWindow
    }

    /// Returns `true` if native popout windows are allowed.
    pub fn allows_native_popout(&self) -> bool {
        matches!(self, Self::NativeMultiWindow)
    }

    /// Returns a human-readable reason string for logging.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::NativeMultiWindow => "platform_supported",
            Self::SingleWindowEmbedded => "explicit_single_window_mode",
        }
    }
}

impl std::fmt::Display for WindowingMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NativeMultiWindow => write!(f, "NativeMultiWindow"),
            Self::SingleWindowEmbedded => write!(f, "SingleWindowEmbedded"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_default_allows_native_popouts() {
        assert_eq!(
            WindowingMode::platform_default(),
            WindowingMode::NativeMultiWindow
        );
        assert!(WindowingMode::platform_default().allows_native_popout());
    }

    #[test]
    fn native_popout_only_in_multi_window() {
        assert!(WindowingMode::NativeMultiWindow.allows_native_popout());
        assert!(!WindowingMode::SingleWindowEmbedded.allows_native_popout());
    }
}
