# sqlfluff-lsp

| [crates.io](https://crates.io/crates/sqlfluff-lsp) | [conda-forge](https://anaconda.org/channels/conda-forge/packages/sqlfluff-lsp/overview) |

Language Server for the SQL linting & formatting tool, SQLFluff.

> [!NOTE]
> The server expects [`sqlfluff`](https://github.com/sqlfluff/sqlfluff) to be installed and added to the PATH.\
> If that is not feasible, pass the absolute path via `--sqlfluff-path` flag,\
> e.g. `sqlfluff-lsp serve --dialect=ansi --sqlfluff-path=/path/to/bin/sqlfluff`.

> [!WARNING]
> This tool might be rough around the edges, it is not widely tested.

## Installation

<details>

<summary>cargo</summary>

From crates.io using `cargo`,

```sh
cargo install sqlfluff-lsp
```

</details>

<details>

<summary>pixi</summary>

Package also published in conda-forge (`sqlfluff` cli is already listed as `sqlfuff-lsp`'s runtime dependency here).

You can use [pixi](https://pixi.sh/latest/) for global installation,

```sh
pixi global install --expose sqlfluff-lsp --expose sqlfluff sqlfluff-lsp
```

or within a project,

```sh
pixi add sqlfluff-lsp
```

</details>

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
