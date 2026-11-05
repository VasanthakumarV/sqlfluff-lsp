use std::{collections::HashMap, process::Stdio};

use rootcause::{Report, prelude::ResultExt as _, report};
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tower_lsp_server::lsp_types::{
    self, CodeAction, CodeActionOrCommand, Diagnostic, DiagnosticSeverity, Position, Range,
    TextEdit, Uri, WorkspaceEdit,
};

use crate::lsp::{Config, FormatCommand};

#[derive(Deserialize, Debug, Clone)]
struct LintOutput {
    violations: Vec<Violation>,
}
#[derive(Deserialize, Debug, Clone)]
struct Violation {
    code: String,
    description: String,
    start_line_no: usize,
    start_line_pos: usize,
    end_line_no: Option<usize>,
    end_line_pos: Option<usize>,
    fixes: Option<Vec<Fix>>,
}
#[derive(Deserialize, Debug, Clone)]
pub struct Fix {
    pub edit: String,
    pub start_line_no: usize,
    pub start_line_pos: usize,
    pub end_line_no: usize,
    pub end_line_pos: usize,
}

#[derive(Debug, Clone)]
pub struct ParsedLint {
    pub has_parsing_error: bool,
    pub diagnostics: Vec<Diagnostic>,
    pub fixes: Vec<Option<ParsedFix>>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedFix {
    pub diag_lines: (u32, u32),
    pub code_action: CodeActionOrCommand,
}

pub async fn lint(uri: &Uri, content: &str, config: Config) -> Result<ParsedLint, Report> {
    let output = Sqlfluff::new("lint", config.sqlfluff_path)
        .dialect(config.dialect)
        .templater(config.templater)
        .args(&[
            &format!("--stdin-filename={}", uri.path()),
            "--disable-progress-bar",
            "--nocolor",
            "--format=json",
            "--nofail",
            "-",
        ])
        .execute(content)
        .await?;

    if output.status.success() {
        let output: Vec<LintOutput> = serde_json::from_slice(&output.stdout).attach(format!(
            "Failed to serialize the linting output from `sqlfluff`: {}",
            String::from_utf8_lossy(&output.stdout)
        ))?;

        let mut has_parsing_error = false;
        let (diagnostics, parsed_fixes): (Vec<_>, Vec<_>) = output
            .first()
            .expect("lint output should be for only one file")
            .violations
            .iter()
            .map(|lint| {
                let diag_range = Range {
                    start: Position {
                        line: (lint.start_line_no - 1) as u32,
                        character: (lint.start_line_pos - 1) as u32,
                    },
                    end: Position {
                        line: (lint.end_line_no.unwrap_or(lint.start_line_no) - 1) as u32,
                        character: (lint.end_line_pos.unwrap_or(lint.start_line_pos) - 1) as u32,
                    },
                };

                let diagnostic = Diagnostic {
                    range: diag_range,
                    severity: if lint.code == "PRS" {
                        has_parsing_error = true;
                        Some(DiagnosticSeverity::ERROR)
                    } else {
                        Some(DiagnosticSeverity::WARNING)
                    },
                    source: Some("sqlfluff-lsp".to_string()),
                    message: format!("{}: {}", lint.code, lint.description),
                    ..Default::default()
                };

                let parsed_fix = if let Some(fixes) = &lint.fixes
                    && !fixes.is_empty()
                {
                    let edits: Vec<_> = fixes
                        .iter()
                        .map(|fix| {
                            TextEdit::new(
                                lsp_types::Range::new(
                                    Position {
                                        line: (fix.start_line_no - 1) as u32,
                                        character: (fix.start_line_pos - 1) as u32,
                                    },
                                    Position {
                                        line: (fix.end_line_no - 1) as u32,
                                        character: (fix.end_line_pos - 1) as u32,
                                    },
                                ),
                                fix.edit.clone(),
                            )
                        })
                        .collect();
                    Some(ParsedFix {
                        diag_lines: (diag_range.start.line, diag_range.end.line),
                        code_action: CodeAction {
                            title: format!("Fix: {} in L{}", lint.code, lint.start_line_no),
                            edit: Some(WorkspaceEdit::new(HashMap::from([(
                                uri.clone(),
                                edits.clone(),
                            )]))),
                            ..Default::default()
                        }
                        .into(),
                    })
                } else {
                    None
                };

                (diagnostic, parsed_fix)
            })
            .unzip();

        Ok(ParsedLint {
            has_parsing_error,
            diagnostics,
            fixes: parsed_fixes,
        })
    } else {
        Err(report!("`sqlfluff lint` failed: {output:?}"))
    }
}

pub async fn fmt(uri: &Uri, content: &str, config: Config) -> Result<Vec<TextEdit>, Report> {
    let formatted_output = run_fmt(uri, content, config).await?;

    let (mut line_count, mut last_line_len) = (0, 0);
    let mut lines = content.lines().peekable();
    while let Some(line) = lines.next() {
        line_count += 1;
        if lines.peek().is_none() {
            last_line_len = line.encode_utf16().count() as u32;
        }
    }

    // TODO(optimization) send only the necessary edits
    Ok(vec![TextEdit::new(
        Range::new(
            Position::new(0, 0),
            Position::new(line_count, last_line_len),
        ),
        formatted_output,
    )])
}

/// Formats every line touched by `range`, keeping the indentation those lines share
pub async fn fmt_range(
    uri: &Uri,
    content: &str,
    range: Range,
    config: Config,
) -> Result<Vec<TextEdit>, Report> {
    let lines: Vec<&str> = content.split_inclusive('\n').collect();
    let start_line = range.start.line as usize;
    let end_line = if range.end.character == 0 && range.end.line > range.start.line {
        range.end.line - 1
    } else {
        range.end.line
    } as usize;
    let end_line = end_line.min(lines.len().saturating_sub(1));
    let Some(selected_lines) = lines.get(start_line..=end_line) else {
        return Ok(vec![]);
    };

    let selected = selected_lines.concat();
    let indent = common_indent(selected_lines);
    let dedented: String = selected_lines
        .iter()
        .map(|line| line.strip_prefix(indent).unwrap_or(line))
        .collect();

    let formatted_output = run_fmt(uri, &dedented, config).await?;

    let mut new_text: String = formatted_output
        .split_inclusive('\n')
        .map(|line| {
            if line.trim().is_empty() {
                line.to_string()
            } else {
                format!("{indent}{line}")
            }
        })
        .collect();
    if !selected.ends_with('\n') {
        new_text.truncate(new_text.trim_end_matches('\n').len());
    }
    if new_text == selected {
        return Ok(vec![]);
    }

    let end = if selected.ends_with('\n') {
        Position::new(end_line as u32 + 1, 0)
    } else {
        Position::new(
            end_line as u32,
            lines[end_line].encode_utf16().count() as u32,
        )
    };
    Ok(vec![TextEdit::new(
        Range::new(Position::new(start_line as u32, 0), end),
        new_text,
    )])
}

async fn run_fmt(uri: &Uri, content: &str, config: Config) -> Result<String, Report> {
    let command = match config.format_command {
        FormatCommand::Fix => "fix",
        FormatCommand::Format => "format",
    };
    let stdin_filename = format!("--stdin-filename={}", uri.path());
    let mut args = vec![
        stdin_filename.as_str(),
        "--disable-progress-bar",
        "--nocolor",
    ];
    // `sqlfluff format` has no `--quiet` flag
    if let FormatCommand::Fix = config.format_command {
        args.push("--quiet");
    }
    args.push("-");

    let output = Sqlfluff::new(command, config.sqlfluff_path)
        .dialect(config.dialect)
        .templater(config.templater)
        .args(&args)
        .execute(content)
        .await?;

    match output.status.code() {
        Some(0 | 1) => Ok(String::from_utf8_lossy(&output.stdout).into_owned()),
        _ => Err(report!("`sqlfluff {command}` failed: {output:?}")),
    }
}

fn common_indent<'a>(lines: &[&'a str]) -> &'a str {
    lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| &line[..line.len() - line.trim_start_matches([' ', '\t']).len()])
        .reduce(|common, indent| {
            let shared = common
                .bytes()
                .zip(indent.bytes())
                .take_while(|(a, b)| a == b)
                .count();
            &common[..shared]
        })
        .unwrap_or_default()
}

