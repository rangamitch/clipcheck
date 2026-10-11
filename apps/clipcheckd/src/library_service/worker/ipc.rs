//! Library commands answered by the worker: `list_libraries`, `scan_libraries` and
//! `list_game_names`.

use std::collections::HashMap;

use clipcheck_ipc::proto::{Command, ErrorCode, GameNameDto, LibraryDto, Reply};
use clipcheck_library::{ListQuery, RecStatus, RootInfo};

use super::Worker;
use crate::library_service::error;

impl Worker {
    /// Answers a library command.
    pub(super) fn ipc(&mut self, command: Command) -> Reply {
        match command {
            Command::ListLibraries => match self.lib.list_roots_with_stats() {
                Ok(rows) => Reply::Libraries {
                    libraries: rows.iter().map(|row| self.dto(row)).collect(),
                },
                Err(e) => error(ErrorCode::Internal, e.to_string(), false),
            },
            Command::ScanLibraries { root_id } => self.ipc_scan(root_id),
            Command::ListGameNames => self.ipc_games(),
            _ => error(
                ErrorCode::NotImplemented,
                "not implemented in this build",
                false,
            ),
        }
    }

    fn dto(&self, row: &RootInfo) -> LibraryDto {
        LibraryDto {
            id: row.id,
            path: row.path.clone(),
            label: row.label.clone(),
            primary: row.is_primary,
            read_only: row.read_only,
            online: row.availability == "online",
            clip_count: row.clip_count,
            session_count: row.session_count,
            total_bytes: row.total_bytes,
            last_scan_unix_ms: row.last_scan_unix_ms,
            scanning: self.is_scanning(row.id),
        }
    }

    /// `scan_libraries`: queues one root, or every root when no id is given.
    fn ipc_scan(&mut self, root_id: Option<i64>) -> Reply {
        let Some(id) = root_id else {
            self.enqueue_all();
            return Reply::Ok;
        };
        match self.lib.list_roots_with_stats() {
            Ok(rows) if rows.iter().any(|row| row.id == id) => {
                self.enqueue(id);
                Reply::Ok
            }
            Ok(_) => error(
                ErrorCode::BadRequest,
                format!("no library with id {id}"),
                false,
            ),
            Err(e) => error(ErrorCode::Internal, e.to_string(), false),
        }
    }

    /// `list_game_names`: game display names with their clip counts, most clips first.
    fn ipc_games(&self) -> Reply {
        let mut counts: HashMap<String, u64> = HashMap::new();
        let mut cursor: Option<String> = None;
        loop {
            let query = ListQuery {
                limit: 200,
                cursor: cursor.clone(),
                ..ListQuery::default()
            };
            let page = match self.lib.list(&query) {
                Ok(page) => page,
                Err(e) => return error(ErrorCode::Internal, e.to_string(), false),
            };
            for row in &page.items {
                if row.game_id.is_none() || row.status == RecStatus::Missing {
                    continue;
                }
                if let Some(name) = &row.game_name {
                    *counts.entry(name.clone()).or_insert(0) += 1;
                }
            }
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        let mut games: Vec<GameNameDto> = counts
            .into_iter()
            .map(|(name, clip_count)| GameNameDto { name, clip_count })
            .collect();
        games.sort_by(|a, b| {
            b.clip_count
                .cmp(&a.clip_count)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Reply::GameNames { games }
    }
}
