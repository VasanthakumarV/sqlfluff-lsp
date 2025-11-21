# sqlfluff-lsp

[crates.io](https://crates.io/crates/sqlfluff-lsp)

---

Language Server for the SQL linting & formatting tool, SQLFluff.

> [!NOTE]
> The server expects [`sqlfluff`](https://github.com/sqlfluff/sqlfluff) to be installed and already added to the path. 

> [!WARNING]
> This tool might be rough around the edges, it is not widely tested.

## Configuration

### Helix

A sql dialect must be supplied either via `languages.toml` file (as shown below) or through a [sqlfluff configuration file](https://docs.sqlfluff.com/en/stable/configuration/setting_configuration.html#configuration-files).

For the list of dialects and their labels, please refer this [link](https://docs.sqlfluff.com/en/stable/reference/dialects.html).

```toml
[language-server.sqlfluff]
command = "sqlfluff-lsp"
args = ["serve", "--dialect=snowflake"]

[[language]]
name = "sql"
language-servers = ["sqlfluff"]
```