struct Sqlfluff {
    cmd: Command,
}

impl Sqlfluff {
    fn new(command: &str, sqlfluff_path: Option<String>) -> Self {
        let mut cmd = Command::new(sqlfluff_path.unwrap_or("sqlfluff".to_string()));
        cmd.arg(command);
        Sqlfluff { cmd }
    }
    fn dialect(mut self, dialect: Option<String>) -> Self {
        if let Some(d) = dialect {
            self.cmd.arg(format!("--dialect={d}"));
        }
        self
    }
    fn templater(mut self, templater: Option<String>) -> Self {
        if let Some(d) = templater {
            self.cmd.arg(format!("--templater={d}"));
        }
        self
    }
    fn args(mut self, args: &[&str]) -> Self {
        self.cmd.args(args);
        self
    }
    async fn execute(mut self, content: &str) -> Result<std::process::Output, Report> {
        let mut child = self
            .cmd
            .kill_on_drop(true)
            .stdout(Stdio::piped())
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .attach(format!("{:?} failed", self.cmd))?;

        {
            let mut stdin = child
                .stdin
                .take()
                .expect("child should have a handle to stdin");
            stdin.write_all(content.as_bytes()).await?;
        }

        Ok(child.wait_with_output().await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::{fs::File, io::Write, str::FromStr as _};

    use tempfile::tempdir;

    #[tokio::test]
    async fn test_fmt_simple() {
        let sql_file_content = "\
SELECT
    region, COUNT(*) AS total_customers, SUM(amount) AS total_sales
FROm customer
INNER JOIN sales ON customer.customer_id = sales.customer_id
 GROUP BY region
ORDER BY total_sales desc
        ";
        let expected_text_edit = TextEdit {
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 7,
                    character: 8,
                },
            },
            new_text: "\
SELECT
    region,
    COUNT(*) AS total_customers,
    SUM(amount) AS total_sales
FROM customer
INNER JOIN sales ON customer.customer_id = sales.customer_id
GROUP BY region
ORDER BY total_sales DESC
"
            .to_string(),
        };

        let tmp_dir = tempdir().unwrap();
        let file_path = tmp_dir.path().join("temp.sql");
        let mut tmp_file = File::create(&file_path).unwrap();
        writeln!(tmp_file, "{sql_file_content}").unwrap();

        let text_edits = fmt(
            &Uri::from_str(&file_path.as_os_str().to_string_lossy()).unwrap(),
            sql_file_content,
            Config {
                dialect: Some("snowflake".to_string()),
                templater: None,
                sqlfluff_path: None,
                format_command: FormatCommand::Fix,
            },
        )
        .await
        .unwrap();

        assert!(text_edits.len() == 1);
        assert_eq!(text_edits[0], expected_text_edit);
    }

