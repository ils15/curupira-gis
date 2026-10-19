//! Fila de processos: **tasks**, **processos por célula** e **log por célula**.
//!
//! Uma [`Task`] é a unidade de trabalho enfileirada pelo operador (hoje só
//! [`TaskKind::Classify`]); cada [`Process`] é o trabalho de UMA célula dentro
//! da task. O [`CellLogEntry`] é o rastro de execução por célula (info/warn/
//! error) produzido pelo executor.
//!
//! Os enums de estado são tipados com `FromStr`/`as_str` para o round-trip com
//! o texto persistido (mesmo padrão de [`crate::classification::DraftState`]).
//! Nomes de tabela próprios do clean-room (Decisão 2 do plano).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;

use crate::error::Error;

/// Tipo de trabalho da task. Extensível por design; nesta fase só existe
/// classificação (`classify`) — nenhum outro kind é inventado (YAGNI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    /// Classificação por regras (`ruleset`) sobre as medidas das células.
    Classify,
}

impl TaskKind {
    /// Texto persistido em `tasks.kind`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Classify => "classify",
        }
    }
}

impl fmt::Display for TaskKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TaskKind {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "classify" => Ok(Self::Classify),
            other => Err(Error::InvalidTaskKind(other.to_owned())),
        }
    }
}

/// Estado da task — derivado dos processos (simples e observável).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Enfileirada, com trabalho pendente.
    Pending,
    /// Um executor está rodando (entre lotes).
    Running,
    /// Todos os processos concluídos.
    Done,
    /// Cancelada cooperativamente (entre lotes); processos pendentes ficam
    /// pendentes, mas nunca mais são reclamados.
    Cancelled,
    /// Terminou com erros (nenhum trabalho restante; `task retry` reabre).
    Failed,
}

impl TaskStatus {
    /// Texto persistido em `tasks.status`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TaskStatus {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "done" => Ok(Self::Done),
            "cancelled" => Ok(Self::Cancelled),
            "failed" => Ok(Self::Failed),
            other => Err(Error::InvalidTaskStatus(other.to_owned())),
        }
    }
}

/// Estado do processo de UMA célula.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessStatus {
    /// Na fila, ainda não reclamado por worker.
    Pending,
    /// Reclamado (`FOR UPDATE SKIP LOCKED`) e em execução.
    Running,
    /// Concluído com sucesso.
    Done,
    /// Falhou (`last_error` tem o motivo); reprocessa só com `task retry`.
    Error,
}

impl ProcessStatus {
    /// Texto persistido em `processes.status`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Done => "done",
            Self::Error => "error",
        }
    }
}

impl fmt::Display for ProcessStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ProcessStatus {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "done" => Ok(Self::Done),
            "error" => Ok(Self::Error),
            other => Err(Error::InvalidProcessStatus(other.to_owned())),
        }
    }
}

/// Nível de uma entrada do log por célula.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    /// Informação (decisão tomada).
    Info,
    /// Aviso (ex.: célula sem classe decidida).
    Warn,
    /// Erro de execução do processo.
    Error,
}

impl LogLevel {
    /// Texto persistido em `cell_log.level`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for LogLevel {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "info" => Ok(Self::Info),
            "warn" => Ok(Self::Warn),
            "error" => Ok(Self::Error),
            other => Err(Error::InvalidLogLevel(other.to_owned())),
        }
    }
}

/// Task da fila de processos.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    /// Identificador (UUID textual na persistência; vazio = não persistido).
    pub id: String,
    /// Projeto dono da task.
    pub project_id: String,
    /// Tipo de trabalho.
    pub kind: TaskKind,
    /// Nome de exibição (livre; não é chave).
    pub name: String,
    /// Parâmetros livres (jsonb; `Value::Null` é normalizado para `{}`).
    #[serde(default)]
    pub params: Value,
    /// Camada de células alvo; `None` = desvinculada (objeto apagado).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cell_layer_id: Option<String>,
    /// Ruleset da classificação; `None` = desvinculado.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ruleset_id: Option<String>,
    /// Rascunho que recebe o resultado; `None` = desvinculado.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_layer_id: Option<String>,
    /// Estado da task.
    pub status: TaskStatus,
    #[serde(default = "OffsetDateTime::now_utc", with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(default = "OffsetDateTime::now_utc", with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl Task {
    /// Cria uma task ainda não persistida (status `pending`, `{}` em params).
    #[must_use]
    pub fn new(project_id: &str, kind: TaskKind, name: &str) -> Self {
        let now = OffsetDateTime::now_utc();
        Self {
            id: String::new(),
            project_id: project_id.to_owned(),
            kind,
            name: name.to_owned(),
            params: serde_json::json!({}),
            cell_layer_id: None,
            ruleset_id: None,
            draft_layer_id: None,
            status: TaskStatus::Pending,
            created_at: now,
            updated_at: now,
        }
    }
}

