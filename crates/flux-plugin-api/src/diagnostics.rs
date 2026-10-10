//! Problems in files. Read what the language servers (and other plugins) report — `get` needs the
//! `project` permission — or publish the plugin's own: a linter's results show as the servers' do,
//! underlined, on F2, in the status bar's counters.
//!
//! ```ignore
//! let problems = vec![
//!     Diagnostic::warning(Range::on_line(3, 0, 12), "Unquoted variable").code("SC2086"),
//! ];
//! diagnostics::publish(&path, &problems); // replaces what the plugin published for the file
//! diagnostics::clear();                   // all of the plugin's, everywhere
//! ```

use crate::Range;
pub use crate::host::diagnostics::{Diagnostic, FileDiagnostics, Severity, clear, get, publish};

impl Diagnostic {
    pub fn new(severity: Severity, range: Range, message: &str) -> Self {
        Diagnostic {
            range,
            severity,
            message: message.to_string(),
            source: None,
            code: None,
        }
    }

    pub fn error(range: Range, message: &str) -> Self {
        Self::new(Severity::Error, range, message)
    }

    pub fn warning(range: Range, message: &str) -> Self {
        Self::new(Severity::Warning, range, message)
    }

    pub fn info(range: Range, message: &str) -> Self {
        Self::new(Severity::Info, range, message)
    }

    pub fn hint(range: Range, message: &str) -> Self {
        Self::new(Severity::Hint, range, message)
    }

    /// Who reports it: "shellcheck" (the plugin's name by default).
    pub fn source(mut self, source: &str) -> Self {
        self.source = Some(source.to_string());
        self
    }

    /// The rule: "SC2086".
    pub fn code(mut self, code: &str) -> Self {
        self.code = Some(code.to_string());
        self
    }
}

/// Every problem Flux knows of, by file.
pub fn all() -> Vec<FileDiagnostics> {
    get(None)
}

/// The problems of one file (relative to the project root or absolute).
pub fn of(path: &str) -> Vec<Diagnostic> {
    get(Some(path))
        .into_iter()
        .flat_map(|file| file.diagnostics)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_problems() {
        let problem = Diagnostic::warning(Range::on_line(3, 0, 12), "Unquoted").code("SC2086");
        assert_eq!(problem.severity, Severity::Warning);
        assert_eq!(problem.code.as_deref(), Some("SC2086"));
        assert_eq!(problem.source, None);
        assert_eq!(problem.range.end.column, 12);
    }
}