    #[tokio::test]
    async fn test_fmt_with_format_command_only_changes_layout() {
        let sql_file_content = "select a,b from t join u on t.id=u.id\n";

        let tmp_dir = tempdir().unwrap();
        let file_path = tmp_dir.path().join("temp.sql");

        let text_edits = fmt(
            &Uri::from_str(&file_path.as_os_str().to_string_lossy()).unwrap(),
            sql_file_content,
            Config {
                dialect: Some("snowflake".to_string()),
                templater: None,
                sqlfluff_path: None,
                format_command: FormatCommand::Format,
            },
        )
        .await
        .unwrap();

        assert!(text_edits.len() == 1);
        assert_eq!(
            text_edits[0].new_text,
            "select\n    a,\n    b\nfrom t join u on t.id = u.id\n"
        );
    }

    #[tokio::test]
    async fn test_fmt_range_formats_only_selected_lines() {
        let sql_file_content = "SELECT a FROM t;\n\nselect b,c from u;";
        let expected_text_edit = TextEdit::new(
            Range::new(Position::new(2, 0), Position::new(2, 18)),
            "select\n    b,\n    c\nfrom u;".to_string(),
        );

        let tmp_dir = tempdir().unwrap();
        let file_path = tmp_dir.path().join("temp.sql");

        let text_edits = fmt_range(
            &Uri::from_str(&file_path.as_os_str().to_string_lossy()).unwrap(),
            sql_file_content,
            Range::new(Position::new(2, 0), Position::new(2, 18)),
            Config {
                dialect: Some("snowflake".to_string()),
                templater: None,
                sqlfluff_path: None,
                format_command: FormatCommand::Fix,
            },
        )
        .await
        .unwrap();

        assert_eq!(text_edits, vec![expected_text_edit]);
    }

