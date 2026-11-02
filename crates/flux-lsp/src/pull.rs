//! Pull diagnostics (LSP 3.17, `textDocument/diagnostic`): a server that has `diagnosticProvider`
//! is asked for a document's problems instead of publishing them (ESLint's server publishes none).
//! The client keeps each document's last `resultId`: with it the server may answer "unchanged".

use lsp_types::{
    Diagnostic, DiagnosticServerCapabilities, DocumentDiagnosticReport,
    DocumentDiagnosticReportKind, DocumentDiagnosticReportResult, ServerCapabilities, Uri,
};

/// What a server said about one file's problems.
#[derive(Debug, Clone, PartialEq)]
pub enum PulledReport {
    /// All of them; `result_id` — what to name next time.
    Full {
        result_id: Option<String>,
        items: Vec<Diagnostic>,
    },
    /// The same as for the `previousResultId` sent.
    Unchanged { result_id: String },
}

/// An answer to `textDocument/diagnostic`: the document's report, if any (a partial result has
/// none), and those of related files the server checked with it.
#[derive(Debug, Clone, PartialEq)]
pub struct Pulled {
    pub report: Option<PulledReport>,
    pub related: Vec<(Uri, PulledReport)>,
}

/// The `identifier` of the server's pulled diagnostics, if it has them (`diagnosticProvider`);
/// `Some(None)` — it has them without an identifier.
pub fn provider(capabilities: &ServerCapabilities) -> Option<Option<String>> {
    capabilities
        .diagnostic_provider
        .as_ref()
        .map(|provider| match provider {
            DiagnosticServerCapabilities::Options(options) => options.identifier.clone(),
            DiagnosticServerCapabilities::RegistrationOptions(options) => {
                options.diagnostic_options.identifier.clone()
            }
        })
}

impl From<DocumentDiagnosticReportResult> for Pulled {
    fn from(result: DocumentDiagnosticReportResult) -> Self {
        let kind = |report: DocumentDiagnosticReportKind| match report {
            DocumentDiagnosticReportKind::Full(full) => PulledReport::Full {
                result_id: full.result_id,
                items: full.items,
            },
            DocumentDiagnosticReportKind::Unchanged(unchanged) => PulledReport::Unchanged {
                result_id: unchanged.result_id,
            },
        };
        let related =
            |documents: Option<std::collections::HashMap<Uri, DocumentDiagnosticReportKind>>| {
                let mut related: Vec<(Uri, PulledReport)> = documents
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(uri, report)| (uri, kind(report)))
                    .collect();
                related.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
                related
            };
        match result {
            DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(full)) => {
                Pulled {
                    report: Some(PulledReport::Full {
                        result_id: full.full_document_diagnostic_report.result_id,
                        items: full.full_document_diagnostic_report.items,
                    }),
                    related: related(full.related_documents),
                }
            }
            DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Unchanged(
                unchanged,
            )) => Pulled {
                report: Some(PulledReport::Unchanged {
                    result_id: unchanged.unchanged_document_diagnostic_report.result_id,
                }),
                related: related(unchanged.related_documents),
            },
            DocumentDiagnosticReportResult::Partial(partial) => Pulled {
                report: None,
                related: related(partial.related_documents),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pulled(value: serde_json::Value) -> Pulled {
        serde_json::from_value::<DocumentDiagnosticReportResult>(value)
            .unwrap()
            .into()
    }

    #[test]
    fn reports_full_unchanged_and_related() {
        let full = pulled(json!({
            "kind": "full",
            "resultId": "7",
            "items": [{
                "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } },
                "message": "no-unused-vars",
            }],
            "relatedDocuments": {
                "file:///p/b.ts": { "kind": "unchanged", "resultId": "3" },
                "file:///p/a.ts": { "kind": "full", "items": [] },
            },
        }));
        let Some(PulledReport::Full { result_id, items }) = &full.report else {
            panic!("a full report expected: {full:?}");
        };
        assert_eq!(result_id.as_deref(), Some("7"));
        assert_eq!(items[0].message, "no-unused-vars");
        let related: Vec<(&str, &PulledReport)> = full
            .related
            .iter()
            .map(|(uri, report)| (uri.as_str(), report))
            .collect();
        assert_eq!(
            related,
            [
                (
                    "file:///p/a.ts",
                    &PulledReport::Full {
                        result_id: None,
                        items: Vec::new()
                    }
                ),
                (
                    "file:///p/b.ts",
                    &PulledReport::Unchanged {
                        result_id: "3".into()
                    }
                ),
            ]
        );
        let unchanged = pulled(json!({ "kind": "unchanged", "resultId": "7" }));
        assert_eq!(
            unchanged.report,
            Some(PulledReport::Unchanged {
                result_id: "7".into()
            })
        );
        assert!(unchanged.related.is_empty());
    }

    #[test]
    fn the_provider_and_its_identifier() {
        let capabilities: ServerCapabilities = serde_json::from_value(json!({
            "diagnosticProvider": {
                "identifier": "eslint",
                "interFileDependencies": false,
                "workspaceDiagnostics": false,
            },
        }))
        .unwrap();
        assert_eq!(provider(&capabilities), Some(Some("eslint".into())));
        assert_eq!(provider(&ServerCapabilities::default()), None);
    }
}
