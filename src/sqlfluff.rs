use std::process::Stdio;

use anyhow::Context as _;
use serde::Deserialize;
use tokio::io::{self, AsyncWriteExt};
use tokio::process::Command;
use tower_lsp_server::lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range, TextEdit, Uri};

#[derive(Deserialize, Debug)]
struct LintOutput {
    start_line: usize,
    start_column: usize,
    end_line: usize,
    end_column: usize,
    message: String,
}

pub async fn lint(
    uri: &Uri,
    content: &str,
    dialect: Option<String>,
) -> anyhow::Result<Vec<Diagnostic>> {
    let output = Sqlfluff::new("lint")
        .dialect(dialect)
        .args(&[
            &format!("--stdin-filename={}", uri.path()),
            "--disable-progress-bar",
            "--nocolor",
            "--format=github-annotation",
            "--nofail",
            "-",
        ])
        .execute(content)
        .await?;

    if output.status.success() {
        let output: Vec<LintOutput> =
            serde_json::from_slice(&output.stdout).with_context(|| {
                format!(
                    "Failed to serialize the linting output from `sqlfluff`: {}",
                    String::from_utf8_lossy(&output.stdout)
                )
            })?;

        Ok(output
            .into_iter()
            .map(|lint| Diagnostic {
                range: Range {
                    start: Position {
                        line: (lint.start_line - 1) as u32,
                        character: (lint.start_column - 1) as u32,
                    },
                    end: Position {
                        line: (lint.end_line - 1) as u32,
                        character: (lint.end_column - 1) as u32,
                    },
                },
                severity: Some(DiagnosticSeverity::WARNING),
                source: Some("sqlfluff-lsp".to_string()),
                message: lint.message,
                ..Default::default()
            })
            .collect::<Vec<_>>())
    } else {
        anyhow::bail!("`sqlfluff lint` failed: {output:?}")
    }
}

pub async fn fmt(
    uri: &Uri,
    content: &str,
    dialect: Option<String>,
) -> anyhow::Result<Vec<TextEdit>> {
    let output = Sqlfluff::new("fix")
        .dialect(dialect)
        .args(&[
            &format!("--stdin-filename={}", uri.path()),
            "--disable-progress-bar",
            "--nocolor",
            "--quiet",
            "-",
        ])
        .execute(content)
        .await?;

    let formatted_output = match output.status.code() {
        Some(0 | 1) => String::from_utf8_lossy(&output.stdout).into_owned(),
        _ => anyhow::bail!("`sqlfluff fix` failed: {output:?}"),
    };

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

struct Sqlfluff {
    cmd: Command,
}

impl Sqlfluff {
    fn new(command: &str) -> Self {
        let mut cmd = Command::new("sqlfluff");
        cmd.arg(command);
        Sqlfluff { cmd }
    }
    fn dialect(mut self, dialect: Option<String>) -> Self {
        if let Some(d) = dialect {
            self.cmd.arg(format!("--dialect={d}"));
        }
        self
    }
    fn args(mut self, args: &[&str]) -> Self {
        self.cmd.args(args);
        self
    }
    async fn execute(mut self, content: &str) -> io::Result<std::process::Output> {
        let mut child = self
            .cmd
            .kill_on_drop(true)
            .stdout(Stdio::piped())
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        {
            let mut stdin = child
                .stdin
                .take()
                .expect("child should have a handle to stdin");
            stdin.write_all(content.as_bytes()).await?;
        }

        child.wait_with_output().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs::File;
    use std::io::Write;
    use std::str::FromStr as _;
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
            Some("snowflake".to_string()),
        )
        .await
        .unwrap();

        assert!(text_edits.len() == 1);
        assert_eq!(text_edits[0], expected_text_edit);
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

        let diagnostics = lint(
            &Uri::from_str(&file_path.as_os_str().to_string_lossy()).unwrap(),
            sql_file_content,
            Some("snowflake".to_string()),
        )
        .await
        .unwrap();

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