/// Processo de UMA célula dentro de uma task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Process {
    /// Identificador (UUID textual na persistência; vazio = não persistido).
    pub id: String,
    /// Task dona.
    pub task_id: String,
    /// Camada de células (redundante com a task para a FK composta de célula).
    pub cell_layer_id: String,
    /// Identificador determinístico da célula (`"<col>_<row>"`).
    pub cell_id: String,
    /// Estado do processo.
    pub status: ProcessStatus,
    /// Quantas vezes o processo terminou (done ou error).
    pub attempts: i32,
    /// Motivo do último erro, se houve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default = "OffsetDateTime::now_utc", with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(default = "OffsetDateTime::now_utc", with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// Entrada do log por célula (`cell_log`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellLogEntry {
    /// Task dona do log.
    pub task_id: String,
    /// Camada de células da entrada.
    pub cell_layer_id: String,
    /// Célula da entrada.
    pub cell_id: String,
    /// Nível da entrada.
    pub level: LogLevel,
    /// Mensagem livre gerada pelo executor.
    pub message: String,
    /// Momento da gravação (`cell_log.created_on`).
    #[serde(default = "OffsetDateTime::now_utc", with = "time::serde::rfc3339")]
    pub created_on: OffsetDateTime,
}

impl CellLogEntry {
    /// Cria uma entrada com o momento atual.
    #[must_use]
    pub fn new(
        task_id: &str,
        cell_layer_id: &str,
        cell_id: &str,
        level: LogLevel,
        message: String,
    ) -> Self {
        Self {
            task_id: task_id.to_owned(),
            cell_layer_id: cell_layer_id.to_owned(),
            cell_id: cell_id.to_owned(),
            level,
            message,
            created_on: OffsetDateTime::now_utc(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enums_de_task_fazem_round_trip_pelo_texto_persistido() {
        assert_eq!(TaskKind::Classify.as_str(), "classify");
        assert_eq!(
            "classify".parse::<TaskKind>().expect("kind válido"),
            TaskKind::Classify
        );
        for (status, text) in [
            (TaskStatus::Pending, "pending"),
            (TaskStatus::Running, "running"),
            (TaskStatus::Done, "done"),
            (TaskStatus::Cancelled, "cancelled"),
            (TaskStatus::Failed, "failed"),
        ] {
            assert_eq!(status.as_str(), text);
            assert_eq!(text.parse::<TaskStatus>().expect("status válido"), status);
        }
        for (status, text) in [
            (ProcessStatus::Pending, "pending"),
            (ProcessStatus::Running, "running"),
            (ProcessStatus::Done, "done"),
            (ProcessStatus::Error, "error"),
        ] {
            assert_eq!(status.as_str(), text);
            assert_eq!(
                text.parse::<ProcessStatus>().expect("status válido"),
                status
            );
        }
        for (level, text) in [
            (LogLevel::Info, "info"),
            (LogLevel::Warn, "warn"),
            (LogLevel::Error, "error"),
        ] {
            assert_eq!(level.as_str(), text);
            assert_eq!(text.parse::<LogLevel>().expect("nível válido"), level);
        }
    }

    #[test]
    fn textos_desconhecidos_sao_erro_tipado() {
        assert!("export".parse::<TaskKind>().is_err());
        assert!("queued".parse::<TaskStatus>().is_err());
        assert!("zombie".parse::<ProcessStatus>().is_err());
        assert!("fatal".parse::<LogLevel>().is_err());
    }

    #[test]
    fn task_nova_classify_pendente_e_parametros_vazios() {
        let task = Task::new("p1", TaskKind::Classify, "classificar");
        assert_eq!(task.status, TaskStatus::Pending);
        assert_eq!(task.kind, TaskKind::Classify);
        assert_eq!(task.params, serde_json::json!({}));
        assert_eq!(task.cell_layer_id, None);
    }
}