    #[tokio::test]
    async fn test_fmt_range_extends_to_whole_lines_and_keeps_their_indentation() {
        let sql_file_content = "\
WITH x AS (
    SELECT a,b FROM t
)
SELECT * FROM x
";
        let expected_text_edit = TextEdit::new(
            Range::new(Position::new(1, 0), Position::new(2, 0)),
            "    SELECT\n        a,\n        b\n    FROM t\n".to_string(),
        );

        let tmp_dir = tempdir().unwrap();
        let file_path = tmp_dir.path().join("temp.sql");

        // only `a,b` is selected
        let text_edits = fmt_range(
            &Uri::from_str(&file_path.as_os_str().to_string_lossy()).unwrap(),
            sql_file_content,
            Range::new(Position::new(1, 11), Position::new(1, 14)),
            Config {
                dialect: Some("snowflake".to_string()),
                templater: None,
                sqlfluff_path: None,
                format_command: FormatCommand::Fix,
            },
        )
        .await
        .unwrap();

        assert_eq!(text_edits, vec![expected_text_edit]);
    }

    #[tokio::test]
    async fn test_lint_simple() {
        let sql_file_content = "\
SELECT
    region, COUNT(*) AS total_customers, SUM(amount) AS total_sales
FROm customer
            ";

        let tmp_dir = tempdir().unwrap();
        let file_path = tmp_dir.path().join("temp.sql");
        let mut tmp_file = File::create(&file_path).unwrap();
        writeln!(tmp_file, "{sql_file_content}").unwrap();

        let ParsedLint {
            has_parsing_error,
            diagnostics,
            fixes,
        } = lint(
            &Uri::from_str(&file_path.as_os_str().to_string_lossy()).unwrap(),
            sql_file_content,
            Config {
                dialect: Some("snowflake".to_string()),
                templater: None,
                sqlfluff_path: None,
                format_command: FormatCommand::Fix,
            },
        )
        .await
        .unwrap();

        let expected_has_parsing_error = false;
        assert_eq!(has_parsing_error, expected_has_parsing_error);

        let expected_fixes = vec![
            Some(ParsedFix {
                diag_lines: (0, 1),
                code_action: CodeActionOrCommand::CodeAction(CodeAction {
                    title: "Fix: LT09 in L1".to_string(),
                    kind: None,
                    diagnostics: None,
                    edit: Some(WorkspaceEdit {
                        changes: Some(HashMap::from([(
                            Uri::from_str(file_path.to_str().unwrap()).unwrap(),
                            vec![
                                TextEdit {
                                    range: Range {
                                        start: Position {
                                            line: 1,
                                            character: 11,
                                        },
                                        end: Position {
                                            line: 1,
                                            character: 12,
                                        },
                                    },
                                    new_text: "".to_string(),
                                },
                                TextEdit {
                                    range: Range {
                                        start: Position {
                                            line: 1,
                                            character: 12,
                                        },
                                        end: Position {
                                            line: 1,
                                            character: 12,
                                        },
                                    },
                                    new_text: "\n".to_string(),
                                },
                                TextEdit {
                                    range: Range {
                                        start: Position {
                                            line: 1,
                                            character: 40,
                                        },
                                        end: Position {
                                            line: 1,
                                            character: 41,
                                        },
                                    },
                                    new_text: "".to_string(),
                                },
                                TextEdit {
                                    range: Range {
                                        start: Position {
                                            line: 1,
                                            character: 41,
                                        },
                                        end: Position {
                                            line: 1,
                                            character: 41,
                                        },
                                    },
                                    new_text: "\n".to_string(),
                                },
                            ],
                        )])),
                        document_changes: None,
                        change_annotations: None,
                    }),
                    command: None,
                    is_preferred: None,
                    disabled: None,
                    data: None,
                }),
            }),
            Some(ParsedFix {
                diag_lines: (2, 2),
                code_action: CodeActionOrCommand::CodeAction(CodeAction {
                    title: "Fix: CP01 in L3".to_string(),
                    kind: None,
                    diagnostics: None,
                    edit: Some(WorkspaceEdit {
                        changes: Some(HashMap::from([(
                            Uri::from_str(file_path.to_str().unwrap()).unwrap(),
                            vec![TextEdit {
                                range: Range {
                                    start: Position {
                                        line: 2,
                                        character: 0,
                                    },
                                    end: Position {
                                        line: 2,
                                        character: 4,
                                    },
                                },
                                new_text: "FROM".to_string(),
                            }],
                        )])),
                        document_changes: None,
                        change_annotations: None,
                    }),
                    command: None,
                    is_preferred: None,
                    disabled: None,
                    data: None,
                }),
            }),
            Some(ParsedFix {
                diag_lines: (3, 3),
                code_action: CodeActionOrCommand::CodeAction(CodeAction {
                    title: "Fix: LT01 in L4".to_string(),
                    kind: None,
                    diagnostics: None,
                    edit: Some(WorkspaceEdit {
                        changes: Some(HashMap::from([(
                            Uri::from_str(file_path.to_str().unwrap()).unwrap(),
                            vec![TextEdit {
                                range: Range {
                                    start: Position {
                                        line: 3,
                                        character: 0,
                                    },
                                    end: Position {
                                        line: 3,
                                        character: 12,
                                    },
                                },
                                new_text: "".to_string(),
                            }],
                        )])),
                        document_changes: None,
                        change_annotations: None,
                    }),
                    command: None,
                    is_preferred: None,
                    disabled: None,
                    data: None,
                }),
            }),
        ];
        assert_eq!(fixes, expected_fixes);

        let expected_diagnostics = [
            Diagnostic {
                range: Range {
                    start: Position {
                        line: 0,
                        character: 0,
                    },
                    end: Position {
                        line: 1,
                        character: 67,
                    },
                },
                severity: Some(DiagnosticSeverity::WARNING),
                source: Some("sqlfluff-lsp".to_string()),
                message: "LT09: Select targets should be on a new line unless there is only one select target.".to_string(),
                ..Default::default()
            },
            Diagnostic {
                range: Range {
                    start: Position {
                        line: 2,
                        character: 0,
                    },
                    end: Position {
                        line: 2,
                        character: 4,
                    },
                },
                severity: Some(DiagnosticSeverity::WARNING),
                source: Some("sqlfluff-lsp".to_string()),
                message: "CP01: Keywords must be consistently upper case.".to_string(),
                ..Default::default()
            },
            Diagnostic {
                range: Range {
                    start: Position {
                        line: 3,
                        character: 0,
                    },
                    end: Position {
                        line: 3,
                        character: 12,
                    },
                },
                severity: Some(DiagnosticSeverity::WARNING),
                source: Some("sqlfluff-lsp".to_string()),
                message: "LT01: Unnecessary trailing whitespace at end of file.".to_string(),
                ..Default::default()
            },
        ];

        assert_eq!(diagnostics, expected_diagnostics);
    }
}
