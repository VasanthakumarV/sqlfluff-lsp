# sqlfluff-lsp

Language Server for the SQL linting & formatting tool, SQLFluff.

> [!WARNING]
> This tool might be rough around the edges, it is not widely tested

## Configuration

### Helix

A sql dialect must be supplied either via `languages.toml` file (as shown below) or through a [sqlfluff configuration file](https://docs.sqlfluff.com/en/stable/configuration/setting_configuration.html#configuration-files).

```toml
[language-server.sqlfluff]
command = "sqlfluff-lsp"
args = ["serve", "--dialect=snowflake"]

[[language]]
name = "sql"
language-servers = ["sqlfluff"]
```
