use std::collections::HashMap;

use tokio::sync::{RwLock, watch};
use tower_lsp_server::{Client, LanguageServer, jsonrpc::Result, lsp_types::*};

use crate::sqlfluff::{self, ParsedFix, ParsedLint};

#[derive(Debug)]
pub struct Backend {
    client: Client,
    config: Config,
    watchers: RwLock<HashMap<Uri, Watcher>>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub dialect: Option<String>,
    pub templater: Option<String>,
    pub sqlfluff_path: Option<String>,
}

#[derive(Debug)]
struct Watcher {
    content_tx: watch::Sender<String>,
    content_rx: watch::Receiver<String>,
    fix_rx: watch::Receiver<(bool, Vec<Option<ParsedFix>>)>,
}

impl Backend {
    pub fn new(
        client: Client,
        dialect: Option<String>,
        templater: Option<String>,
        sqlfluff_path: Option<String>,
    ) -> Self {
        Self {
            client,
            config: Config {
                dialect,
                templater,
                sqlfluff_path,
            },
            watchers: RwLock::new(HashMap::new()),
        }
    }
}

impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            server_info: None,
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                document_formatting_provider: Some(OneOf::Right(DocumentFormattingOptions {
                    work_done_progress_options: WorkDoneProgressOptions {
                        work_done_progress: Some(false),
                    },
                })),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
                ..ServerCapabilities::default()
            },
        })
    }

    async fn initialized(&self, _: InitializedParams) {}

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn code_action(
        &self,
        CodeActionParams {
            text_document: TextDocumentIdentifier { uri },
            range:
                Range {
                    start:
                        Position {
                            line: start_line, ..
                        },
                    end:
                        Position {
                            line: mut end_line,
                            character: end_pos,
                        },
                },
            ..
        }: CodeActionParams,
    ) -> Result<Option<CodeActionResponse>> {
        if end_pos == 0 {
            end_line -= 1;
        }
        if let Some((has_parsing_error, fixes)) = self
            .watchers
            .read()
            .await
            .get(&uri)
            .map(|watcher| watcher.fix_rx.borrow().clone())
        {
            let filtered_code_actions: Vec<_> = fixes
                .into_iter()
                .flatten()
                .filter_map(
                    |ParsedFix {
                         diag_lines,
                         code_action,
                     }| {
                        if diag_lines.0 <= end_line && start_line <= diag_lines.1 {
                            Some(code_action)
                        } else {
                            None
                        }
                    },
                )
                .collect();
            if !filtered_code_actions.is_empty() && has_parsing_error {
                self.client
                    .show_message(
                        MessageType::WARNING,
                        "Fix parsing error(s) [PRS] to avoid applying wrong fixes",
                    )
                    .await;
            }
            Ok(Some(filtered_code_actions))
        } else {
            Ok(None)
        }
    }

    async fn formatting(
        &self,
        DocumentFormattingParams {
            text_document: TextDocumentIdentifier { uri },
            ..
        }: DocumentFormattingParams,
    ) -> Result<Option<Vec<TextEdit>>> {
        let config = self.config.clone();
        if let Some((content, (has_parsing_error, _))) =
            self.watchers.read().await.get(&uri).map(|watcher| {
                (
                    watcher.content_rx.borrow().clone(),
                    watcher.fix_rx.borrow().clone(),
                )
            })
        {
            if has_parsing_error {
                self.client
                    .show_message(
                        MessageType::ERROR,
                        "Fix parsing error(s) [PRS] in the document before formatting",
                    )
                    .await;
                return Ok(None);
            }
            let output = match sqlfluff::fmt(&uri, &content, config).await {
                Ok(output) => output,
                Err(error) => {
                    eprintln!("{error}");
                    self.client.show_message(MessageType::ERROR, error).await;
                    return Ok(None);
                }
            };

            Ok(Some(output))
        } else {
            Ok(None)
        }
    }

    async fn did_open(
        &self,
        DidOpenTextDocumentParams {
            text_document: TextDocumentItem { uri, text, .. },
        }: DidOpenTextDocumentParams,
    ) {
        let config = self.config.clone();
        self.watchers
            .write()
            .await
            .entry(uri.clone())
            .and_modify(|watcher| watcher.content_tx.send(text.clone()).unwrap())
            .or_insert_with(|| {
                let (content_tx, content_rx) = watch::channel(text);
                let (fix_tx, fix_rx) = watch::channel((false, vec![]));

                let client = self.client.clone();
                let mut content_rx_clone = content_rx.clone();
                let fix_tx_clone = fix_tx.clone();
                tokio::spawn(async move {
                    loop {
                        let content = content_rx_clone.borrow_and_update().clone();

                        match sqlfluff::lint(&uri, &content, config.clone()).await {
                            Ok(ParsedLint {
                                has_parsing_error,
                                diagnostics,
                                fixes,
                            }) => {
                                client
                                    .publish_diagnostics(uri.clone(), diagnostics, None)
                                    .await;
                                fix_tx_clone.send((has_parsing_error, fixes)).unwrap();
                            }
                            Err(error) => {
                                eprintln!("{error}");
                                client.show_message(MessageType::ERROR, error).await;
                            }
                        }

                        if content_rx_clone.changed().await.is_err() {
                            break;
                        }
                    }
                });

                Watcher {
                    content_tx,
                    content_rx,
                    fix_rx,
                }
            });
    }

    async fn did_close(
        &self,
        DidCloseTextDocumentParams {
            text_document: TextDocumentIdentifier { uri },
        }: DidCloseTextDocumentParams,
    ) {
        self.watchers.write().await.remove(&uri);
        self.client.publish_diagnostics(uri, vec![], None).await;
    }

    async fn did_change(
        &self,
        DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier { uri, .. },
            content_changes,
        }: DidChangeTextDocumentParams,
    ) {
        if let Some(change) = content_changes.first()
            && let Some(watcher) = self.watchers.read().await.get(&uri)
        {
            watcher.content_tx.send(change.text.clone()).unwrap();
        }
    }

    async fn did_save(
        &self,
        DidSaveTextDocumentParams {
            text_document: TextDocumentIdentifier { uri },
            text,
        }: DidSaveTextDocumentParams,
    ) {
        if let Some(text) = text
            && let Some(watcher) = self.watchers.read().await.get(&uri)
        {
            watcher.content_tx.send(text).unwrap();
        }
    }
}
